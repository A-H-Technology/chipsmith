//! chipsmith's 7-series synthesis against the RTL it starts from, with
//! yosys as the oracle (ADR-0004): the Cyclone II synthesis tests' check
//! (`support::check`: the RTL against the refined stage, then that against
//! the netlist, registers synthesis found constant proven so on the RTL),
//! the netlist's primitives read through yosys' own models of them
//! (`techlibs/xilinx/cells_sim.v`). Memories in RAM32Ms are checked by
//! co-simulation against the RTL (`support::cosim`), and the rest of such a
//! design by the proof with distributed RAM off. The cell counts are
//! printed beside `synth_xilinx`'s for the same Verilog. Needs yosys and
//! iverilog: run in the devshell.

#[path = "../../chipsmith-cyclone2/tests/support/mod.rs"]
mod support;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;

use chipsmith_netlist::{CellKind, Design, Dir, Module, SigBit, Wire};
use chipsmith_xc7::synth::{cells, synthesize_observed, Observer, Options};

const ROOT: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../..");

fn yosys_share() -> PathBuf {
    let out = Command::new("yosys-config")
        .arg("--datdir")
        .output()
        .expect("yosys-config: run in the devshell");
    PathBuf::from(String::from_utf8(out.stdout).unwrap().trim())
}

fn setup() {
    let sim = std::fs::read_to_string(yosys_share().join("xilinx/cells_sim.v")).unwrap();
    support::use_lib(support::Lib {
        name: cells::LIBRARY_NAME,
        library: cells::LIBRARY,
        sim: cells::yosys_sim_without_models(&sim),
        models: cells::MODELS,
    });
}

#[derive(Default)]
struct Stages {
    refined: Option<Design>,
    cells: Option<Design>,
}

