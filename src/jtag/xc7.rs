//! Loading a Xilinx 7-series `.bit` over JTAG (UG470, "JTAG configuration"),
//! as a sequence for the SVF player: the device sits in a chain (on a Zynq,
//! behind the ARM debug port), so every IR and DR shift pads the other
//! devices into BYPASS, and INIT and DONE are checked against TDO the way
//! `cram svf` checks CONF_DONE.
//!
//! Sequence as openFPGALoader (src/xilinx.cpp) runs it: JPROGRAM, wait for
//! INIT, CFG_IN, the bitstream (each byte MSB first), JSTART, 2000 TCKs,
//! then DONE.

use anyhow::{bail, ensure, Context, Result};

use std::time::{Duration, Instant};

use super::svf::{Bits, Command, Player, Shift};
use super::{bit, Cable, TapState};

const IR_LEN: usize = 6;
const JPROGRAM: u64 = 0x0b;
const CFG_IN: u64 = 0x05;
const JSTART: u64 = 0x0c;
const BYPASS: u64 = 0x3f;
/// IR capture bits of a 7-series TAP.
const IR_INIT: usize = 4;
const IR_DONE: usize = 5;

/// A TAP on the chain, nearest TDO first (the order IDCODEs come out in).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Tap {
    pub idcode: u32,
    pub ir_len: usize,
}

/// Instruction register lengths of the TAPs chipsmith knows.
fn ir_len(idcode: u32) -> Option<usize> {
    match idcode & 0x0fff_ffff {
        // ARM CoreSight DAP (Zynq-7000's processor side).
        0x0ba0_0477 => Some(4),
        // Xilinx 7-series: manufacturer 0x049, family bits in 27..21.
        id if id & 0xfff == 0x093 => Some(IR_LEN),
        _ => None,
    }
}

/// Read every TAP's IDCODE: after Test-Logic-Reset each TAP's DR is its
/// IDCODE (bit 0 set) or a 1-bit BYPASS (0).
pub fn scan(cable: &mut impl Cable) -> Result<Vec<Tap>> {
    cable.tms(&TapState::Reset.path_to(TapState::Reset))?;
    cable.tms(&TapState::Reset.path_to(TapState::DrShift))?;
    let bits = 32 * 8;
    let tdo = cable
        .shift(&vec![0xff; bits / 8], bits, true, true)?
        .context("no TDO")?;
    cable.tms(&TapState::DrExit1.path_to(TapState::Reset))?;
    cable.flush()?;

    let mut taps = Vec::new();
    let mut i = 0;
    while i + 32 <= bits {
        if !bit(&tdo, i) {
            bail!("a TAP without IDCODE in the chain; chipsmith can't size its IR");
        }
        let idcode = (0..32).fold(0u32, |acc, k| acc | (bit(&tdo, i + k) as u32) << k);
        // Ones shifted in come out once every TAP has been passed.
        if idcode == u32::MAX {
            break;
        }
        let ir_len =
            ir_len(idcode).with_context(|| format!("unknown TAP with IDCODE {idcode:#010x}"))?;
        taps.push(Tap { idcode, ir_len });
        i += 32;
    }
    ensure!(!taps.is_empty(), "no TAPs found; is the board powered?");
    Ok(taps)
}

fn bits_of(len: usize, value: impl Fn(usize) -> bool) -> Bits {
    let mut bytes = vec![0u8; len.div_ceil(8)];
    for i in 0..len {
        if value(i) {
            bytes[i / 8] |= 1 << (i % 8);
        }
    }
    Bits { len, bytes }
}

/// The `.bit` payload: the header's fields `a`..`d` are skipped, `e` holds
/// the length of what follows.
pub fn bit_payload(file: &[u8]) -> Result<&[u8]> {
    // A 13-byte preamble field, then `a` (a 2-byte key 0x0001 + 'a').
    let mut at = 2 + u16::from_be_bytes([file[0], file[1]]) as usize;
    at += 2;
    loop {
        let key = *file.get(at).context(".bit header is truncated")?;
        at += 1;
        if key == b'e' {
            let len = u32::from_be_bytes(
                file.get(at..at + 4)
                    .context(".bit header is truncated")?
                    .try_into()?,
            ) as usize;
            at += 4;
            return file.get(at..at + len).context(".bit payload is truncated");
        }
        ensure!(
            matches!(key, b'a'..=b'd'),
            ".bit header has an unknown field {:?}",
            key as char
        );
        let len = u16::from_be_bytes(
            file.get(at..at + 2)
                .context(".bit header is truncated")?
                .try_into()?,
        ) as usize;
        at += 2 + len;
    }
}

