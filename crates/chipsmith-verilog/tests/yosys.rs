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

use std::collections::BTreeMap;
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
    for (file, top) in [("fsm.vhd", "fsm"), ("rom.vhd", "rom"), ("ops.vhd", "ops")] {
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

/// Cell counts after cyclone2-re's `cyclone2.ys`, run on yosys' front end
/// and on chipsmith's netlist (read with `read_json`, its blackbox modules
/// dropped for the script's own library).
fn cyclone2_counts(case: &Case) -> (BTreeMap<String, usize>, BTreeMap<String, usize>) {
    let synth = std::env::var("CHIPSMITH_CYCLONE2_SYNTH")
        .expect("CHIPSMITH_CYCLONE2_SYNTH unset: run in `nix develop`");
    let dir = scratch(&format!("cyclone2-{}", case.name));
    let libs = common::lib_files(Lib::Cyclone2);
    let mut fe = chipsmith_verilog::Frontend::new();
    for l in &libs {
        fe.read_library(l).unwrap();
    }
    for f in &case.files {
        fe.read_file(f).unwrap();
    }
    let design = fe.elaborate(&case.top).unwrap();
    let ours = dir.join("ours.json");
    chipsmith_netlist::write_json(&design, std::fs::File::create(&ours).unwrap()).unwrap();
    let rename = if case.top == "top" {
        String::new()
    } else {
        format!("rename {} top\n", case.top)
    };
    let mut read_gold = String::new();
    for f in &case.files {
        read_gold += &format!("read_verilog {}\n", common::quoted(f));
    }
    let count = |name: &str, read: &str| -> BTreeMap<String, usize> {
        let stat = dir.join(format!("{name}.stat"));
        let script = format!(
            "{read}{rename}script synth/cyclone2.ys\ntee -q -o {} stat\n",
            common::quoted(&stat)
        );
        let ys = dir.join(format!("{name}.ys"));
        std::fs::write(&ys, script).unwrap();
        let out = Command::new("yosys")
            .args(["-q", "-s"])
            .arg(&ys)
            .current_dir(Path::new(&synth))
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{name}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        std::fs::read_to_string(&stat)
            .unwrap()
            .lines()
            .filter_map(|l| {
                let mut w = l.split_whitespace();
                let n = w.next()?.parse().ok()?;
                let ty = w.next()?;
                // Device cells only (`stat` also counts wires, ports, ...).
                let device = ty.starts_with(|c: char| c.is_ascii_uppercase());
                (w.next().is_none() && device).then(|| (ty.to_owned(), n))
            })
            .collect()
    };
    let gold = count("gold", &read_gold);
    let mine = count(
        "ours",
        &format!("read_json {}\ndelete A:blackbox\n", common::quoted(&ours)),
    );
    (gold, mine)
}

/// The rest of the DE2 flow sees the same netlist: the same device cells
/// as from yosys' front end. picorv32's LUT count differs by a little
/// (ADR-0006: abc maps a slightly different, equivalent input); its
/// registers, pins and memories don't.
#[test]
fn cyclone2_cell_counts_match_yosys_front_end() {
    let mut cases: Vec<Case> = [
        "blinky-verilog",
        "bidir",
        "ram",
        "ram-leds",
        "ram_widths",
        "picorv32",
    ]
    .iter()
    .map(|n| {
        example(
            n,
            if *n == "blinky-verilog" {
                "blinky"
            } else {
                "top"
            },
            Lib::Cyclone2,
        )
    })
    .collect();
    cases.push(Case {
        name: "loader".into(),
        files: vec![repo().join("crates/chipsmith-cyclone2/src/persist/loader/top.v")],
        top: "top".into(),
        lib: Lib::Cyclone2,
    });
    cases.push(ghdl(
        "cyclone2-blinky-vhdl",
        &[repo().join("examples/blinky-vhdl/src/blinky.vhd")],
        "blinky",
    ));
    let mut report = Vec::new();
    for case in &cases {
        let (gold, ours) = cyclone2_counts(case);
        eprintln!(
            "{}: yosys {gold:?}\n{}: ours  {ours:?}",
            case.name, case.name
        );
        if case.name == "picorv32" {
            for (ty, &n) in &gold {
                let m = ours.get(ty).copied().unwrap_or(0);
                if ty == "LCELL_COMB" {
                    assert!(m.abs_diff(n) * 100 <= n, "picorv32 LUTs: {m} vs yosys' {n}");
                } else {
                    assert_eq!(m, n, "picorv32 {ty}");
                }
            }
        } else if gold != ours {
            report.push(format!("{}: yosys {gold:?}, ours {ours:?}", case.name));
        }
    }
    assert!(report.is_empty(), "{}", report.join("\n"));
}
