//! Bench: read examples/zybo-jtag-counter's 64-bit USER1 register over a
//! Digilent FT2232 with chipsmith's own JTAG stack, twice, a second apart.
//! Prints the tag, both counts and the clock rate they imply.
//! `cargo run --release --example xc7_user1`
use std::time::{Duration, Instant};

use anyhow::Context;
use chipsmith_jtag::{bit, ftdi::Ftdi, xc7, Cable, TapState};

/// USER1 on 7-series (UG470, table 6-3); every other TAP gets BYPASS (all ones).
const USER1: u64 = 0x02;
const DR_BITS: usize = 64;

fn main() -> anyhow::Result<()> {
    let mut cable = Ftdi::open()?;
    let chain = xc7::scan(&mut cable)?;
    let target = chain
        .iter()
        .position(|t| t.ir_len == 6)
        .context("no 7-series TAP on the chain")?;

    let first = (Instant::now(), read_user1(&mut cable, &chain, target)?);
    std::thread::sleep(Duration::from_secs(1));
    let second = (Instant::now(), read_user1(&mut cable, &chain, target)?);

    let mut counts = Vec::new();
    for (_, dr) in [&first, &second] {
        let tag = (dr >> 32) as u32;
        let count = gray_to_binary(*dr as u32);
        println!(
            "USER1 {dr:#018x}: tag {:?}, count {count}",
            String::from_utf8_lossy(&tag.to_be_bytes())
        );
        counts.push(count);
    }
    let elapsed = second.0 - first.0;
    let delta = counts[1].wrapping_sub(counts[0]);
    println!(
        "{delta} counts in {:.3} s: {:.2} MHz",
        elapsed.as_secs_f64(),
        delta as f64 / elapsed.as_secs_f64() / 1e6
    );
    Ok(())
}

/// TAP 0 is nearest TDO: its IR bits are shifted first, and in DR each TAP
/// nearer TDO than the target adds one BYPASS bit in front of the data.
fn read_user1(cable: &mut Ftdi, chain: &[xc7::Tap], target: usize) -> anyhow::Result<u64> {
    let ir_len: usize = chain.iter().map(|t| t.ir_len).sum();
    let offset: usize = chain[..target].iter().map(|t| t.ir_len).sum();
    let ir = bits(ir_len, |i| {
        if (offset..offset + 6).contains(&i) {
            USER1 >> (i - offset) & 1 == 1
        } else {
            true
        }
    });
    cable.tms(&TapState::Reset.path_to(TapState::Reset))?;
    cable.tms(&TapState::Reset.path_to(TapState::IrShift))?;
    cable.shift(&ir, ir_len, true, false)?;
    cable.tms(&TapState::IrExit1.path_to(TapState::DrShift))?;
    let dr_len = chain.len() - 1 + DR_BITS;
    let tdo = cable
        .shift(&vec![0; dr_len.div_ceil(8)], dr_len, true, true)?
        .context("no TDO")?;
    cable.tms(&TapState::DrExit1.path_to(TapState::Idle))?;
    cable.flush()?;
    Ok((0..DR_BITS).fold(0u64, |acc, i| acc | (bit(&tdo, target + i) as u64) << i))
}

fn bits(len: usize, value: impl Fn(usize) -> bool) -> Vec<u8> {
    let mut bytes = vec![0u8; len.div_ceil(8)];
    for i in (0..len).filter(|&i| value(i)) {
        bytes[i / 8] |= 1 << (i % 8);
    }
    bytes
}

fn gray_to_binary(gray: u32) -> u32 {
    (1..32).fold(gray, |b, shift| b ^ (gray >> shift))
}
