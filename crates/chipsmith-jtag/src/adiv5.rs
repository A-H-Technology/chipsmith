//! ARM Debug Interface v5 over JTAG (IHI0031): the JTAG-DP on the Zynq's
//! chain and a MEM-AP behind it, so the host can read and write the
//! processor side's address space (the SLCR, the QSPI controller, OCM)
//! with no code running on the ARM.
//!
//! Every DPACC/APACC scan is 35 bits: in, RnW then A[3:2] then 32 data
//! bits; out, a 3-bit acknowledge then the data of the previous read. A
//! read's result only comes out on the next scan, so a lone read is
//! followed by a read of RDBUFF.

use anyhow::{bail, ensure, Context, Result};
use std::time::{Duration, Instant};

use super::xc7::Tap;
use super::{bit, Cable, TapState};

/// JTAG-DP instructions (IHI0031, "JTAG-DP register descriptions").
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Ir {
    Dpacc = 0xa,
    Apacc = 0xb,
}

const IR_LEN: usize = 4;
const ACCESS_BITS: usize = 35;

/// Debug port registers, by A[3:2].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DpReg {
    CtrlStat = 0x4,
    Select = 0x8,
    RdBuff = 0xc,
}

/// Access port registers: bank in bits 7:4, A[3:2] in 3:2.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApReg {
    Csw = 0x00,
    Tar = 0x04,
    Drw = 0x0c,
    Idr = 0xfc,
}

impl ApReg {
    fn bank(self) -> u8 {
        self as u8 >> 4
    }
    fn a32(self) -> u8 {
        self as u8 & 0xc
    }
}

const CSYSPWRUPACK: u32 = 1 << 31;
const CSYSPWRUPREQ: u32 = 1 << 30;
const CDBGPWRUPACK: u32 = 1 << 29;
const CDBGPWRUPREQ: u32 = 1 << 28;
const STICKYERR: u32 = 1 << 5;

/// A WAIT means the DP is still busy with the last access and ignored this
/// one; AHB accesses to the Zynq's registers finish in a few TCKs, so a
/// handful of retries is plenty, and more means something is stuck.
const WAIT_RETRIES: usize = 32;
/// TCKs in Run-Test/Idle after each access, which gives the AP time to
/// finish before the next scan instead of answering WAIT.
const IDLE_CLOCKS: u32 = 8;

/// One JTAG-DP on a chain whose other TAPs sit in BYPASS.
pub struct Dap<'c, C: Cable> {
    cable: &'c mut C,
    ir_total: usize,
    /// IR bit where the DAP's instruction starts (TAPs nearer TDO first).
    ir_offset: usize,
    /// BYPASS bits in front of (nearer TDO) and behind the DAP's DR.
    pad_tdo: usize,
    pad_tdi: usize,
    ir: Option<Ir>,
    /// What SELECT holds, once written: (APSEL, APBANKSEL).
    select: Option<(u8, u8)>,
}

impl<'c, C: Cable> Dap<'c, C> {
    /// The DAP is `chain[target]`; power up its debug and system domains.
    pub fn new(cable: &'c mut C, chain: &[Tap], target: usize) -> Result<Self> {
        ensure!(
            chain.get(target).is_some_and(|t| t.ir_len == IR_LEN),
            "TAP {target} isn't an ARM DAP"
        );
        let mut dap = Dap {
            cable,
            ir_total: chain.iter().map(|t| t.ir_len).sum(),
            ir_offset: chain[..target].iter().map(|t| t.ir_len).sum(),
            pad_tdo: target,
            pad_tdi: chain.len() - target - 1,
            ir: None,
            select: None,
        };
        dap.cable.tms(&TapState::Reset.path_to(TapState::Reset))?;
        dap.cable.tms(&TapState::Reset.path_to(TapState::Idle))?;
        dap.power_up()?;
        Ok(dap)
    }

    fn power_up(&mut self) -> Result<()> {
        self.dp_write(DpReg::CtrlStat, CSYSPWRUPREQ | CDBGPWRUPREQ)?;
        let deadline = Instant::now() + Duration::from_millis(200);
        loop {
            let stat = self.dp_read(DpReg::CtrlStat)?;
            if stat & (CSYSPWRUPACK | CDBGPWRUPACK) == CSYSPWRUPACK | CDBGPWRUPACK {
                return Ok(());
            }
            ensure!(
                Instant::now() < deadline,
                "debug power-up not acknowledged (CTRL/STAT {stat:#010x})"
            );
        }
    }