impl Observer for Stages {
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

struct Synthesized {
    dir: PathBuf,
    rtl: PathBuf,
    refined: Design,
    cells: Design,
}

fn synthesize(name: &str, top: &str, files: &[PathBuf], options: Options) -> Synthesized {
    setup();
    let dir = support::scratch(Path::new(env!("CARGO_TARGET_TMPDIR")), name);
    let rtl = support::rtl(&dir, top, files);
    let design = support::frontend(top, files);
    let mut stages = Stages::default();
    synthesize_observed(design, options, &mut stages).unwrap_or_else(|e| panic!("{name}: {e:#}"));
    Synthesized {
        dir,
        rtl,
        refined: stages.refined.expect("a refined stage"),
        cells: stages.cells.expect("a cells stage"),
    }
}

/// Replaces every cell of type `ty` (a hard block neither side models) by
/// ports, its outputs in and its inputs out (`__<cell>_<port>`), so the
/// check compares the logic around it.
fn cut_cells(m: &mut Module, ty: &str) {
    let mut ports = Vec::new();
    let mut subst = chipsmith_synth::rewrite::Subst::new(m);
    let mut keep = Vec::new();
    for c in std::mem::take(&mut m.cells) {
        let CellKind::Other(g) = &c.kind else {
            keep.push(c);
            continue;
        };
        if g.ty != ty {
            keep.push(c);
            continue;
        }
        let tag: String = c
            .name
            .trim_start_matches('\\')
            .chars()
            .map(|ch| if ch.is_ascii_alphanumeric() { ch } else { '_' })
            .collect();
        for p in &g.ports {
            let name = format!("__{tag}_{}", p.name);
            if p.dir == Some(Dir::Out) {
                let fresh: Vec<SigBit> = p.sig.iter().map(|_| SigBit::Net(m.new_net())).collect();
                for (old, new) in p.sig.iter().zip(&fresh) {
                    if let SigBit::Net(n) = old {
                        subst.set(*n, *new);
                    }
                }
                ports.push((name, Dir::In, fresh));
            } else {
                ports.push((name, Dir::Out, p.sig.clone()));
            }
        }
    }
    m.cells = keep;
    for (name, dir, bits) in ports {
        let mut w = Wire::new(name, bits);
        w.port = Some(dir);
        m.wires.push(w);
    }
    subst.apply(m);
}

fn cut_all(d: &mut Design, ty: &str) {
    for m in d.modules.iter_mut().filter(|m| !m.is_blackbox()) {
        cut_cells(m, ty);
    }
}

/// OBUFT and IOBUF as the tri-states they are (UG953: T high releases the
/// pin, IOBUF's O reads it), which the check cuts at the pins as it does
/// the RTL's (`support::cut_pins`); the yosys models' tri-states would be
/// opaque cells, merged only once their enables were proven equal, which
/// with T inverted they never are structurally.
fn tristates(d: &mut Design) {
    use chipsmith_netlist::{Gate, GateOp, Tribuf};
    let m = d
        .modules
        .iter_mut()
        .find(|m| !m.is_blackbox())
        .expect("top");
    let mut subst = chipsmith_synth::rewrite::Subst::new(m);
    let mut out = Vec::new();
    let cells = std::mem::take(&mut m.cells);
    for c in cells {
        let CellKind::Other(g) = &c.kind else {
            out.push(c);
            continue;
        };
        let pin = |p: &str| g.port(p).map(|p| p.sig[0]).expect("a buffer's pins");
        let (data, t, pad) = match g.ty.as_str() {
            "OBUFT" => (pin("I"), pin("T"), pin("O")),
            "IOBUF" => {
                if let SigBit::Net(o) = pin("O") {
                    subst.set(o, pin("IO"));
                }
                (pin("I"), pin("T"), pin("IO"))
            }
            _ => {
                out.push(c);
                continue;
            }
        };
        let en = SigBit::Net(m.new_net());
        out.push(chipsmith_netlist::Cell {
            name: format!("{}$en", c.name),
            kind: CellKind::Gate(Gate::new(GateOp::Not, &[t], en).unwrap()),
            attributes: Default::default(),
        });
        out.push(chipsmith_netlist::Cell {
            name: c.name.clone(),
            kind: CellKind::Tribuf(Tribuf {
                a: vec![data],
                en,
                y: vec![pad],
            }),
            attributes: Default::default(),
        });
    }
    m.cells = out;
    subst.apply(m);
}

/// The proof, hard blocks `cut` out on all three sides.
fn prove(name: &str, s: &Synthesized, cut: &[&str]) -> support::Proof {
    let (mut rtl, mut refined, mut gate) =
        (support::read(&s.rtl), s.refined.clone(), s.cells.clone());
    tristates(&mut gate);
    let rtl_path = if cut.is_empty() {
        s.rtl.clone()
    } else {
        for ty in cut {
            cut_all(&mut rtl, ty);
            cut_all(&mut refined, ty);
            cut_all(&mut gate, ty);
        }
        let p = s.dir.join("rtl_cut.json");
        support::write(&rtl, &p);
        p
    };
    support::check(&s.dir, &rtl_path, &refined, &gate).unwrap_or_else(|e| panic!("{name}: {e}"))
}

/// The netlist's cells by type.
fn counts(d: &Design) -> BTreeMap<String, usize> {
    let mut out = BTreeMap::new();
    for c in &support::top(d).cells {
        *out.entry(c.kind.type_name().to_string()).or_default() += 1;
    }
    out
}

/// `synth_xilinx`'s cells for the same Verilog, as the build ran it before
/// chipsmith's synthesis.
fn synth_xilinx(dir: &Path, top: &str, files: &[PathBuf]) -> BTreeMap<String, usize> {
    let json = dir.join("synth_xilinx.json");
    let reads: String = files
        .iter()
        .map(|f| format!("read_verilog \"{}\"\n", f.display()))
        .collect();
    support::yosys(
        dir,
        "synth_xilinx",
        &format!(
            "{reads}synth_xilinx -flatten -abc9 -arch xc7 -top {top}\nwrite_json {}\n",
            json.display()
        ),
        dir,
    )
    .unwrap_or_else(|e| panic!("{e}"));
    counts(&support::read(&json))
}

/// Prints chipsmith's counts beside synth_xilinx's: LUTs, registers,
/// CARRY4s, RAM32Ms.
fn compare(name: &str, ours: &BTreeMap<String, usize>, theirs: &BTreeMap<String, usize>) {
    let sum = |m: &BTreeMap<String, usize>, tys: &[&str]| -> usize {
        tys.iter().map(|t| m.get(*t).copied().unwrap_or(0)).sum()
    };
    let luts = ["LUT1", "LUT2", "LUT3", "LUT4", "LUT5", "LUT6", "INV"];
    let ffs = ["FDRE", "FDSE", "FDCE", "FDPE"];
    eprintln!(
        "{name}: LUTs {} (synth_xilinx {}), registers {} ({}), CARRY4 {} ({}), RAM32M {} ({}); \
         chipsmith {ours:?}; synth_xilinx {theirs:?}",
        sum(ours, &luts),
        sum(theirs, &luts),
        sum(ours, &ffs),
        sum(theirs, &ffs),
        sum(ours, &["CARRY4"]),
        sum(theirs, &["CARRY4"]),
        sum(ours, &["RAM32M"]),
        sum(theirs, &["RAM32M"]),
    );
}

fn example(dir: &str, files: &[&str]) -> Vec<PathBuf> {
    files
        .iter()
        .map(|f| {
            Path::new(ROOT)
                .join("examples")
                .join(dir)
                .join("src")
                .join(f)
        })
        .collect()
}

fn design(file: &str) -> Vec<PathBuf> {
    vec![Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/designs")
        .join(file)]
}

#[test]
fn zybo_blinky() {
    let files = example("zybo-blinky", &["blinky.v"]);
    let s = synthesize("zybo_blinky", "blinky", &files, Options::default());
    compare(
        "zybo-blinky",
        &counts(&s.cells),
        &synth_xilinx(&s.dir, "blinky", &files),
    );
    let p = prove("zybo_blinky", &s, &[]);
    assert!(p.equivalences > 0);
}

#[test]
fn zybo_jtag_counter() {
    let files = example("zybo-jtag-counter", &["jtag_counter.v"]);
    let s = synthesize(
        "zybo_jtag_counter",
        "jtag_counter",
        &files,
        Options::default(),
    );
    compare(
        "zybo-jtag-counter",
        &counts(&s.cells),
        &synth_xilinx(&s.dir, "jtag_counter", &files),
    );
    prove("zybo_jtag_counter", &s, &["BSCANE2"]);
}

/// Every register shape the slice has (CE, sync and async reset and set,
/// both clock edges, power-up values) and adders, subtractors and
/// comparisons of several widths, carry-in from logic included.
#[test]
fn registers_and_arithmetic() {
    let files = design("regs_arith.v");
    let s = synthesize("regs_arith", "regs_arith", &files, Options::default());
    let c = counts(&s.cells);
    compare(
        "regs_arith",
        &c,
        &synth_xilinx(&s.dir, "regs_arith", &files),
    );
    for ty in ["FDRE", "FDSE", "FDCE", "FDPE", "CARRY4"] {
        assert!(c.contains_key(ty), "no {ty}: {c:?}");
    }
    prove("regs_arith", &s, &[]);
}

/// Tri-state outputs and a bidirectional bus.
#[test]
fn pins() {
    let files = design("pins.v");
    let s = synthesize("pins", "pins", &files, Options::default());
    let c = counts(&s.cells);
    for ty in ["IBUF", "OBUF", "OBUFT", "IOBUF"] {
        assert!(c.contains_key(ty), "no {ty}: {c:?}");
    }
    prove("pins", &s, &[]);
}

/// Memories of the RAM32M's shapes: one, two and three read ports, odd
/// widths, shallower than 32 words, contents given, a falling-edge write.
#[test]
fn distributed_ram() {
    let files = design("lutram.v");
    let s = synthesize("lutram", "lutram", &files, Options::default());
    let c = counts(&s.cells);
    compare("lutram", &c, &synth_xilinx(&s.dir, "lutram", &files));
    assert!(c.get("RAM32M").copied().unwrap_or(0) >= 4, "{c:?}");
    for seed in 1..=3 {
        let r = support::cosim(&s.dir, &s.rtl, &s.refined, &s.cells, 3000, seed)
            .unwrap_or_else(|e| panic!("lutram, seed {seed}: {e}"));
        assert!(r.compared > 0);
    }
    let logic = synthesize(
        "lutram_logic",
        "lutram",
        &files,
        Options {
            distributed_ram: false,
            block_ram: false,
        },
    );
    prove("lutram_logic", &logic, &[]);
}

/// Every block RAM shape (`tests/designs/bram`, one memory each, its ports
/// on the design's): synthesized into RAMB18E1/RAMB36E1 (but `tiny`,
/// which stays in logic as Vivado keeps it) and co-simulated against the
/// RTL, the blocks as `chipsmith_xc7::synth::cells::MODELS`, three seeds.
#[test]
fn block_ram_shapes() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/designs/bram");
    let mut names: Vec<String> = std::fs::read_dir(&dir)
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.extension().is_some_and(|x| x == "v"))
        .map(|p| p.file_stem().unwrap().to_string_lossy().into_owned())
        .collect();
    names.sort();
    if let Ok(only) = std::env::var("CHIPSMITH_BRAM_SHAPE") {
        names.retain(|n| *n == only);
    }
    let mut failures = Vec::new();
    for name in &names {
        let files = vec![dir.join(format!("{name}.v"))];
        let s = synthesize(&format!("bram_{name}"), name, &files, Options::default());
        let c = counts(&s.cells);
        let blocks =
            c.get("RAMB18E1").copied().unwrap_or(0) + c.get("RAMB36E1").copied().unwrap_or(0);
        let mut compared = 0;
        let mut failed = None;
        for seed in 1..=3 {
            match support::cosim(&s.dir, &s.rtl, &s.refined, &s.cells, 3000, seed) {
                Ok(r) => compared += r.compared,
                Err(e) => {
                    failed = Some(format!("{name}, seed {seed}: {e}"));
                    break;
                }
            }
        }
        eprintln!("{name}: {c:?}, {compared} output bits compared");
        if let Some(f) = failed {
            failures.push(f);
        } else if (blocks == 0) != (name == "tiny") {
            failures.push(format!("{name}: {blocks} blocks"));
        } else if compared == 0 {
            failures.push(format!("{name}: nothing compared"));
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

/// Every multiplier shape (`tests/designs/dsp`): synthesized onto
/// DSP48E1s and co-simulated against the RTL (yosys' DSP48E1 model), three
/// seeds; the extremes of every operand come first.
#[test]
fn dsp_shapes() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/designs/dsp");
    let mut names: Vec<String> = std::fs::read_dir(&dir)
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.extension().is_some_and(|x| x == "v"))
        .map(|p| p.file_stem().unwrap().to_string_lossy().into_owned())
        .collect();
    names.sort();
    if let Ok(only) = std::env::var("CHIPSMITH_DSP_SHAPE") {
        names.retain(|n| *n == only);
    }
    let mut failures = Vec::new();
    for name in &names {
        let files = vec![dir.join(format!("{name}.v"))];
        let s = synthesize(&format!("dsp_{name}"), name, &files, Options::default());
        let c = counts(&s.cells);
        let mut compared = 0;
        let mut failed = None;
        for seed in 1..=3 {
            match support::cosim(&s.dir, &s.rtl, &s.refined, &s.cells, 3000, seed) {
                Ok(r) => compared += r.compared,
                Err(e) => {
                    failed = Some(format!("{name}, seed {seed}: {e}"));
                    break;
                }
            }
        }
        eprintln!("{name}: {c:?}, {compared} output bits compared");
        if let Some(f) = failed {
            failures.push(f);
        } else if !c.contains_key("DSP48E1") {
            failures.push(format!("{name}: no DSP48E1"));
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

fn picorv32() -> Vec<PathBuf> {
    example("picorv32", &["picorv32.v", "top.v"])
}

#[test]
fn zybo_picorv32() {
    let files = picorv32();
    let s = synthesize("zybo_picorv32", "top", &files, Options::default());
    compare(
        "zybo-picorv32",
        &counts(&s.cells),
        &synth_xilinx(&s.dir, "top", &files),
    );
    // The harness names the top `top`, which picorv32's own already is.
    let dir = support::scratch(Path::new(env!("CARGO_TARGET_TMPDIR")), "picorv32_top");
    let renamed = dir.join("picorv32_top.v");
    let top_v = std::fs::read_to_string(&files[1]).unwrap();
    std::fs::write(
        &renamed,
        top_v.replacen("module top(", "module picorv32_top(", 1),
    )
    .unwrap();
    let mut running = vec![files[0].clone(), renamed];
    running.extend(design("picorv32_run.v"));
    let s = synthesize("zybo_picorv32_run", "run", &running, Options::default());
    let r = support::cosim(&s.dir, &s.rtl, &s.refined, &s.cells, 20000, 1)
        .unwrap_or_else(|e| panic!("picorv32: {e}"));
    assert!(r.compared > 100_000, "{} output bits compared", r.compared);
    let logic = synthesize(
        "zybo_picorv32_logic",
        "top",
        &files,
        Options {
            distributed_ram: false,
            block_ram: false,
        },
    );
    prove("zybo_picorv32_logic", &logic, &[]);
}

#[test]
fn zybo_sha256() {
    let files = example(
        "sha256",
        &[
            "sha256.v",
            "sha256_core.v",
            "sha256_k_constants.v",
            "sha256_w_mem.v",
            "top.v",
        ],
    );
    let s = synthesize("zybo_sha256", "top", &files, Options::default());
    compare(
        "zybo-sha256",
        &counts(&s.cells),
        &synth_xilinx(&s.dir, "top", &files),
    );
    prove("zybo_sha256", &s, &[]);
}