/// A `.bit` payload as the Zynq's devcfg driver takes it (Xilinx calls it a
/// byte-swapped `.bin`): every 32-bit word little-endian, so the sync word
/// reads `66 55 99 aa`. The PCAP DMA moves whole words.
pub fn zynq_bin(payload: &[u8]) -> Result<Vec<u8>> {
    ensure!(
        payload.len().is_multiple_of(4),
        ".bit payload is {} bytes, not whole 32-bit words",
        payload.len()
    );
    Ok(payload
        .as_chunks::<4>()
        .0
        .iter()
        .flat_map(|&[a, b, c, d]| [d, c, b, a])
        .collect())
}

/// Load `bitstream` (a `.bit` payload) into TAP `target` of `chain`: clear
/// the configuration, wait for INIT, then play the load and check DONE.
pub fn load<C: Cable>(cable: &mut C, chain: &[Tap], target: usize, bitstream: &[u8]) -> Result<()> {
    let Program {
        clear,
        init_ir,
        load,
    } = program(chain, target, bitstream);
    Player::new(cable).play(&clear)?;
    // Configuration memory clears in up to 10 ms (UG470); poll INIT in the
    // target's IR capture, as openFPGALoader does.
    let deadline = Instant::now() + Duration::from_secs(1);
    loop {
        cable.tms(&TapState::Idle.path_to(TapState::IrShift))?;
        let tdo = cable
            .shift(&init_ir.bytes, init_ir.len, true, true)?
            .context("no TDO")?;
        cable.tms(&TapState::IrExit1.path_to(TapState::Idle))?;
        let ir_offset: usize = chain[..target].iter().map(|t| t.ir_len).sum();
        if bit(&tdo, ir_offset + IR_INIT) {
            break;
        }
        ensure!(
            Instant::now() < deadline,
            "INIT never went high after JPROGRAM"
        );
        cable.idle(15_000)?;
    }
    Player::new(cable).play(&load)
}

/// The JTAG sequence `load` plays: everything up to and from the INIT poll.
pub struct Program {
    pub clear: Vec<Command>,
    /// BYPASS on every TAP: shifted while polling INIT.
    pub init_ir: Bits,
    pub load: Vec<Command>,
}

