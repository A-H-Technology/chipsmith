//! Bench: read examples/de2-jtag-counter's 64-bit USER1 register over the
//! USB-Blaster with chipsmith's own JTAG stack, twice, a second apart.
//! Prints the tag, both counts and the clock rate they imply. The design
//! samples TCK rather than clocking on it, so the shift is bit-banged.
//! `cargo run --release -p chipsmith-cyclone2 --example de2_user1`
use std::time::{Duration, Instant};

use chipsmith_jtag::{usb_blaster::UsbBlaster, Cable, TapState};

/// The EP2C35's instruction register is 10 bits; USER1 is 0x00e.
const IR_LEN: usize = 10;
const USER1: u16 = 0x00e;
const DR_BITS: usize = 64;

fn main() -> anyhow::Result<()> {
    let mut cable = UsbBlaster::open()?;
    cable.tms(&TapState::Reset.path_to(TapState::Reset))?;
    cable.tms(&TapState::Reset.path_to(TapState::IrShift))?;
    cable.shift(&USER1.to_le_bytes(), IR_LEN, true, false)?;
    cable.tms(&TapState::IrExit1.path_to(TapState::Idle))?;

    let first = (Instant::now(), read_dr(&mut cable)?);
    std::thread::sleep(Duration::from_secs(1));
    let second = (Instant::now(), read_dr(&mut cable)?);

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
    let seconds = (second.0 - first.0).as_secs_f64();
    let delta = counts[1].wrapping_sub(counts[0]);
    println!(
        "{delta} counts in {seconds:.3} s: {:.2} MHz",
        delta as f64 / seconds / 1e6
    );
    Ok(())
}

fn read_dr(cable: &mut UsbBlaster) -> anyhow::Result<u64> {
    cable.tms(&TapState::Idle.path_to(TapState::DrShift))?;
    // One bit per call: the USB-Blaster bit-bangs shifts shorter than a
    // byte, a USB byte per TCK edge, which leaves the design (sampling TCK at
    // 50 MHz) far more than the ~80 ns half period of a byte-mode shift.
    let mut dr = 0u64;
    for i in 0..DR_BITS {
        let tdo = cable
            .shift(&[0], 1, i == DR_BITS - 1, true)?
            .expect("captured");
        dr |= u64::from(tdo[0] & 1) << i;
    }
    cable.tms(&TapState::DrExit1.path_to(TapState::Idle))?;
    cable.flush()?;
    // Bit-banged, the cable samples TDO after the rising TCK, by which time
    // the design has already shifted: the first bit read is the register's
    // bit 1. Bit 0 (the counter's lowest Gray bit) is lost, which only
    // makes the count's LSB uncertain.
    Ok(dr << 1)
}

fn gray_to_binary(mut g: u32) -> u32 {
    let mut b = g;
    while g > 0 {
        g >>= 1;
        b ^= g;
    }
    b
}
