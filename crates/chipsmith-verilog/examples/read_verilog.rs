//! `read_verilog [-lib FILE]... [-I DIR]... [-D NAME[=VALUE]]... -top TOP
//! [-o OUT.json] FILE...`: elaborates a design and writes the netlist as
//! yosys JSON, flattened with `-flatten`.

use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Instant;

fn main() -> ExitCode {
    let mut fe = chipsmith_verilog::Frontend::new();
    let mut files = Vec::new();
    let mut libs = Vec::new();
    let mut top = None;
    let mut out = None;
    let mut flatten = false;
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        match a.as_str() {
            "-lib" => libs.push(PathBuf::from(args.next().expect("-lib FILE"))),
            "-I" => fe.include_dir(args.next().expect("-I DIR")),
            "-D" => {
                let d = args.next().expect("-D NAME[=VALUE]");
                let (n, v) = d.split_once('=').unwrap_or((&d, "1"));
                fe.define(n, v);
            }
            "-top" => top = Some(args.next().expect("-top TOP")),
            "-o" => out = Some(PathBuf::from(args.next().expect("-o FILE"))),
            "-flatten" => flatten = true,
            _ => files.push(PathBuf::from(a)),
        }
    }
    let Some(top) = top else {
        eprintln!("usage: read_verilog [-lib FILE]... -top TOP [-o OUT.json] [-flatten] FILE...");
        return ExitCode::FAILURE;
    };
    let t = Instant::now();
    let result = (|| {
        for l in &libs {
            fe.read_library(l)?;
        }
        for f in &files {
            fe.read_file(f)?;
        }
        let parsed = t.elapsed();
        let d = fe.elaborate(&top)?;
        Ok::<_, chipsmith_verilog::Error>((d, parsed))
    })();
    let (mut design, parsed) = match result {
        Ok(d) => d,
        Err(e) => {
            eprintln!("error: {e}");
            return ExitCode::FAILURE;
        }
    };
    let elaborated = t.elapsed();
    if flatten {
        if let Err(e) = chipsmith_synth::flatten::flatten(&mut design) {
            eprintln!("error: flatten: {e}");
            return ExitCode::FAILURE;
        }
    }
    eprintln!(
        "parsed in {:.1} ms, elaborated in {:.1} ms, flattened in {:.1} ms",
        parsed.as_secs_f64() * 1e3,
        (elaborated - parsed).as_secs_f64() * 1e3,
        (t.elapsed() - elaborated).as_secs_f64() * 1e3
    );
    let w: Box<dyn std::io::Write> = match &out {
        Some(p) => Box::new(std::fs::File::create(p).expect("creating the output")),
        None => Box::new(std::io::stdout()),
    };
    if let Err(e) = chipsmith_netlist::write_json(&design, w) {
        eprintln!("error: {e}");
        return ExitCode::FAILURE;
    }
    ExitCode::SUCCESS
}