    fn set_ir(&mut self, ir: Ir) -> Result<()> {
        if self.ir == Some(ir) {
            return Ok(());
        }
        let bits = bits_of(self.ir_total, |i| {
            if (self.ir_offset..self.ir_offset + IR_LEN).contains(&i) {
                (ir as u8) >> (i - self.ir_offset) & 1 == 1
            } else {
                true // BYPASS
            }
        });
        self.cable.tms(&TapState::Idle.path_to(TapState::IrShift))?;
        self.cable.shift(&bits, self.ir_total, true, false)?;
        self.cable.tms(&TapState::IrExit1.path_to(TapState::Idle))?;
        self.ir = Some(ir);
        Ok(())
    }

    /// One DPACC or APACC access; returns the data the previous read left.
    fn access(&mut self, ir: Ir, read: bool, a32: u8, data: u32) -> Result<u32> {
        self.set_ir(ir)?;
        let len = self.pad_tdo + ACCESS_BITS + self.pad_tdi;
        let at = self.pad_tdo;
        let word = read as u64 | ((a32 as u64 >> 2) << 1) | (data as u64) << 3;
        let tdi = bits_of(len, |i| {
            (at..at + ACCESS_BITS).contains(&i) && word >> (i - at) & 1 == 1
        });
        for _ in 0..WAIT_RETRIES {
            self.cable.tms(&TapState::Idle.path_to(TapState::DrShift))?;
            let tdo = self.cable.shift(&tdi, len, true, true)?.context("no TDO")?;
            self.cable.tms(&TapState::DrExit1.path_to(TapState::Idle))?;
            self.cable.idle(IDLE_CLOCKS)?;
            let ack = (0..3).fold(0u8, |acc, k| acc | (bit(&tdo, at + k) as u8) << k);
            match ack {
                // OK/FAULT: a fault shows up later as STICKYERR.
                0b010 => {
                    return Ok(
                        (0..32).fold(0u32, |acc, k| acc | (bit(&tdo, at + 3 + k) as u32) << k)
                    )
                }
                0b001 => continue, // WAIT: not taken, so the same access again.
                _ => bail!("DAP answered {ack:#05b} to {ir:?}; not a DAP, or the chain is wrong"),
            }
        }
        bail!("DAP kept answering WAIT to {ir:?} {a32:#x}")
    }

    pub fn dp_read(&mut self, reg: DpReg) -> Result<u32> {
        self.access(Ir::Dpacc, true, reg as u8, 0)?;
        self.access(Ir::Dpacc, true, DpReg::RdBuff as u8, 0)
    }

    pub fn dp_write(&mut self, reg: DpReg, value: u32) -> Result<()> {
        if reg == DpReg::Select {
            self.select = None;
        }
        self.access(Ir::Dpacc, false, reg as u8, value).map(drop)
    }

    fn select(&mut self, ap: u8, bank: u8) -> Result<()> {
        if self.select != Some((ap, bank)) {
            self.dp_write(DpReg::Select, (ap as u32) << 24 | (bank as u32) << 4)?;
            self.select = Some((ap, bank));
        }
        Ok(())
    }

    pub fn ap_read(&mut self, ap: u8, reg: ApReg) -> Result<u32> {
        self.select(ap, reg.bank())?;
        self.access(Ir::Apacc, true, reg.a32(), 0)?;
        self.access(Ir::Dpacc, true, DpReg::RdBuff as u8, 0)
    }

