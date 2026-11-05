//! Round trips through chipsmith-netlist, with yosys as the oracle
//! (ADR-0004). For each corpus design and synthesis level, yosys writes the
//! netlist; the IR reads it and writes it back; then:
//!
//! - reading what was written gives the same IR (structural round trip);
//! - yosys reads both files to the same design: its `write_json` of each
//!   is the same JSON, up to the order of object keys and the width of
//!   flag parameters, which yosys itself writes inconsistently;
//! - yosys proves the two equivalent (`equiv_make`/`equiv_simple`/
//!   `equiv_induct`).
//!
//! Needs yosys on PATH: run in the devshell.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Instant;

use serde_json::Value;

const CORPUS: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/corpus");

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Level {
    /// `read_verilog; proc`: RTL cells, `$memrd`/`$memwr` and memories.
    Rtl,
    /// `synth -run :fine`: `$alu`, `$mem_v2`, enable/reset registers.
    Coarse,
    /// `synth -run :fine; techmap; opt`: gates and gate-level registers.
    Gates,
    /// cyclone2-re's `synth/cyclone2.ys`: Cyclone II device cells.
    Cyclone2,
}

impl Level {
    fn name(self) -> &'static str {
        match self {
            Level::Rtl => "rtl",
            Level::Coarse => "coarse",
            Level::Gates => "gates",
            Level::Cyclone2 => "cyclone2",
        }
    }
}

fn scratch(design: &str, level: Level) -> PathBuf {
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR"))
        .join("yosys-roundtrip")
        .join(format!("{design}-{}", level.name()));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// Runs a yosys script, panicking with the end of its log if it fails.
fn yosys(dir: &Path, name: &str, script: &str, cwd: &Path) {
    let ys = dir.join(format!("{name}.ys"));
    let log = dir.join(format!("{name}.log"));
    std::fs::write(&ys, script).unwrap();
    let out = Command::new("yosys")
        .arg("-q")
        .arg("-l")
        .arg(&log)
        .arg("-s")
        .arg(&ys)
        .current_dir(cwd)
        .output()
        .unwrap_or_else(|e| {
            panic!("can't run yosys ({e}); these tests need it on PATH: run them in `nix develop`")
        });
    if !out.status.success() {
        let log = std::fs::read_to_string(&log).unwrap_or_default();
        let tail: Vec<&str> = log.lines().rev().take(30).collect();
        panic!(
            "yosys {} failed:\n{}\n{}",
            ys.display(),
            tail.into_iter().rev().collect::<Vec<_>>().join("\n"),
            String::from_utf8_lossy(&out.stderr)
        );
    }
}

fn sources(dir: &Path) -> String {
    let mut files: Vec<String> = std::fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.extension().is_some_and(|e| e == "v"))
        .map(|p| p.display().to_string())
        .collect();
    files.sort();
    files.join(" ")
}

/// yosys' netlist of the design in `src` at `level`.
fn generate(src: &Path, top: &str, level: Level, dir: &Path) -> PathBuf {
    let json = dir.join("yosys.json");
    let srcs = sources(src);
    let lib = format!("read_verilog -lib {CORPUS}/lib/cy2_cells.v {CORPUS}/lib/device_cells.v");
    // `-icells`: `\$add` in Verilog is RTLIL's `$add`, not a module of that
    // name (rtlil_cells and simcells instantiate them directly).
    let (script, cwd) = match level {
        Level::Cyclone2 => {
            let synth = std::env::var("CHIPSMITH_CYCLONE2_SYNTH").expect(
                "CHIPSMITH_CYCLONE2_SYNTH (cyclone2-re's synth/) unset: run in `nix develop`",
            );
            (
                format!(
                    "read_verilog {srcs}\nscript synth/cyclone2.ys\nwrite_json {}\n",
                    json.display()
                ),
                PathBuf::from(synth),
            )
        }
        _ => {
            let passes = match level {
                Level::Rtl => format!("hierarchy -top {top}\nproc"),
                Level::Coarse => format!("synth -top {top} -run :fine"),
                _ => format!("synth -top {top} -run :fine\ntechmap\nopt"),
            };
            (
                format!(
                    "{lib}\nread_verilog -icells {srcs}\n{passes}\nwrite_json {}\n",
                    json.display()
                ),
                dir.to_owned(),
            )
        }
    };
    yosys(dir, "generate", &script, &cwd);
    json
}

