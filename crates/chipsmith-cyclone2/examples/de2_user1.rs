//! Bench: read a DE2 design's USER1 register over the USB-Blaster with
//! chipsmith's own JTAG stack, twice, a second apart, and print the clock
//! rates the counters in it imply.
//!
//! The register is a 32-bit tag (the top word) above Gray-coded 32-bit
//! counters, one per clock, lowest word first:
//!
//! - examples/de2-jtag-counter: 64 bits, `"DE2!"` and CLOCK_50's counter.
//! - examples/de2-pll-counter: 128 bits, `"PLL!"`, CLOCK_50's counter, then
//!   one per PLL output. `--words 4` reads it.
//!
//! Both designs clock the register on TCK, so the shift runs at the cable's
//! byte-mode speed. `cargo run --release -p chipsmith-cyclone2 --example
//! de2_user1 [-- --words N]`
use std::time::{Duration, Instant};

use chipsmith_jtag::{usb_blaster::UsbBlaster, Cable, TapState};

/// The EP2C35's instruction register is 10 bits; USER1 is 0x00e.
const IR_LEN: usize = 10;
const USER1: u16 = 0x00e;
/// CLOCK_50: the first counter, against which the others are measured too.
const REFERENCE_HZ: f64 = 50e6;

fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let words = match args.as_slice() {
        [] => 2,
        [flag, n] if flag == "--words" => n.parse()?,
        _ => anyhow::bail!("usage: de2_user1 [--words N]"),
    };
    anyhow::ensure!(words >= 2, "a tag and at least one counter");

    let mut cable = UsbBlaster::open()?;
    cable.tms(&TapState::Reset.path_to(TapState::Reset))?;
    cable.tms(&TapState::Reset.path_to(TapState::IrShift))?;
    cable.shift(&USER1.to_le_bytes(), IR_LEN, true, false)?;
    cable.tms(&TapState::IrExit1.path_to(TapState::Idle))?;

    let first = (Instant::now(), read_dr(&mut cable, words)?);
    std::thread::sleep(Duration::from_secs(1));
    let second = (Instant::now(), read_dr(&mut cable, words)?);

    for (_, dr) in [&first, &second] {
        let tag = dr[words - 1];
        let counts: Vec<String> = dr[..words - 1]
            .iter()
            .map(|&g| gray_to_binary(g).to_string())
            .collect();
        println!(
            "USER1 {}: tag {:?}, counts {}",
            dr.iter()
                .rev()
                .map(|w| format!("{w:08x}"))
                .collect::<String>(),
            String::from_utf8_lossy(&tag.to_be_bytes()),
            counts.join(" ")
        );
    }
    let seconds = (second.0 - first.0).as_secs_f64();
    let delta = |k: usize| gray_to_binary(second.1[k]).wrapping_sub(gray_to_binary(first.1[k]));
    let reference = delta(0);
    println!(
        "counter 0: {reference} counts in {seconds:.3} s: {:.2} MHz",
        f64::from(reference) / seconds / 1e6
    );
    // The host's clock is good to a few ms over a second; the ratio to
    // CLOCK_50's counter, read in the same shifts, is exact to a count.
    for k in 1..words - 1 {
        let d = delta(k);
        println!(
            "counter {k}: {d} counts in {seconds:.3} s: {:.2} MHz; {:.6} x counter 0 = {:.4} MHz",
            f64::from(d) / seconds / 1e6,
            f64::from(d) / f64::from(reference),
            f64::from(d) / f64::from(reference) * REFERENCE_HZ / 1e6
        );
    }
    Ok(())
}

/// The register as 32-bit words, lowest first.
fn read_dr(cable: &mut UsbBlaster, words: usize) -> anyhow::Result<Vec<u32>> {
    let bits = 32 * words;
    cable.tms(&TapState::Idle.path_to(TapState::DrShift))?;
    // One bit more than the register: all but the last go out in byte
    // mode, which samples TDO at the rising TCK that also shifts the
    // register. The last one (leaving Shift-DR, with TMS high) is
    // bit-banged and sampled after that edge, so it may already be the
    // next bit; the extra bit takes that place and is dropped.
    let tdo = cable
        .shift(&vec![0; bits / 8 + 1], bits + 1, true, true)?
        .expect("captured");
    cable.tms(&TapState::DrExit1.path_to(TapState::Idle))?;
    cable.flush()?;
    Ok((0..words)
        .map(|w| u32::from_le_bytes(tdo[4 * w..4 * w + 4].try_into().unwrap()))
        .collect())
}

fn gray_to_binary(mut g: u32) -> u32 {
    let mut b = g;
    while g > 0 {
        g >>= 1;
        b ^= g;
    }
    b
}