    /// AP accesses in one batch of scans: `(reg, Some(value))` writes,
    /// `(reg, None)` reads. A read's data comes out of the next scan, the
    /// last one's out of RDBUFF; reads come back in order. `None` if any
    /// scan got WAIT: that scan wasn't taken but later ones may have been,
    /// so only a caller whose accesses can be repeated may retry.
    fn ap_batch(
        &mut self,
        ap: u8,
        ops: &[(ApReg, Option<u32>)],
        idle: u32,
    ) -> Result<Option<Vec<u32>>> {
        let Some(&(first, _)) = ops.first() else {
            return Ok(Some(Vec::new()));
        };
        ensure!(
            ops.iter().all(|(r, _)| r.bank() == first.bank()),
            "a batch of AP accesses across register banks"
        );
        self.select(ap, first.bank())?;
        self.set_ir(Ir::Apacc)?;
        let len = self.pad_tdo + ACCESS_BITS + self.pad_tdi;
        let at = self.pad_tdo;
        let scans: Vec<Vec<u8>> = ops
            .iter()
            .map(|&(reg, value)| {
                let word = value.is_none() as u64
                    | ((reg.a32() as u64 >> 2) << 1)
                    | (value.unwrap_or(0) as u64) << 3;
                bits_of(len, |i| {
                    (at..at + ACCESS_BITS).contains(&i) && word >> (i - at) & 1 == 1
                })
            })
            .collect();
        let tdos = self.cable.dr_scans(&scans, len, idle)?;
        let mut data = Vec::with_capacity(ops.len());
        for tdo in &tdos {
            match (0..3).fold(0u8, |acc, k| acc | (bit(tdo, at + k) as u8) << k) {
                0b010 => {
                    data.push((0..32).fold(0u32, |acc, k| acc | (bit(tdo, at + 3 + k) as u32) << k))
                }
                0b001 => return Ok(None),
                ack => bail!("DAP answered {ack:#05b} in a batch of AP accesses"),
            }
        }
        let mut reads = Vec::new();
        for (k, (_, value)) in ops.iter().enumerate() {
            if value.is_none() {
                reads.push(match data.get(k + 1) {
                    Some(&d) => d,
                    None => self.access(Ir::Dpacc, true, DpReg::RdBuff as u8, 0)?,
                });
            }
        }
        Ok(Some(reads))
    }

    pub fn ap_write(&mut self, ap: u8, reg: ApReg, value: u32) -> Result<()> {
        self.select(ap, reg.bank())?;
        self.access(Ir::Apacc, false, reg.a32(), value).map(drop)
    }

    /// Fail on a sticky error from any access since the last check, and
    /// clear it so the DAP takes accesses again.
    pub fn check(&mut self) -> Result<()> {
        if let Some(stat) = self.take_fault()? {
            bail!("bus fault behind the DAP (CTRL/STAT {stat:#010x})");
        }
        Ok(())
    }

    /// CTRL/STAT if an access since the last check faulted; the sticky
    /// error is cleared either way.
    fn take_fault(&mut self) -> Result<Option<u32>> {
        let stat = self.dp_read(DpReg::CtrlStat)?;
        if stat & STICKYERR == 0 {
            return Ok(None);
        }
        self.dp_write(DpReg::CtrlStat, CSYSPWRUPREQ | CDBGPWRUPREQ | STICKYERR)?;
        Ok(Some(stat))
    }
}

/// MEM-AP CSW (IHI0031 C2.6.4): DbgSwEnable, a debug master's privileged
/// data accesses (HPROT[1], AHB master type debug, as OpenOCD's
/// CSW_AHB_DEFAULT), the address incremented by the access size, 32 bits.
const CSW_WORD: u32 = 1 << 31 | 1 << 29 | 1 << 25 | 0b01 << 4 | 0b010;

/// A memory access port (the Zynq's AHB-AP is AP 0) as 32-bit words.
pub struct MemAp<'d, 'c, C: Cable> {
    dap: &'d mut Dap<'c, C>,
    ap: u8,
    /// Whether CSW has TAR step after each DRW access.
    increment: bool,
}

/// One word access in a `MemAp::run` batch.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Access {
    Write(u32, u32),
    Read(u32),
}

impl Access {
    fn addr(self) -> u32 {
        match self {
            Access::Write(a, _) | Access::Read(a) => a,
        }
    }
}

impl<'d, 'c, C: Cable> MemAp<'d, 'c, C> {
    pub fn new(dap: &'d mut Dap<'c, C>, ap: u8) -> Result<Self> {
        dap.ap_write(ap, ApReg::Csw, CSW_WORD)?;
        Ok(MemAp {
            dap,
            ap,
            increment: true,
        })
    }

    fn set_increment(&mut self, on: bool) -> Result<()> {
        if self.increment != on {
            let csw = if on {
                CSW_WORD
            } else {
                CSW_WORD & !(0b11 << 4)
            };
            self.dap.ap_write(self.ap, ApReg::Csw, csw)?;
            self.increment = on;
        }
        Ok(())
    }

