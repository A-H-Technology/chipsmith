//! Shared by the yosys-oracle tests: running yosys, elaborating with both
//! front ends, and proving the results equivalent.

#![allow(dead_code)]

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Instant;

pub const MANIFEST: &str = env!("CARGO_MANIFEST_DIR");

pub fn repo() -> PathBuf {
    Path::new(MANIFEST).join("../..")
}

pub fn netlist_corpus() -> PathBuf {
    repo().join("crates/chipsmith-netlist/tests/corpus")
}

pub fn scratch(name: &str) -> PathBuf {
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR"))
        .join("verilog-equiv")
        .join(name);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// Runs a yosys script; the end of its log if it fails.
pub fn yosys(dir: &Path, name: &str, script: &str) -> Result<(), String> {
    let ys = dir.join(format!("{name}.ys"));
    let log = dir.join(format!("{name}.log"));
    std::fs::write(&ys, script).unwrap();
    let out = Command::new("yosys")
        .arg("-q")
        .arg("-l")
        .arg(&log)
        .arg("-s")
        .arg(&ys)
        .current_dir(dir)
        .output()
        .unwrap_or_else(|e| {
            panic!("can't run yosys ({e}); these tests need it on PATH: run them in `nix develop`")
        });
    if out.status.success() {
        return Ok(());
    }
    let log = std::fs::read_to_string(&log).unwrap_or_default();
    let tail: Vec<&str> = log.lines().rev().take(25).collect();
    Err(format!(
        "yosys {} failed:\n{}\n{}",
        ys.display(),
        tail.into_iter().rev().collect::<Vec<_>>().join("\n"),
        String::from_utf8_lossy(&out.stderr)
    ))
}

pub fn yosys_datdir() -> PathBuf {
    let out = Command::new("yosys-config")
        .arg("--datdir")
        .output()
        .expect("yosys-config on PATH: run in `nix develop`");
    PathBuf::from(String::from_utf8(out.stdout).unwrap().trim())
}

/// The cell libraries a design is read with.
#[derive(Clone, Copy)]
pub enum Lib {
    /// cyclone2-re's synth/cells_sim.v and Quartus' cycloneii_* cells.
    Cyclone2,
    /// yosys' 7-series cells.
    Xilinx,
}

pub fn lib_files(lib: Lib) -> Vec<PathBuf> {
    match lib {
        Lib::Cyclone2 => {
            let synth = std::env::var("CHIPSMITH_CYCLONE2_SYNTH").expect(
                "CHIPSMITH_CYCLONE2_SYNTH (cyclone2-re's synth/) unset: run in `nix develop`",
            );
            vec![
                Path::new(&synth).join("synth/cells_sim.v"),
                netlist_corpus().join("lib/device_cells.v"),
            ]
        }
        // cells_xtra.v has the blackboxes (BSCANE2, PS7, ...); yosys keeps
        // cells_sim.v's models as whiteboxes even with -lib.
        Lib::Xilinx => vec![yosys_datdir().join("xilinx/cells_xtra.v")],
    }
}

pub fn quoted(p: &Path) -> String {
    format!("\"{}\"", p.display())
}

pub struct Case {
    pub name: String,
    pub files: Vec<PathBuf>,
    pub top: String,
    pub lib: Lib,
}

/// Elaborates with both front ends and proves the results equivalent.
pub fn check(case: &Case) -> Result<(), String> {
    let dir = scratch(&case.name);
    let libs = lib_files(case.lib);
    let gold = dir.join("gold.json");
    let ours = dir.join("ours.json");
    // yosys' xilinx cells_sim.v has simulation models: -lib keeps only
    // their interfaces, as chipsmith's read_library does.
    let mut script = String::new();
    for l in &libs {
        script += &format!("read_verilog -lib {}\n", quoted(l));
    }
    for f in &case.files {
        script += &format!("read_verilog {}\n", quoted(f));
    }
    script += &format!(
        "hierarchy -check -top {}\nproc\nflatten\nopt_clean\nwrite_json {}\n",
        case.top,
        quoted(&gold)
    );
    yosys(&dir, "gold", &script)?;

    let t = Instant::now();
    let mut fe = chipsmith_verilog::Frontend::new();
    for l in &libs {
        fe.read_library(l).map_err(|e| e.to_string())?;
    }
    let read_libs = t.elapsed();
    for f in &case.files {
        fe.read_file(f).map_err(|e| e.to_string())?;
    }
    let mut design = fe.elaborate(&case.top).map_err(|e| e.to_string())?;
    let elaborated = t.elapsed() - read_libs;
    chipsmith_synth::flatten::flatten(&mut design).map_err(|e| e.to_string())?;
    chipsmith_netlist::write_json(&design, std::fs::File::create(&ours).unwrap())
        .map_err(|e| e.to_string())?;
    eprintln!(
        "{}: elaborated in {:.1} ms (+{:.1} ms reading the cell libraries)",
        case.name,
        elaborated.as_secs_f64() * 1e3,
        read_libs.as_secs_f64() * 1e3
    );
    equivalent(&dir, &gold, &ours, &case.top)
}

/// The equivalence script of chipsmith-netlist's tests/yosys.rs, with
/// `tribuf` added: yosys' front end leaves `? :` with `z` as a `$mux`,
/// chipsmith makes it the `$tribuf` the `tribuf` pass would.
/// Both netlists carry the blackbox modules they use.
pub fn equivalent(dir: &Path, gold: &Path, ours: &Path, top: &str) -> Result<(), String> {
    let prep = format!(
        "hierarchy -top {top}\nproc\nflatten\ntribuf\nmemory -nomap\n\
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
        gold = quoted(gold),
        ours = quoted(ours),
    );
    yosys(dir, "equiv", &script)
}

