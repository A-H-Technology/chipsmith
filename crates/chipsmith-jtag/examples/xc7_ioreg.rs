//! Bench: read examples/zybo-ioreg's USER1 register over the Zybo's
//! FT2232, twice, two seconds apart, and check its I/O-tile registers held
//! on the board: no error counted on either pin, and checks going on in
//! between. `cargo run --release -p chipsmith-jtag --example xc7_ioreg`
use std::time::Duration;

use anyhow::{ensure, Context};
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
    let mut readings = Vec::new();
    for i in 0..2 {
        if i > 0 {
            std::thread::sleep(Duration::from_secs(2));
        }
        let dr = read_user1(&mut cable, &chain, target)?;
        let tag = (dr >> 48) as u16;
        ensure!(
            tag == u16::from_be_bytes(*b"IR"),
            "USER1 {dr:#018x}: not examples/zybo-ioreg (tag {tag:#06x})"
        );
        let (rise, fall, checks) = ((dr >> 40) as u8, (dr >> 32) as u8, dr as u32);
        println!(
            "USER1 {dr:#018x}: rising-edge pin wrong {rise}x, falling-edge pin wrong {fall}x, {checks} checks"
        );
        readings.push((rise, fall, checks));
    }
    let (a, b) = (readings[0], readings[1]);
    ensure!(b.2 != a.2, "no checks between the reads");
    ensure!(b.0 == 0 && b.1 == 0, "an I/O-tile register read back wrong");
    println!("both pins drove, released and read back as their registers say");
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