    /// Word accesses at fixed addresses in one batch, in order (TAR doesn't
    /// step, so a FIFO register can take many in a row); returns the reads.
    /// A WAIT anywhere fails the batch rather than retrying it, since a
    /// FIFO write that was taken can't be undone: `idle` must give the
    /// slave time to finish each access.
    pub fn run(&mut self, accesses: &[Access], idle: u32) -> Result<Vec<u32>> {
        self.set_increment(false)?;
        let mut ops = Vec::with_capacity(accesses.len() * 2);
        let mut tar = None;
        for &a in accesses {
            ensure!(
                a.addr().is_multiple_of(4),
                "unaligned access at {:#010x}",
                a.addr()
            );
            if tar != Some(a.addr()) {
                ops.push((ApReg::Tar, Some(a.addr())));
                tar = Some(a.addr());
            }
            ops.push((
                ApReg::Drw,
                match a {
                    Access::Write(_, v) => Some(v),
                    Access::Read(_) => None,
                },
            ));
        }
        let reads = self
            .dap
            .ap_batch(self.ap, &ops, idle)?
            .context("the AP answered WAIT inside a batch; some accesses may have been taken")?;
        self.dap.check().context("a batch of accesses")?;
        Ok(reads)
    }

    pub fn idr(&mut self) -> Result<u32> {
        self.dap.ap_read(self.ap, ApReg::Idr)
    }

    pub fn read32(&mut self, addr: u32) -> Result<u32> {
        self.try_read32(addr)?
            .with_context(|| format!("bus fault reading {addr:#010x}"))
    }

    /// A read the slave may refuse: `None` when it answers with a bus
    /// error (an empty FIFO, say) instead of failing.
    pub fn try_read32(&mut self, addr: u32) -> Result<Option<u32>> {
        ensure!(addr.is_multiple_of(4), "unaligned read at {addr:#010x}");
        self.dap.ap_write(self.ap, ApReg::Tar, addr)?;
        let v = self.dap.ap_read(self.ap, ApReg::Drw)?;
        Ok(self.dap.take_fault()?.is_none().then_some(v))
    }

    /// `words` 32-bit words from `addr` on, batched: TAR is only sure to
    /// auto-increment within a 1 KiB block (IHI0031 C2.2.2), so each block
    /// is one TAR write and one batch of DRW reads.
    pub fn read_block(&mut self, addr: u32, words: usize) -> Result<Vec<u32>> {
        ensure!(addr.is_multiple_of(4), "unaligned read at {addr:#010x}");
        let mut out = Vec::with_capacity(words);
        let mut idle = IDLE_CLOCKS;
        while out.len() < words {
            let at = addr + 4 * out.len() as u32;
            let n = (((at | 0x3ff) + 1 - at) / 4).min((words - out.len()) as u32) as usize;
            self.set_increment(true)?;
            self.dap.ap_write(self.ap, ApReg::Tar, at)?;
            match self
                .dap
                .ap_batch(self.ap, &vec![(ApReg::Drw, None); n], idle)?
            {
                Some(block) => {
                    self.dap
                        .check()
                        .with_context(|| format!("reading {n} words at {at:#010x}"))?;
                    out.extend(block);
                }
                None => {
                    // The slave is slower than our scans: give it more time and
                    // take the block again (TAR is rewritten, so reads restart).
                    ensure!(
                        idle < 4096,
                        "the AP still answers WAIT with {idle} TCKs between reads"
                    );
                    idle *= 2;
                }
            }
        }
        Ok(out)
    }

    pub fn write32(&mut self, addr: u32, value: u32) -> Result<()> {
        ensure!(addr.is_multiple_of(4), "unaligned write at {addr:#010x}");
        self.dap.ap_write(self.ap, ApReg::Tar, addr)?;
        self.dap.ap_write(self.ap, ApReg::Drw, value)?;
        self.dap
            .check()
            .with_context(|| format!("writing {addr:#010x}"))
    }
}

fn bits_of(len: usize, value: impl Fn(usize) -> bool) -> Vec<u8> {
    let mut bytes = vec![0u8; len.div_ceil(8)];
    for i in (0..len).filter(|&i| value(i)) {
        bytes[i / 8] |= 1 << (i % 8);
    }
    bytes
}

#[cfg(test)]
mod tests {
    //! Against a model of a JTAG-DP and an AHB-AP behind it, on the Zybo's
    //! chain: the xc7z010 nearest TDO, the DAP behind it.
    use super::*;
    use std::collections::HashMap;

    const XC7Z010: Tap = Tap {
        idcode: 0x0372_2093,
        ir_len: 6,
    };
    const DAP: Tap = Tap {
        idcode: 0x4ba0_0477,
        ir_len: 4,
    };