/// yosys' own re-serialization of a netlist file, normalized.
fn canonical(dir: &Path, name: &str, json: &Path) -> Value {
    let out = dir.join(format!("{name}.canon.json"));
    yosys(
        dir,
        &format!("{name}.canon"),
        &format!(
            "read_json {}\nwrite_json {}\n",
            json.display(),
            out.display()
        ),
        dir,
    );
    let mut v: Value = serde_json::from_slice(&std::fs::read(out).unwrap()).unwrap();
    normalize(&mut v);
    v
}

/// What two equally-read designs can still differ in: the creator, and
/// flag parameters, which yosys writes as 1 or 32 bits depending on the
/// pass that made the cell (`$mem_v2`'s per-port flag vectors aside).
fn normalize(v: &mut Value) {
    v.as_object_mut().unwrap().remove("creator");
    for m in v["modules"].as_object_mut().unwrap().values_mut() {
        let Some(cells) = m.get_mut("cells").and_then(Value::as_object_mut) else {
            continue;
        };
        for c in cells.values_mut() {
            if c["type"] == "$mem_v2" {
                continue;
            }
            for (k, p) in c["parameters"].as_object_mut().unwrap() {
                if k.ends_with("_POLARITY") || k.ends_with("_SIGNED") {
                    *p = Value::Bool(p.as_str().is_some_and(|s| s.contains('1')));
                }
            }
        }
    }
}

/// The first place two JSON values differ.
fn first_difference(a: &Value, b: &Value, path: &str) -> Option<String> {
    match (a, b) {
        (Value::Object(x), Value::Object(y)) => {
            for (k, va) in x {
                match y.get(k) {
                    None => return Some(format!("{path}/{k}: only in yosys' file")),
                    Some(vb) => {
                        if let Some(d) = first_difference(va, vb, &format!("{path}/{k}")) {
                            return Some(d);
                        }
                    }
                }
            }
            y.keys()
                .find(|k| !x.contains_key(*k))
                .map(|k| format!("{path}/{k}: only in ours"))
        }
        _ if a == b => None,
        _ => Some(format!("{path}: {a} vs {b}")),
    }
}

/// yosys proves `ours` equivalent to `gold`.
fn assert_equivalent(dir: &Path, gold: &Path, ours: &Path, top: &str) {
    // `equiv_simple`/`equiv_induct` have no model of `$mem_v2`. A small
    // memory becomes flops (picorv32's register file, which feeds back
    // into its own address). A big one becomes an opaque cell, which
    // `equiv_struct -fwd` merges with its twin once their inputs are proven
    // equal: `memory_map` on the 4K-word ones takes minutes. Only forward
    // sweeps: the backward ones assume the two sides are the same structure,
    // which is what's being checked. Tri-state buffers have no model
    // either, and go the same way.
    let prep = format!(
        "hierarchy -top {top}\nproc\nflatten\nmemory -nomap\n\
         memory_map t:$mem_v2 r:SIZE<=64 %i\nchtype -set chipsmith_mem t:$mem_v2\n\
         chtype -set chipsmith_tribuf t:$tribuf t:$_TBUF_\nopt_clean"
    );
    let script = format!(
        "read_json {gold}\n{prep}\nrename {top} gold\ndesign -stash gold\n\
         read_json {ours}\n{prep}\nrename {top} gate\ndesign -stash gate\n\
         design -copy-from gold -as gold gold\ndesign -copy-from gate -as gate gate\n\
         equiv_make gold gate equiv\nhierarchy -top equiv\nasync2sync\n\
         equiv_struct -fwd\nequiv_simple\nequiv_induct -ignore-unknown-cells\n\
         equiv_status -assert\n",
        gold = gold.display(),
        ours = ours.display(),
    );
    yosys(dir, "equiv", &script, dir);
}

