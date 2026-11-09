//! Debugging aid: chipsmith's front end and synthesis of a design, every
//! stage's netlist written to a directory (`<stage>.json`), for looking at
//! what one stage did or mapping a stage's gates another way:
//! `synth_stages <out dir> <top> <verilog files...>`.

#[path = "../tests/support/mod.rs"]
mod support;

use std::path::PathBuf;

use chipsmith_cyclone2::synth::{synthesize_observed, Observer};
use chipsmith_netlist::{Design, Module};

struct Dump(PathBuf);

impl Observer for Dump {
    fn stage(&mut self, name: &str, m: &Module) {
        let d = Design {
            modules: vec![m.clone()],
        };
        support::write(&d, &self.0.join(format!("{name}.json")));
    }
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let dir = PathBuf::from(&args[1]);
    std::fs::create_dir_all(&dir).unwrap();
    let files: Vec<PathBuf> = args[3..]
        .iter()
        .map(|f| std::fs::canonicalize(f).unwrap_or_else(|e| panic!("{f}: {e}")))
        .collect();
    let input = support::frontend(&args[2], &files);
    let out = synthesize_observed(input, &Default::default(), &mut Dump(dir)).unwrap();
    eprintln!("{:?}", out.stats);
}