pub fn program(chain: &[Tap], target: usize, bitstream: &[u8]) -> Program {
    // IR: TAP 0 (nearest TDO) takes the first bits shifted.
    let ir_total: usize = chain.iter().map(|t| t.ir_len).sum();
    let ir_offset: usize = chain[..target].iter().map(|t| t.ir_len).sum();
    let ir = |op: u64| {
        bits_of(ir_total, |i| {
            if (ir_offset..ir_offset + IR_LEN).contains(&i) {
                op >> (i - ir_offset) & 1 == 1
            } else {
                true // BYPASS on every other TAP
            }
        })
    };
    let sir = |op: u64, expect: Option<usize>| {
        Command::Sir(Shift {
            len: ir_total,
            tdi: Some(ir(op)),
            tdo: expect.map(|b| bits_of(ir_total, |i| i == ir_offset + b)),
            mask: expect.map(|b| bits_of(ir_total, |i| i == ir_offset + b)),
        })
    };
    // DR: one BYPASS bit per TAP nearer TDO goes first, then the data; each
    // byte of a .bit is sent MSB first.
    let pad = target;
    let data = bits_of(pad + bitstream.len() * 8, |i| {
        i >= pad && {
            let k = i - pad;
            bitstream[k / 8] >> (7 - k % 8) & 1 == 1
        }
    });
    let runtest = |clocks| Command::RunTest {
        run_state: Some(TapState::Idle),
        clocks,
        end_state: None,
    };
    let clear = vec![
        Command::State(vec![TapState::Reset, TapState::Idle]),
        sir(JPROGRAM, None),
    ];
    let load = vec![
        Command::State(vec![TapState::Reset, TapState::Idle]),
        sir(CFG_IN, None),
        Command::Sdr(Shift {
            len: data.len,
            tdi: Some(data),
            tdo: None,
            mask: None,
        }),
        sir(JSTART, None),
        runtest(2000),
        Command::State(vec![TapState::Reset, TapState::Idle]),
        sir(BYPASS, Some(IR_DONE)),
    ];
    Program {
        clear,
        init_ir: ir(BYPASS),
        load,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const DAP: Tap = Tap {
        idcode: 0x4ba0_0477,
        ir_len: 4,
    };
    const XC7Z010: Tap = Tap {
        idcode: 0x0372_2093,
        ir_len: 6,
    };
    /// The Zybo's chain as scanned: the xc7z010 nearest TDO.
    const ZYBO: [Tap; 2] = [XC7Z010, DAP];
    /// The other order, where padding goes in front of the target's bits.
    const BEHIND_DAP: [Tap; 2] = [DAP, XC7Z010];

    #[test]
    fn on_the_zybo_the_target_takes_the_first_ir_bits_and_dr_needs_no_padding() {
        let p = program(&ZYBO, 0, &[0x80]);
        let Command::Sir(Shift { tdi: Some(ir), .. }) = &p.clear[1] else {
            panic!()
        };
        // JPROGRAM 0b001011 LSB first, then the DAP's 4 bits BYPASS.
        assert_eq!(ir.bytes, vec![0b1100_1011, 0b11]);
        let Command::Sdr(Shift { tdi: Some(d), .. }) = &p.load[2] else {
            panic!()
        };
        assert_eq!((d.len, d.bytes.as_slice()), (8, &[0x01][..]));
    }

    #[test]
    fn known_taps_have_their_ir_lengths() {
        assert_eq!(ir_len(0x4ba0_0477), Some(4));
        assert_eq!(ir_len(0x0372_2093), Some(6)); // xc7z010
        assert_eq!(ir_len(0x1234_5671), None);
    }

    #[test]
    fn instructions_reach_the_target_and_bypass_the_dap() {
        let commands = program(&BEHIND_DAP, 1, &[0x80]).clear;
        let Command::Sir(Shift { tdi: Some(ir), .. }) = &commands[1] else {
            panic!()
        };
        // DAP's 4 bits BYPASS, then JPROGRAM 0b001011 LSB first.
        assert_eq!(ir.len, 10);
        assert_eq!(ir.bytes, vec![0b1011_1111, 0b00]);
    }

    #[test]
    fn data_is_msb_first_after_one_bypass_bit_per_tap_nearer_tdo() {
        let commands = program(&BEHIND_DAP, 1, &[0x80, 0x01]).load;
        let Command::Sdr(Shift { tdi: Some(d), .. }) = &commands[2] else {
            panic!()
        };
        assert_eq!(d.len, 17);
        // bit 0: DAP bypass; bit 1: 0x80's MSB; bit 16: 0x01's LSB.
        let set: Vec<usize> = (0..d.len).filter(|&i| bit(&d.bytes, i)).collect();
        assert_eq!(set, [1, 16]);
    }

    #[test]
    fn done_is_checked_on_the_target_only() {
        let commands = program(&BEHIND_DAP, 1, &[0]).load;
        let Some(Command::Sir(Shift { mask: Some(m), .. })) = commands.last() else {
            panic!()
        };
        let set: Vec<usize> = (0..m.len).filter(|&i| bit(&m.bytes, i)).collect();
        assert_eq!(set, [4 + IR_DONE]);
    }

    #[test]
    fn zynq_bin_swaps_each_word() {
        let sync = [0xff, 0xff, 0xff, 0xff, 0xaa, 0x99, 0x55, 0x66];
        assert_eq!(
            zynq_bin(&sync).unwrap(),
            [0xff, 0xff, 0xff, 0xff, 0x66, 0x55, 0x99, 0xaa]
        );
        assert!(zynq_bin(&[0xaa, 0x99, 0x55]).is_err());
    }

    #[test]
    fn bit_header_is_skipped() {
        let mut f = vec![
            0x00, 0x09, 0x0f, 0xf0, 0x0f, 0xf0, 0x0f, 0xf0, 0x0f, 0xf0, 0x00, 0x00, 0x01,
        ];
        f.extend_from_slice(
            b"a\x00\x02x\x00b\x00\x01\x00c\x00\x01\x00d\x00\x01\x00e\x00\x00\x00\x03",
        );
        f.extend_from_slice(&[0xaa, 0x99, 0x55]);
        assert_eq!(bit_payload(&f).unwrap(), [0xaa, 0x99, 0x55]);
    }
}
