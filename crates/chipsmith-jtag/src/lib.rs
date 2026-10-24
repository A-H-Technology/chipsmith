//! JTAG: the TAP state machine, the `Cable` that clocks it, and an SVF
//! player on top. A bitstream writer emits SVF (`cram svf`), so loading a
//! device is "play this SVF", whatever the vendor.

pub mod ftdi;
pub mod svf;
pub mod usb_blaster;
pub mod xc7;

use anyhow::Result;

/// The 16 TAP controller states (IEEE 1149.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TapState {
    Reset,
    Idle,
    DrSelect,
    DrCapture,
    DrShift,
    DrExit1,
    DrPause,
    DrExit2,
    DrUpdate,
    IrSelect,
    IrCapture,
    IrShift,
    IrExit1,
    IrPause,
    IrExit2,
    IrUpdate,
}

impl TapState {
    pub const ALL: [TapState; 16] = [
        TapState::Reset,
        TapState::Idle,
        TapState::DrSelect,
        TapState::DrCapture,
        TapState::DrShift,
        TapState::DrExit1,
        TapState::DrPause,
        TapState::DrExit2,
        TapState::DrUpdate,
        TapState::IrSelect,
        TapState::IrCapture,
        TapState::IrShift,
        TapState::IrExit1,
        TapState::IrPause,
        TapState::IrExit2,
        TapState::IrUpdate,
    ];

    pub fn next(self, tms: bool) -> TapState {
        use TapState::*;
        match (self, tms) {
            (Reset, false) => Idle,
            (Reset, true) => Reset,
            (Idle, false) => Idle,
            (Idle, true) => DrSelect,
            (DrSelect, false) => DrCapture,
            (DrSelect, true) => IrSelect,
            (DrCapture, false) => DrShift,
            (DrCapture, true) => DrExit1,
            (DrShift, false) => DrShift,
            (DrShift, true) => DrExit1,
            (DrExit1, false) => DrPause,
            (DrExit1, true) => DrUpdate,
            (DrPause, false) => DrPause,
            (DrPause, true) => DrExit2,
            (DrExit2, false) => DrShift,
            (DrExit2, true) => DrUpdate,
            (DrUpdate, false) => Idle,
            (DrUpdate, true) => DrSelect,
            (IrSelect, false) => IrCapture,
            (IrSelect, true) => Reset,
            (IrCapture, false) => IrShift,
            (IrCapture, true) => IrExit1,
            (IrShift, false) => IrShift,
            (IrShift, true) => IrExit1,
            (IrExit1, false) => IrPause,
            (IrExit1, true) => IrUpdate,
            (IrPause, false) => IrPause,
            (IrPause, true) => IrExit2,
            (IrExit2, false) => IrShift,
            (IrExit2, true) => IrUpdate,
            (IrUpdate, false) => Idle,
            (IrUpdate, true) => DrSelect,
        }
    }

    /// Shortest TMS sequence from `self` to `to`. Reset is special: from an
    /// unknown state only five ones are sure to get there, so asking for it
    /// always gives those.
    pub fn path_to(self, to: TapState) -> Vec<bool> {
        if to == TapState::Reset {
            return vec![true; 5];
        }
        let mut paths: Vec<(TapState, Vec<bool>)> = vec![(self, Vec::new())];
        let mut seen = vec![self];
        let mut i = 0;
        while paths[i].0 != to {
            let (state, path) = paths[i].clone();
            for tms in [false, true] {
                let next = state.next(tms);
                if !seen.contains(&next) {
                    seen.push(next);
                    let mut p = path.clone();
                    p.push(tms);
                    paths.push((next, p));
                }
            }
            i += 1;
        }
        paths.swap_remove(i).1
    }
}

/// Something that clocks a TAP. Bit vectors are LSB first: bit `i` is
/// `bytes[i / 8] >> (i % 8) & 1`, and bit 0 is shifted first.
pub trait Cable {
    /// One TCK per entry, with TMS at that value and TDI low.
    fn tms(&mut self, tms: &[bool]) -> Result<()>;

