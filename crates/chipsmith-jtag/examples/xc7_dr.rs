//! Read a 7-series USER1 data register of any length over a Digilent
//! FT2232 with chipsmith's own JTAG stack, and print it as 32-bit words,
//! least significant first (examples/zybo-shapes' run.sh).
//! `cargo run --release --example xc7_dr -- BITS`
use anyhow::Context;
use chipsmith_jtag::{bit, ftdi::Ftdi, xc7, Cable, TapState};

/// USER1 on 7-series (UG470, table 6-3); every other TAP gets BYPASS (all ones).
const USER1: u64 = 0x02;

fn main() -> anyhow::Result<()> {
    let bits: usize = std::env::args()
        .nth(1)
        .context("usage: xc7_dr BITS")?
        .parse()
        .context("BITS")?;
    let mut cable = Ftdi::open()?;
    let chain = xc7::scan(&mut cable)?;
    let target = chain
        .iter()
        .position(|t| t.ir_len == 6)
        .context("no 7-series TAP on the chain")?;

    // TAP 0 is nearest TDO: its IR bits are shifted first, and in DR each
    // TAP nearer TDO than the target adds one BYPASS bit in front of the data.
    let ir_len: usize = chain.iter().map(|t| t.ir_len).sum();
    let offset: usize = chain[..target].iter().map(|t| t.ir_len).sum();
    let mut ir = vec![0u8; ir_len.div_ceil(8)];
    for i in 0..ir_len {
        let one = if (offset..offset + 6).contains(&i) {
            USER1 >> (i - offset) & 1 == 1
        } else {
            true
        };
        if one {
            ir[i / 8] |= 1 << (i % 8);
        }
    }
    cable.tms(&TapState::Reset.path_to(TapState::Reset))?;
    cable.tms(&TapState::Reset.path_to(TapState::IrShift))?;
    cable.shift(&ir, ir_len, true, false)?;
    cable.tms(&TapState::IrExit1.path_to(TapState::DrShift))?;
    let dr_len = chain.len() - 1 + bits;
    let tdo = cable
        .shift(&vec![0; dr_len.div_ceil(8)], dr_len, true, true)?
        .context("no TDO")?;
    cable.tms(&TapState::DrExit1.path_to(TapState::Idle))?;
    cable.flush()?;

    for w in 0..bits.div_ceil(32) {
        let word = (0..32).filter(|i| w * 32 + i < bits).fold(0u32, |acc, i| {
            acc | (bit(&tdo, target + w * 32 + i) as u32) << i
        });
        println!("{word:08x}");
    }
    Ok(())
}