    #[derive(Default)]
    struct Model {
        state: Option<TapState>,
        ir: Vec<bool>,
        dr: Vec<bool>,
        /// The DAP's instruction, once an IR update has set it.
        dap_ir: u8,
        /// The data a read leaves for the next scan.
        last_read: u32,
        ctrl_stat: u32,
        select: u32,
        csw: u32,
        tar: u32,
        mem: HashMap<u32, u32>,
        /// Scans to answer WAIT to before taking one.
        waits: usize,
        /// Addresses that fault.
        faults: Vec<u32>,
    }

    impl Model {
        const IR_TOTAL: usize = 10;
        fn dr_len(&self) -> usize {
            // xc7z010 BYPASS (nearest TDO), then the DAP's DR.
            1 + if matches!(self.dap_ir, 0xa | 0xb) {
                ACCESS_BITS
            } else {
                1
            }
        }

        fn clock(&mut self, tms: bool, tdi: bool) -> bool {
            let state = self.state.unwrap_or(TapState::Reset);
            let mut tdo = false;
            match state {
                TapState::IrShift => {
                    tdo = self.ir.remove(0);
                    self.ir.push(tdi);
                }
                TapState::DrShift => {
                    tdo = self.dr.remove(0);
                    self.dr.push(tdi);
                }
                _ => {}
            }
            let next = if self.state.is_none() && !tms {
                None
            } else {
                Some(state.next(tms))
            };
            match next {
                Some(TapState::Reset) => self.dap_ir = 0xf,
                Some(TapState::IrCapture) => {
                    self.ir = vec![false; Self::IR_TOTAL];
                    self.ir[0] = true; // 7-series IR capture ...01
                    self.ir[6] = true; // DAP IR capture 0001
                }
                Some(TapState::IrUpdate) => {
                    self.dap_ir = (0..4).fold(0, |a, k| a | (self.ir[6 + k] as u8) << k);
                }
                Some(TapState::DrCapture) => {
                    let n = self.dr_len();
                    self.dr = vec![false; n];
                    if n > 2 {
                        let ack: u8 = if self.waits > 0 { 0b001 } else { 0b010 };
                        for k in 0..3 {
                            self.dr[1 + k] = ack >> k & 1 == 1;
                        }
                        for k in 0..32 {
                            self.dr[4 + k] = self.last_read >> k & 1 == 1;
                        }
                    }
                }
                Some(TapState::DrUpdate) if self.dr_len() > 2 => {
                    if self.waits > 0 {
                        self.waits -= 1;
                    } else {
                        let w =
                            (0..ACCESS_BITS).fold(0u64, |a, k| a | (self.dr[1 + k] as u64) << k);
                        self.transact(w & 1 == 1, ((w >> 1) & 3) as u8 * 4, (w >> 3) as u32);
                    }
                }
                _ => {}
            }
            self.state = next;
            tdo
        }

        fn transact(&mut self, read: bool, a32: u8, data: u32) {
            if self.dap_ir == 0xa {
                match (a32, read) {
                    (0x4, true) => self.last_read = self.ctrl_stat,
                    (0x4, false) => {
                        let acks = (data & (CSYSPWRUPREQ | CDBGPWRUPREQ)) << 1;
                        let sticky = self.ctrl_stat & STICKYERR & !(data & STICKYERR);
                        self.ctrl_stat = (data & !STICKYERR) | acks | sticky;
                    }
                    (0x8, false) => self.select = data,
                    (0xc, true) => {} // RDBUFF: the last read again, no side effect
                    _ => panic!("DP access {a32:#x} read {read}"),
                }
                return;
            }
            assert_eq!(self.select >> 24, 0, "only AP 0 in the model");
            let bank = (self.select >> 4 & 0xf) as u8;
            match (bank << 4 | a32, read) {
                (0x00, false) => self.csw = data,
                (0x04, false) => self.tar = data,
                (0x0c, false) => {
                    assert_eq!(self.csw & 7, 2);
                    if self.faults.contains(&self.tar) {
                        self.ctrl_stat |= STICKYERR;
                    } else {
                        self.mem.insert(self.tar, data);
                    }
                }
                (0x0c, true) => {
                    self.last_read = if self.faults.contains(&self.tar) {
                        self.ctrl_stat |= STICKYERR;
                        0
                    } else {
                        self.mem.get(&self.tar).copied().unwrap_or(0)
                    };
                }
                (0xfc, true) => self.last_read = 0x2477_0011, // AHB-AP
                other => panic!("AP access {other:?}"),
            }
            // CSW AddrInc single: TAR steps by 4 after a DRW access, wrapping
            // within its 1 KiB block as the narrowest implementations do.
            if a32 == 0x0c && bank == 0 && self.csw >> 4 & 3 == 1 {
                self.tar = (self.tar & !0x3ff) | (self.tar.wrapping_add(4) & 0x3ff);
            }
        }
    }

