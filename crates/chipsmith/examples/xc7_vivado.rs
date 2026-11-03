//! A Zybo build's placement and routing as Vivado input, for timing it
//! with Vivado's timer (`chipsmith_xc7::vivado`):
//!
//! `xc7_vivado <project dir> <.fasm> <out dir> [period ns]` writes
//! `top.v`, `place.xdc`, `route.tcl` and `cells.tsv` into the out dir.

use std::path::PathBuf;

use anyhow::{bail, Context, Result};
use chipsmith::manifest::{Manifest, Target};

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let (project, fasm, out, period) = match &args[..] {
        [p, f, o] => (p, f, o, 8.0),
        [p, f, o, t] => (p, f, o, t.parse().context("the period in ns")?),
        _ => bail!("usage: xc7_vivado <project dir> <.fasm> <out dir> [period ns]"),
    };
    let manifest = Manifest::load(&PathBuf::from(project))?;
    let Target::Xc7z010clg400_1 { pins, .. } = &manifest.target else {
        bail!("{project} isn't a Zybo project");
    };
    let pins = pins.iter().map(|a| (a.port.to_string(), a.pin.to_string())).collect();
    let db = chipsmith_xc7::xray::load(chipsmith_xc7::part::XC7Z010CLG400_1)?;
    let family = PathBuf::from(std::env::var(chipsmith_xc7::xray::PRJXRAY_DB_VAR)?).join("zynq7");
    let fasm = std::fs::read_to_string(fasm).with_context(|| format!("reading {fasm}"))?;
    let routed = chipsmith_xc7::decode::decode_routed(&db, &family, &fasm, &pins)?;
    let e = chipsmith_xc7::vivado::export(&db, &routed, &pins, period)?;
    let out = PathBuf::from(out);
    std::fs::create_dir_all(&out)?;
    std::fs::write(out.join("top.v"), e.verilog)?;
    std::fs::write(out.join("place.xdc"), e.xdc)?;
    std::fs::write(out.join("route.tcl"), e.route)?;
    std::fs::write(out.join("cells.tsv"), e.cells)?;
    eprintln!("xc7_vivado: {} routes", e.routed);
    Ok(())
}
