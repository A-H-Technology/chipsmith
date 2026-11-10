//! Bench: read bench/ring's ring-oscillator counts over the USB-Blaster.
//!
//! The design's USER1 register is, lowest word first: a pad word (the
//! cable loses bit 0, see `de2_user1`), the sequencer's round, one count
//! per ring (its edges in its last window of `window` CLOCK_50 cycles), and
//! the tag `"RING"`. Each ring runs once every `rings` rounds, so this
//! waits for `windows` fresh counts of every ring and prints one line per
//! count: `ring <k> <count> <MHz>`.
//!
//! `de2_ring <rings> <window cycles> <windows>`
use std::time::Duration;

use chipsmith_jtag::{usb_blaster::UsbBlaster, Cable, TapState};

const IR_LEN: usize = 10;
const USER1: u16 = 0x00e;
const CLOCK_HZ: f64 = 50e6;
const TAG: u32 = u32::from_be_bytes(*b"RING");

fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let [rings, window, windows] = &args[..] else {
        anyhow::bail!("usage: de2_ring <rings> <window cycles> <windows>");
    };
    let (rings, window, windows): (usize, f64, usize) =
        (rings.parse()?, window.parse()?, windows.parse()?);
    let words = 3 + rings;

    let mut cable = UsbBlaster::open()?;
    cable.tms(&TapState::Reset.path_to(TapState::Reset))?;
    cable.tms(&TapState::Reset.path_to(TapState::IrShift))?;
    cable.shift(&USER1.to_le_bytes(), IR_LEN, true, false)?;
    cable.tms(&TapState::IrExit1.path_to(TapState::Idle))?;

    let mut last_round = None;
    let mut seen = vec![0usize; rings];
    // A ring that doesn't oscillate never reads a count: give up on it
    // after every ring has had two windows more than asked for.
    let mut rounds = 0;
    while seen.iter().any(|&n| n < windows) && rounds < rings * (windows + 2) {
        // A read can tear while the sequencer updates a count; two equal
        // reads in a row can't.
        let a = read_dr(&mut cable, words)?;
        let b = read_dr(&mut cable, words)?;
        anyhow::ensure!(a[words - 1] == TAG, "tag {:08x}, not \"RING\"", a[words - 1]);
        if a != b {
            continue;
        }
        let round = a[1];
        match last_round {
            None => last_round = Some(round),
            Some(r) if r == round => {}
            Some(_) => {
                // The window that just ended is the ring before the one now
                // running; skip the first round seen (it may be partial).
                let k = (round as usize + rings - 1) % rings;
                let count = a[2 + k];
                // A ring that hasn't had its window yet reads 0.
                if count > 0 {
                    println!(
                        "ring {k} {count} {:.4}",
                        f64::from(count) * CLOCK_HZ / window / 1e6
                    );
                    seen[k] += 1;
                }
                last_round = Some(round);
                rounds += 1;
            }
        }
        std::thread::sleep(Duration::from_millis(30));
    }
    Ok(())
}

fn read_dr(cable: &mut UsbBlaster, words: usize) -> anyhow::Result<Vec<u32>> {
    let bits = 32 * words;
    cable.tms(&TapState::Idle.path_to(TapState::DrShift))?;
    let tdo = cable
        .shift(&vec![0; bits / 8], bits, true, true)?
        .expect("captured");
    cable.tms(&TapState::DrExit1.path_to(TapState::Idle))?;
    cable.flush()?;
    let dr = (0..words)
        .map(|w| u32::from_le_bytes(tdo[4 * w..4 * w + 4].try_into().unwrap()))
        .collect::<Vec<_>>();
    Ok((0..words)
        .map(|w| dr[w] << 1 | if w == 0 { 0 } else { dr[w - 1] >> 31 })
        .collect())
}
