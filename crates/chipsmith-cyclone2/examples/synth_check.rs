//! Synthesizes a design with chipsmith and with cyclone2-re's yosys flow,
//! proves chipsmith's netlist equivalent to the RTL, and packs both:
//! `synth_check <scratch dir> <top> <verilog files...>`. Needs the devshell.

#[path = "../tests/support/mod.rs"]
mod support;

use std::path::PathBuf;
use std::time::Instant;

use chipsmith_cyclone2::synth::{synthesize_observed, Observer};
use chipsmith_netlist::{Design, Module};

#[derive(Default)]
struct Keep {
    refined: Option<Design>,
    cells: Option<Design>,
}

impl Observer for Keep {
    fn stage(&mut self, name: &str, m: &Module) {
        let d = Some(Design {
            modules: vec![m.clone()],
        });
        match name {
            "refined" => self.refined = d,
            "cells" => self.cells = d,
            _ => {}
        }
    }
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let dir = PathBuf::from(&args[1]);
    std::fs::create_dir_all(&dir).unwrap();
    let top = &args[2];
    let files: Vec<PathBuf> = args[3..]
        .iter()
        .map(|f| std::fs::canonicalize(f).unwrap_or_else(|e| panic!("{f}: {e}")))
        .collect();
    // The netlist to synthesize: chipsmith's front end's, or with
    // `YOSYS_RTL` set yosys'. The equivalence check's reference is yosys'
    // either way.
    let rtl = support::rtl(&dir, top, &files);
    let input = if std::env::var_os("YOSYS_RTL").is_some() {
        support::read(&rtl)
    } else {
        support::frontend(top, &files)
    };
    let t = Instant::now();
    let mut keep = Keep::default();
    let out = synthesize_observed(input, &mut keep).unwrap();
    eprintln!("chipsmith synthesis: {:.2} s", t.elapsed().as_secs_f64());
    let ours = dir.join("chipsmith.json");
    support::write(&out, &ours);
    let flow = support::yosys_flow(&dir, top, &files);
    for (name, path) in [("yosys", &flow), ("chipsmith", &ours)] {
        let s = chipsmith_cyclone2::pack::pack_file(path, &dir.join(format!("{name}.packed.json")))
            .unwrap_or_else(|e| panic!("packing {name}: {e:#}"));
        eprintln!(
            "{name:>9}: {} LUTs, {} registers, {} carry chains, {} M4Ks",
            s.luts, s.ffs, s.chains, s.m4ks
        );
    }
    if std::env::var_os("NO_EQUIV").is_none() {
        let t = Instant::now();
        match support::check(&dir, &rtl, &keep.refined.unwrap(), &keep.cells.unwrap()) {
            Ok(p) => eprintln!(
                "equivalent: {} $equiv proven, {} registers proven constant ({:.1} s)",
                p.equivalences,
                p.constant_registers,
                t.elapsed().as_secs_f64()
            ),
            Err(e) => {
                eprintln!("NOT EQUIVALENT: {e}");
                std::process::exit(1);
            }
        }
    }
}
