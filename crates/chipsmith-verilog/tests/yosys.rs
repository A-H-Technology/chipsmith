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

mod common;

use std::path::{Path, PathBuf};
use std::process::Command;

use common::{check, netlist_corpus, repo, scratch, Case, Lib, MANIFEST};

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

/// The check has teeth: one constant bit flipped in chipsmith's netlist
/// (the counter's increment in bidir, `c + 1'b1` becoming `c + 0`) and
/// the proof fails.
#[test]
fn a_one_bit_miscompile_is_caught() {
    use chipsmith_netlist::{BinaryOp, CellKind, SigBit};
    let case = example("bidir", "top", Lib::Cyclone2);
    let dir = scratch("mutation");
    let libs = common::lib_files(case.lib);
    let mut fe = chipsmith_verilog::Frontend::new();
    for l in &libs {
        fe.read_library(l).unwrap();
    }
    for f in &case.files {
        fe.read_file(f).unwrap();
    }
    let mut design = fe.elaborate(&case.top).unwrap();
    chipsmith_synth::flatten::flatten(&mut design).unwrap();
    let top = &mut design.modules[0];
    let add = top
        .cells
        .iter_mut()
        .find_map(|c| match &mut c.kind {
            CellKind::Binary(b) if b.op == BinaryOp::Add => Some(b),
            _ => None,
        })
        .expect("bidir's counter has an adder");
    assert_eq!(add.b.sig[0], SigBit::ONE);
    add.b.sig[0] = SigBit::ZERO;
    let ours = dir.join("ours.json");
    chipsmith_netlist::write_json(&design, std::fs::File::create(&ours).unwrap()).unwrap();
    // Gold as check() makes it.
    let gold = dir.join("gold.json");
    let mut script = String::new();
    for l in &libs {
        script += &format!("read_verilog -lib {}\n", common::quoted(l));
    }
    for f in &case.files {
        script += &format!("read_verilog {}\n", common::quoted(f));
    }
    script += &format!(
        "hierarchy -check -top top\nproc\nflatten\nopt_clean\nwrite_json {}\n",
        common::quoted(&gold)
    );
    common::yosys(&dir, "gold", &script).unwrap();
    let r = common::equivalent(&dir, &gold, &ours, "top");
    let err = r.expect_err("a flipped bit must not prove equivalent");
    assert!(err.contains("unproven"), "{err}");
}
