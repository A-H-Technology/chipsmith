//! chipsmith's front end against yosys' (ADR-0004: yosys is the oracle).
//!
//! For each corpus design, yosys elaborates it (`read_verilog; hierarchy
//! -check; proc; flatten; opt_clean`) and so does chipsmith (then
//! `chipsmith_synth::flatten`); yosys proves the two sequentially
//! equivalent with `equiv_make`/`equiv_simple`/`equiv_induct`, as
//! chipsmith-netlist's tests/yosys.rs does for its round trips.
//!
//! Needs yosys and ghdl on PATH: run in the devshell. The cyclone2-re
//! specimens are checked if `CYCLONE2_RE` points at a checkout.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Instant;

const MANIFEST: &str = env!("CARGO_MANIFEST_DIR");

fn repo() -> PathBuf {
    Path::new(MANIFEST).join("../..")
}

fn netlist_corpus() -> PathBuf {
    repo().join("crates/chipsmith-netlist/tests/corpus")
}

fn scratch(name: &str) -> PathBuf {
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR"))
        .join("verilog-equiv")
        .join(name);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// Runs a yosys script; the end of its log if it fails.
fn yosys(dir: &Path, name: &str, script: &str) -> Result<(), String> {
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

fn yosys_datdir() -> PathBuf {
    let out = Command::new("yosys-config")
        .arg("--datdir")
        .output()
        .expect("yosys-config on PATH: run in `nix develop`");
    PathBuf::from(String::from_utf8(out.stdout).unwrap().trim())
}

/// The cell libraries a design is read with.
#[derive(Clone, Copy)]
enum Lib {
    /// cyclone2-re's synth/cells_sim.v and Quartus' cycloneii_* cells.
    Cyclone2,
    /// yosys' 7-series cells.
    Xilinx,
}

fn lib_files(lib: Lib) -> Vec<PathBuf> {
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

fn quoted(p: &Path) -> String {
    format!("\"{}\"", p.display())
}

pub struct Case {
    pub name: String,
    pub files: Vec<PathBuf>,
    pub top: String,
    pub lib: Lib,
}

/// Elaborates with both front ends and proves the results equivalent.
fn check(case: &Case) -> Result<(), String> {
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
fn equivalent(dir: &Path, gold: &Path, ours: &Path, top: &str) -> Result<(), String> {
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

fn example(name: &str, top: &str, lib: Lib) -> Case {
    let src = repo().join("examples").join(name).join("src");
    let mut files: Vec<PathBuf> = std::fs::read_dir(&src)
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.extension().is_some_and(|e| e == "v"))
        .collect();
    files.sort();
    Case {
        name: name.to_owned(),
        files,
        top: top.to_owned(),
        lib,
    }
}

/// GHDL's Verilog for a VHDL design, as chipsmith's flow makes it
/// (`ghdl --synth --std=08 --out=verilog`, flow.rs `verilog_sources`).
fn ghdl(name: &str, files: &[PathBuf], top: &str) -> Case {
    let dir = scratch(name);
    let out = Command::new("ghdl")
        .args(["--synth", "--std=08", "--out=verilog"])
        .args(files)
        .args(["-e", top])
        .current_dir(&dir)
        .output()
        .expect("ghdl on PATH: run in `nix develop`");
    assert!(
        out.status.success(),
        "ghdl failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let v = dir.join(format!("{top}.v"));
    std::fs::write(&v, &out.stdout).unwrap();
    Case {
        name: name.to_owned(),
        files: vec![v],
        top: top.to_owned(),
        lib: Lib::Cyclone2,
    }
}

fn run(case: Case) {
    if let Err(e) = check(&case) {
        panic!("{}: {e}", case.name);
    }
}

macro_rules! examples {
    ($($test:ident: $dir:literal top $top:literal $lib:ident;)*) => {
        $(
            #[test]
            fn $test() {
                run(example($dir, $top, Lib::$lib));
            }
        )*
    };
}

examples! {
    blinky_verilog: "blinky-verilog" top "blinky" Cyclone2;
    bidir: "bidir" top "top" Cyclone2;
    picorv32: "picorv32" top "top" Cyclone2;
    ram: "ram" top "top" Cyclone2;
    ram_leds: "ram-leds" top "top" Cyclone2;
    ram_widths: "ram_widths" top "top" Cyclone2;
    zybo_blinky: "zybo-blinky" top "blinky" Xilinx;
    zybo_jtag_counter: "zybo-jtag-counter" top "jtag_counter" Xilinx;
}

#[test]
fn loader() {
    run(Case {
        name: "loader".into(),
        files: vec![repo().join("crates/chipsmith-cyclone2/src/persist/loader/top.v")],
        top: "top".into(),
        lib: Lib::Cyclone2,
    });
}

#[test]
fn blinky_vhdl() {
    let src = repo().join("examples/blinky-vhdl/src/blinky.vhd");
    run(ghdl("blinky-vhdl", &[src], "blinky"));
}

/// VHDL that exercises more of GHDL's Verilog idioms than blinky:
/// enumerated state, case, an asynchronous reset, a RAM, tri-states.
#[test]
fn ghdl_idioms() {
    let src = Path::new(MANIFEST).join("tests/vhdl");
    for (file, top) in [("fsm.vhd", "fsm"), ("rom.vhd", "rom")] {
        run(ghdl(&format!("ghdl-{top}"), &[src.join(file)], top));
    }
}

/// The specimens chipsmith-netlist's tests carry from cyclone2-re.
#[test]
fn netlist_corpus_specimens() {
    let corpus = netlist_corpus();
    let mut failures = Vec::new();
    for e in std::fs::read_dir(&corpus).unwrap() {
        let p = e.unwrap().path();
        let name = p.file_name().unwrap().to_string_lossy().into_owned();
        if !name.starts_with("specimen-") {
            continue;
        }
        let case = Case {
            name: name.clone(),
            files: vec![p.join("top.v")],
            top: "top".into(),
            lib: Lib::Cyclone2,
        };
        if let Err(e) = check(&case) {
            failures.push(format!("{name}: {e}"));
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n\n"));
}

/// Every synthesizable specimen in a cyclone2-re checkout (`CYCLONE2_RE`).
#[test]
fn cyclone2_re_specimens() {
    let Ok(root) = std::env::var("CYCLONE2_RE") else {
        eprintln!("CYCLONE2_RE unset: skipping cyclone2-re's specimens");
        return;
    };
    let mut files = Vec::new();
    let mut stack = vec![Path::new(&root).join("specimens")];
    while let Some(d) = stack.pop() {
        for e in std::fs::read_dir(&d).unwrap() {
            let p = e.unwrap().path();
            if p.is_dir() {
                stack.push(p);
            } else if p.extension().is_some_and(|e| e == "v") {
                files.push(p);
            }
        }
    }
    files.sort();
    let mut failures = Vec::new();
    let n = files.len();
    for f in files {
        let rel = f.strip_prefix(&root).unwrap().display().to_string();
        let case = Case {
            name: rel.replace('/', "_"),
            files: vec![f],
            top: "top".into(),
            lib: Lib::Cyclone2,
        };
        if let Err(e) = check(&case) {
            failures.push(format!("{rel}: {e}"));
        }
    }
    eprintln!("{} of {n} specimens equivalent", n - failures.len());
    assert!(failures.is_empty(), "{}", failures.join("\n\n"));
}