fn roundtrip(design: &str, top: &str, level: Level) -> chipsmith_netlist::Design {
    roundtrip_in(&Path::new(CORPUS).join(design), design, top, level)
}

fn roundtrip_in(src: &Path, design: &str, top: &str, level: Level) -> chipsmith_netlist::Design {
    let dir = scratch(design, level);
    let gold = generate(src, top, level, &dir);
    let text = std::fs::read(&gold).unwrap();

    let t = Instant::now();
    let ir = chipsmith_netlist::read_json(&text)
        .unwrap_or_else(|e| panic!("reading {}: {e}", gold.display()));
    let read = t.elapsed();
    ir.check().unwrap();
    let ours = dir.join("chipsmith.json");
    let t = Instant::now();
    chipsmith_netlist::write_json(&ir, std::fs::File::create(&ours).unwrap()).unwrap();
    let write = t.elapsed();
    let cells: usize = ir.modules.iter().map(|m| m.cells.len()).sum();
    eprintln!(
        "{design} {}: {cells} cells, {} KiB; read {:.1} ms, write {:.1} ms",
        level.name(),
        text.len() / 1024,
        read.as_secs_f64() * 1e3,
        write.as_secs_f64() * 1e3,
    );

    let again = chipsmith_netlist::read_json(&std::fs::read(&ours).unwrap()).unwrap();
    assert!(
        again == ir,
        "{design} {}: read-write-read changed the IR",
        level.name()
    );

    let (a, b) = (
        canonical(&dir, "yosys", &gold),
        canonical(&dir, "chipsmith", &ours),
    );
    if let Some(d) = first_difference(&a, &b, "") {
        panic!(
            "{design} {}: yosys reads our file differently: {d}",
            level.name()
        );
    }

    assert_equivalent(&dir, &gold, &ours, top);
    ir
}

/// Every cell of yosys' `simcells.v` (its gate-level library), instantiated
/// once, but the wide multiplexers (`$_MUX4_` ...): the IR leaves those
/// generic, and yosys' SAT passes have no model of them. Everything else
/// must come out typed.
#[test]
fn simcells() {
    let datdir = Command::new("yosys-config")
        .arg("--datdir")
        .output()
        .expect("yosys-config on PATH: run in `nix develop`");
    let datdir = String::from_utf8(datdir.stdout).unwrap();
    let lib = std::fs::read_to_string(Path::new(datdir.trim()).join("simcells.v")).unwrap();

    let (mut ins, mut outs) = (0, 0);
    let mut body = String::new();
    let mut cell: Option<(String, Vec<String>)> = None;
    let mut types = Vec::new();
    for line in lib.lines().map(str::trim) {
        if let Some(rest) = line.strip_prefix("module \\") {
            let name = rest.split_whitespace().next().unwrap().to_owned();
            cell = Some((name, Vec::new()));
        } else if let Some((name, conns)) = &mut cell {
            let decl = line
                .strip_prefix("input ")
                .map(|r| (r, true))
                .or_else(|| line.strip_prefix("output ").map(|r| (r, false)));
            if let Some((ports, input)) = decl {
                for p in ports
                    .trim_start_matches("reg ")
                    .trim_end_matches(';')
                    .split(',')
                {
                    let bit = if input {
                        ins += 1;
                        format!("i[{}]", ins - 1)
                    } else {
                        outs += 1;
                        format!("o[{}]", outs - 1)
                    };
                    conns.push(format!(".{}({bit})", p.trim()));
                }
            } else if line == "endmodule" && name.starts_with("$_MUX") && name.len() > 6 {
                cell = None;
            } else if line == "endmodule" {
                body += &format!("  \\{name} c{} ({});\n", types.len(), conns.join(", "));
                types.push(name.clone());
                cell = None;
            }
        }
    }
    assert_eq!(types.len(), 146, "simcells.v changed: check the new cells");
    let src = scratch("simcells", Level::Rtl).join("src");
    std::fs::create_dir_all(&src).unwrap();
    std::fs::write(
        src.join("top.v"),
        format!(
            "module top(input [{}:0] i, output [{}:0] o);\n{body}endmodule\n",
            ins - 1,
            outs - 1
        ),
    )
    .unwrap();

    let design = roundtrip_in(&src, "simcells", "top", Level::Rtl);
    let generic: Vec<String> = design
        .top()
        .unwrap()
        .cells
        .iter()
        .filter(|c| matches!(c.kind, chipsmith_netlist::CellKind::Other(_)))
        .map(|c| c.kind.type_name().into_owned())
        .collect();
    assert!(generic.is_empty(), "not typed: {generic:?}");
}