    /// Shift `bits` bits of `tdi` with TMS low, except the last bit when
    /// `exit` (which leaves Shift-DR/IR for Exit1). Returns TDO when `capture`.
    fn shift(
        &mut self,
        tdi: &[u8],
        bits: usize,
        exit: bool,
        capture: bool,
    ) -> Result<Option<Vec<u8>>>;

    /// `clocks` TCKs with TMS and TDI low: waiting in Run-Test/Idle or a pause state.
    fn idle(&mut self, clocks: u32) -> Result<()>;

    /// Send whatever is buffered.
    fn flush(&mut self) -> Result<()>;
}

pub fn bit(bytes: &[u8], i: usize) -> bool {
    bytes[i / 8] >> (i % 8) & 1 == 1
}

#[cfg(test)]
pub mod sim {
    //! A cable that is also a TAP, for tests: it records what each state saw.

    use super::*;

    #[derive(Debug, Default)]
    pub struct SimCable {
        pub state: Option<TapState>,
        /// Every completed IR and DR shift: (is IR, bits shifted in).
        pub shifts: Vec<(bool, Vec<bool>)>,
        current: Vec<bool>,
        /// Returned as TDO for every captured DR shift.
        pub dr_tdo: Vec<u8>,
        pub idle_clocks: u32,
    }

    impl SimCable {
        pub fn new() -> Self {
            SimCable {
                state: None,
                ..Default::default()
            }
        }

        fn clock(&mut self, tms: bool, tdi: bool) {
            let state = self.state.unwrap_or(TapState::Reset);
            if matches!(state, TapState::DrShift | TapState::IrShift) {
                self.current.push(tdi);
            }
            let next = if self.state.is_none() && !tms {
                // Unknown state: only TMS high counts until we reach Reset.
                None
            } else {
                Some(state.next(tms))
            };
            if matches!(state, TapState::DrShift | TapState::IrShift)
                && next.is_some_and(|n| n != state)
            {
                self.shifts.push((
                    state == TapState::IrShift,
                    std::mem::take(&mut self.current),
                ));
            }
            self.state = next;
        }
    }

    impl Cable for SimCable {
        fn tms(&mut self, tms: &[bool]) -> Result<()> {
            for &t in tms {
                self.clock(t, false);
            }
            Ok(())
        }

        fn shift(
            &mut self,
            tdi: &[u8],
            bits: usize,
            exit: bool,
            capture: bool,
        ) -> Result<Option<Vec<u8>>> {
            let ir = self.state == Some(TapState::IrShift);
            for i in 0..bits {
                self.clock(exit && i == bits - 1, bit(tdi, i));
            }
            Ok(capture.then(|| {
                if ir {
                    vec![0; bits.div_ceil(8)]
                } else {
                    let mut tdo = self.dr_tdo.clone();
                    tdo.resize(bits.div_ceil(8), 0);
                    tdo
                }
            }))
        }

        fn idle(&mut self, clocks: u32) -> Result<()> {
            self.idle_clocks += clocks;
            for _ in 0..clocks {
                self.clock(false, false);
            }
            Ok(())
        }

        fn flush(&mut self) -> Result<()> {
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths_are_the_standard_ones() {
        use TapState::*;
        assert_eq!(Idle.path_to(DrShift), [true, false, false]);
        assert_eq!(Idle.path_to(IrShift), [true, true, false, false]);
        assert_eq!(IrExit1.path_to(IrPause), [false]);
        assert_eq!(IrPause.path_to(Idle), [true, true, false]);
        assert_eq!(DrExit1.path_to(Idle), [true, false]);
        assert_eq!(Idle.path_to(Idle), Vec::<bool>::new());
        assert_eq!(Idle.path_to(Reset), [true; 5]);
    }

    #[test]
    fn every_state_reaches_every_other() {
        for from in TapState::ALL {
            for to in TapState::ALL {
                let end = from.path_to(to).into_iter().fold(from, TapState::next);
                assert_eq!(end, to, "{from:?} -> {to:?}");
            }
        }
    }
}
