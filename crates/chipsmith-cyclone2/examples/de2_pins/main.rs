//! Bench: read a running DE2 design's pins over the USB-Blaster with the
//! EP2C35's boundary-scan SAMPLE instruction, which captures every I/O
//! ball's level without taking the pins from the design. So LEDs, keys and
//! switches can be checked with nobody looking at the board.
//!
//! `cargo run --release -p chipsmith-cyclone2 --example de2_pins --
//! examples/sha256 [--samples N] [--every MS] [--only LEDR,LEDG]`
//!
//! It prints each signal of the project's `[pins]` (index 0 rightmost) once
//! per sample: what the design drives on a driven pin, the pad's level on
//! an undriven one.
//!
//! The cell table (bsr.rs) is the BC_1 input cell of each ball in Altera's
//! EP2C35F672 BSDL, BOUNDARY_LENGTH 1449: a copy is at
//! <https://github.com/opengateware-labs/BSDL> ("Altera (Intel)/Cyclone II/
//! EP2C35F672.BSD"), Intel's own download refuses scripted fetches.
use std::{collections::HashMap, path::PathBuf, time::Duration};

use anyhow::{bail, Context};
use chipsmith_jtag::{usb_blaster::UsbBlaster, Cable, TapState};

mod bsr;

const IR_LEN: usize = 10;
const SAMPLE: u16 = 0x005;
const BOUNDARY_LENGTH: usize = 1449;

fn main() -> anyhow::Result<()> {
    let mut args = std::env::args().skip(1);
    let mut project = None;
    let mut samples = 1usize;
    let mut every = Duration::from_millis(250);
    let mut only: Option<Vec<String>> = None;
    while let Some(a) = args.next() {
        match a.as_str() {
            "--samples" => samples = args.next().context("--samples N")?.parse()?,
            "--every" => every = Duration::from_millis(args.next().context("--every MS")?.parse()?),
            "--only" => {
                only = Some(
                    args.next()
                        .context("--only A,B")?
                        .split(',')
                        .map(str::to_owned)
                        .collect(),
                )
            }
            _ if project.is_none() => project = Some(PathBuf::from(a)),
            _ => bail!("usage: de2_pins <project dir> [--samples N] [--every MS] [--only A,B]"),
        }
    }
    let project = project.context("usage: de2_pins <project dir>")?;
    let signals = pins(&project.join("chipsmith.toml"))?;
    let cells: HashMap<&str, usize> = bsr::INPUT_CELLS.iter().copied().collect();
    let shown: Vec<(&String, Vec<usize>)> = signals
        .iter()
        .filter(|(name, _)| only.as_ref().is_none_or(|o| o.contains(name)))
        .filter_map(|(name, balls)| {
            let bits: Option<Vec<usize>> = balls
                .iter()
                .map(|b| cells.get(b.trim_start_matches("PIN_")).copied())
                .collect();
            // Dedicated clock inputs (CLOCK_50) have no boundary cell.
            bits.map(|bits| (name, bits))
        })
        .collect();

    let mut cable = UsbBlaster::open()?;
    cable.tms(&TapState::Reset.path_to(TapState::Reset))?;
    cable.tms(&TapState::Reset.path_to(TapState::IrShift))?;
    cable.shift(&SAMPLE.to_le_bytes(), IR_LEN, true, false)?;
    cable.tms(&TapState::IrExit1.path_to(TapState::Idle))?;
    for s in 0..samples {
        if s > 0 {
            std::thread::sleep(every);
        }
        let bsr = capture(&mut cable)?;
        let line: Vec<String> = shown
            .iter()
            .map(|(name, bits)| {
                let v: String = bits
                    .iter()
                    .rev()
                    .map(|&b| if pad(&bsr, b) { '1' } else { '0' })
                    .collect();
                format!("{name}={v}")
            })
            .collect();
        println!("{}", line.join(" "));
    }
    cable.tms(&TapState::Idle.path_to(TapState::Reset))?;
    cable.flush()?;
    Ok(())
}

/// One Capture-DR of the boundary register, bit `i` the cell numbered `i`.
fn capture(cable: &mut UsbBlaster) -> anyhow::Result<Vec<bool>> {
    cable.tms(&TapState::Idle.path_to(TapState::DrShift))?;
    // The zeros shifted in only reach the update latches, which drive nothing
    // until EXTEST. Bit i is cell i here, with no skew (de2_user1's one-bit
    // skew is its user register's): on sha256 the KEYs' input and control
    // cells read 1, the LEDs' control cells 0, their output cells the fold.
    let tdo = cable
        .shift(
            &vec![0; BOUNDARY_LENGTH.div_ceil(8)],
            BOUNDARY_LENGTH,
            true,
            true,
        )?
        .expect("captured");
    cable.tms(&TapState::DrExit1.path_to(TapState::Idle))?;
    cable.flush()?;
    Ok((0..BOUNDARY_LENGTH)
        .map(|i| chipsmith_jtag::bit(&tdo, i))
        .collect())
}

/// A ball's level from its cells, which for every ball in bsr.rs are input,
/// control (1: not driven) and output at `input`, +1 and +2 in the BSDL. A
/// driven pin's input cell reads 1 whatever it drives (seen on the LEDs), so
/// a driven pin reads its output cell.
fn pad(bsr: &[bool], input: usize) -> bool {
    if bsr[input + 1] {
        bsr[input]
    } else {
        bsr[input + 2]
    }
}

/// The project's `[pins]`, each signal's balls with index 0 first.
fn pins(path: &std::path::Path) -> anyhow::Result<Vec<(String, Vec<String>)>> {
    let text = std::fs::read_to_string(path).with_context(|| path.display().to_string())?;
    let doc: toml::Table = text.parse()?;
    let pins = doc
        .get("pins")
        .and_then(|p| p.as_table())
        .context("no [pins]")?;
    let list = |v: &toml::Value| -> Option<Vec<String>> {
        v.as_array()?
            .iter()
            .map(|p| p.as_str().map(str::to_owned))
            .collect()
    };
    pins.iter()
        .map(|(name, v)| {
            let balls = match v {
                toml::Value::String(p) => Some(vec![p.clone()]),
                toml::Value::Array(_) => list(v),
                toml::Value::Table(t) => t
                    .get("pin")
                    .and_then(|p| p.as_str())
                    .map(|p| vec![p.to_owned()])
                    .or_else(|| t.get("pins").and_then(list)),
                _ => None,
            };
            Ok((name.clone(), balls.with_context(|| format!("pin {name}"))?))
        })
        .collect()
}
