//! Reads a yosys JSON netlist and writes it back, timing both:
//! `cargo run --release -p chipsmith-netlist --example roundtrip -- in.json out.json`.

use std::time::Instant;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let [input, output] = &args[..] else {
        eprintln!("usage: roundtrip IN.json OUT.json");
        std::process::exit(2);
    };
    let text = std::fs::read(input).expect("reading the input");
    let t = Instant::now();
    let design = chipsmith_netlist::read_json(&text).unwrap_or_else(|e| panic!("{input}: {e}"));
    let read = t.elapsed();
    let t = Instant::now();
    let file = std::fs::File::create(output).expect("creating the output");
    chipsmith_netlist::write_json(&design, file).expect("writing");
    let write = t.elapsed();
    let cells: usize = design.modules.iter().map(|m| m.cells.len()).sum();
    println!(
        "{input}: {} modules, {cells} cells, {} bytes; read {:.1} ms, write {:.1} ms",
        design.modules.len(),
        text.len(),
        read.as_secs_f64() * 1e3,
        write.as_secs_f64() * 1e3
    );
}