/// One test per design and level, so they run in parallel.
macro_rules! corpus {
    ($($module:ident: $dir:literal top $top:literal [$($level:ident),*];)*) => {
        $(
            mod $module {
                $(
                    #[test]
                    #[allow(non_snake_case)]
                    fn $level() {
                        super::roundtrip($dir, $top, super::Level::$level);
                    }
                )*
            }
        )*
    };
}

/// Every typed RTLIL cell, instantiated directly: none may stay generic.
#[test]
fn rtlil_cells() {
    let design = roundtrip("rtlil_cells", "top", Level::Rtl);
    let generic: Vec<_> = design
        .top()
        .unwrap()
        .cells
        .iter()
        .filter(|c| matches!(c.kind, chipsmith_netlist::CellKind::Other(_)))
        .map(|c| c.kind.type_name().into_owned())
        .collect();
    assert!(generic.is_empty(), "not typed: {generic:?}");
}

corpus! {
    blinky: "blinky-verilog" top "blinky" [Rtl, Coarse, Gates];
    bidir: "bidir" top "top" [Rtl, Coarse, Gates, Cyclone2];
    picorv32: "picorv32" top "top" [Rtl, Coarse, Gates, Cyclone2];
    ram: "ram" top "top" [Rtl, Coarse, Gates, Cyclone2];
    ram_leds: "ram-leds" top "top" [Rtl, Coarse, Gates, Cyclone2];
    ram_widths: "ram_widths" top "top" [Rtl, Coarse, Gates, Cyclone2];
    zybo_blinky: "zybo-blinky" top "blinky" [Rtl, Coarse, Gates];
    zybo_jtag_counter: "zybo-jtag-counter" top "jtag_counter" [Rtl, Coarse, Gates];
    loader: "loader" top "top" [Rtl, Coarse, Gates, Cyclone2];
    specimen_cnt: "specimen-cnt-c20" top "top" [Rtl, Coarse, Gates, Cyclone2];
    specimen_de2: "specimen-de2-d1" top "top" [Rtl, Coarse, Gates, Cyclone2];
    specimen_empty: "specimen-empty" top "top" [Rtl, Coarse, Gates, Cyclone2];
    specimen_io: "specimen-io-roles2" top "top" [Rtl, Coarse, Gates, Cyclone2];
    specimen_oe: "specimen-oe-oe1" top "top" [Rtl, Coarse, Gates, Cyclone2];
    specimen_lab: "specimen-lab-l1" top "top" [Rtl, Coarse, Gates];
    specimen_lut_free: "specimen-lut_free" top "top" [Rtl, Coarse, Gates];
    specimen_route: "specimen-route-r4" top "top" [Rtl, Coarse, Gates];
}