    impl Cable for Model {
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
            let mut out = vec![0u8; bits.div_ceil(8)];
            for i in 0..bits {
                if self.clock(exit && i == bits - 1, bit(tdi, i)) {
                    out[i / 8] |= 1 << (i % 8);
                }
            }
            Ok(capture.then_some(out))
        }
        fn idle(&mut self, clocks: u32) -> Result<()> {
            for _ in 0..clocks {
                self.clock(false, false);
            }
            Ok(())
        }
        fn flush(&mut self) -> Result<()> {
            Ok(())
        }
    }

    const ZYBO: [Tap; 2] = [XC7Z010, DAP];

    #[test]
    fn powers_up_and_reads_back_what_it_wrote() {
        let mut m = Model::default();
        {
            let mut dap = Dap::new(&mut m, &ZYBO, 1).unwrap();
            let mut ahb = MemAp::new(&mut dap, 0).unwrap();
            assert_eq!(ahb.idr().unwrap(), 0x2477_0011);
            ahb.write32(0xf800_025c, 0x1234_5678).unwrap();
            ahb.write32(0xf800_0260, 0x9abc_def0).unwrap();
            assert_eq!(ahb.read32(0xf800_025c).unwrap(), 0x1234_5678);
            assert_eq!(ahb.read32(0xf800_0260).unwrap(), 0x9abc_def0);
        }
        assert_eq!(
            m.ctrl_stat & (CSYSPWRUPACK | CDBGPWRUPACK),
            CSYSPWRUPACK | CDBGPWRUPACK
        );
        assert_eq!(m.csw, CSW_WORD);
    }

    #[test]
    fn block_reads_cross_1k_boundaries_and_survive_waits() {
        let mut m = Model::default();
        for i in 0..600u32 {
            m.mem.insert(0x3f0 + 4 * i, 0x1000 + i);
        }
        let mut dap = Dap::new(&mut m, &ZYBO, 1).unwrap();
        let mut ahb = MemAp::new(&mut dap, 0).unwrap();
        ahb.dap.cable.waits = 2;
        let got = ahb.read_block(0x3f0, 600).unwrap();
        assert_eq!(got, (0..600).map(|i| 0x1000 + i).collect::<Vec<_>>());
    }

    #[test]
    fn a_batch_writes_and_reads_fixed_addresses_in_order() {
        let mut m = Model::default();
        let mut dap = Dap::new(&mut m, &ZYBO, 1).unwrap();
        let mut ahb = MemAp::new(&mut dap, 0).unwrap();
        let reads = ahb
            .run(
                &[
                    Access::Write(0x40, 1),
                    Access::Write(0x40, 2),
                    Access::Read(0x40),
                    Access::Write(0x44, 3),
                    Access::Read(0x44),
                    Access::Read(0x40),
                ],
                8,
            )
            .unwrap();
        assert_eq!(reads, [2, 3, 2]);
        // And back to stepping TAR for blocks.
        assert_eq!(ahb.read_block(0x40, 2).unwrap(), [2, 3]);
    }

    #[test]
    fn a_wait_is_retried_not_taken() {
        let mut m = Model::default();
        let mut dap = Dap::new(&mut m, &ZYBO, 1).unwrap();
        let mut ahb = MemAp::new(&mut dap, 0).unwrap();
        ahb.write32(0x100, 7).unwrap();
        ahb.dap.cable.waits = 3;
        assert_eq!(ahb.read32(0x100).unwrap(), 7);
    }

    #[test]
    fn a_bus_fault_fails_that_access_and_clears() {
        let mut m = Model {
            faults: vec![0xdead_0000],
            ..Default::default()
        };
        let mut dap = Dap::new(&mut m, &ZYBO, 1).unwrap();
        let mut ahb = MemAp::new(&mut dap, 0).unwrap();
        let err = ahb.read32(0xdead_0000).unwrap_err();
        assert!(format!("{err:#}").contains("bus fault"), "{err:#}");
        ahb.write32(0x100, 1).unwrap();
        assert_eq!(ahb.read32(0x100).unwrap(), 1);
    }

    #[test]
    fn a_tap_that_isnt_a_dap_is_refused() {
        let mut m = Model::default();
        assert!(Dap::new(&mut m, &ZYBO, 0).is_err());
    }
}
