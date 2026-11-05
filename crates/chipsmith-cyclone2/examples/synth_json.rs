//! Synthesizes an RTL netlist (yosys JSON at `proc; flatten` level) to
//! Cyclone II cells: `synth_json <rtl.json> <out.json> [stage dir]`. With
//! a stage directory, every stage's netlist is written there too.

use std::path::{Path, PathBuf};
use std::time::Instant;

use anyhow::{Context, Result};
use chipsmith_cyclone2::synth::{synthesize_observed, Observer};
use chipsmith_netlist::{read_json, write_json, Design, Module};

struct Dump(Option<PathBuf>);

impl Observer for Dump {
    fn stage(&mut self, name: &str, m: &Module) {
        if let Some(dir) = &self.0 {
            let d = Design {
                modules: vec![m.clone()],
            };
            let f = std::fs::File::create(dir.join(format!("{name}.json"))).unwrap();
            write_json(&d, std::io::BufWriter::new(f)).unwrap();
        }
        let mut counts = std::collections::BTreeMap::<String, usize>::new();
        for c in &m.cells {
            *counts.entry(c.kind.type_name().into_owned()).or_default() += 1;
        }
        eprintln!("  {name}: {counts:?}");
    }
}

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().collect();
    let (input, output) = (Path::new(&args[1]), Path::new(&args[2]));
    let dir = args.get(3).map(PathBuf::from);
    if let Some(d) = &dir {
        std::fs::create_dir_all(d)?;
    }
    let design = read_json(&std::fs::read(input)?).with_context(|| input.display().to_string())?;
    let t = Instant::now();
    let out = synthesize_observed(design, &Default::default(), &mut Dump(dir))?;
    eprintln!("{:?}", out.stats);
    let out = out.design;
    eprintln!("synthesized in {:.2} s", t.elapsed().as_secs_f64());
    write_json(
        &out,
        std::io::BufWriter::new(std::fs::File::create(output)?),
    )?;
    Ok(())
}
