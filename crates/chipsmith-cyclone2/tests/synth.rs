//! chipsmith's Cyclone II synthesis against the yosys flow it replaces
//! (ADR-0004: yosys is the oracle). For each design, from chipsmith's own
//! front end: the netlist is proven sequentially equivalent to yosys' RTL
//! of the same Verilog, the packer takes it, and it is about as small as
//! the yosys flow's; M4K blocks come out as the yosys flow's do, cell for
//! cell. Needs yosys (and GHDL for the VHDL example): run in the devshell.

#[path = "support/mult.rs"]
mod mult;
mod support;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use chipsmith_cyclone2::pin::IoRegisters;
use chipsmith_cyclone2::synth::{synthesize_observed, Observer};
use chipsmith_netlist::{CellKind, Design, Module, ParamValue};

const ROOT: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../..");

fn scratch(name: &str) -> PathBuf {
    support::scratch(Path::new(env!("CARGO_TARGET_TMPDIR")), name)
}

/// The stages the check needs: the RTL with x resolved, and the netlist
/// before wire names are purged.
#[derive(Default)]
struct Stages {
    refined: Option<Design>,
    wreduce: Option<Design>,
    mult: Option<Design>,
    alumacc: Option<Design>,
    m4k: Option<Design>,
    cells: Option<Design>,
}

impl Observer for Stages {
    fn stage(&mut self, name: &str, m: &Module) {
        let d = Some(Design {
            modules: vec![m.clone()],
        });
        match name {
            "refined" => self.refined = d,
            "wreduce" => self.wreduce = d,
            "mult" => self.mult = d,
            "alumacc" => self.alumacc = d,
            "m4k" => self.m4k = d,
            "cells" => self.cells = d,
            _ => {}
        }
    }
}

/// The equivalence check for a synthesized design: with M4Ks or
/// multipliers, around the blocks (`support::check_staged`), else
/// straight. The mappings into blocks are the per-shape proofs'
/// (`m4k_shape_*`, `mult_*`): through a block whose registers took one of
/// the design's, a proof is SAT on a whole product and doesn't finish.
fn prove(name: &str, s: &Synthesized) -> support::Proof {
    let st = &s.stages;
    let cells = st.cells.as_ref().expect("a cells stage");
    let has = |ty: &str| {
        cells.modules[0]
            .cells
            .iter()
            .any(|c| c.kind.type_name() == ty)
    };
    let refined = st.refined.as_ref().expect("a refined stage");
    let r = match (has("CY2_M4K"), has("CY2_MULT")) {
        (true, true) => {
            panic!("{name}: M4Ks and multipliers in one design: no staged check for both yet")
        }
        (true, false) => support::check_staged(
            &s.dir,
            &s.rtl,
            refined,
            st.alumacc.as_ref().expect("an alumacc stage"),
            st.m4k.as_ref().expect("an m4k stage"),
            cells,
        ),
        (false, true) => support::check_staged(
            &s.dir,
            &s.rtl,
            refined,
            st.wreduce.as_ref().expect("a wreduce stage"),
            st.mult.as_ref().expect("a mult stage"),
            cells,
        ),
        (false, false) => support::check(&s.dir, &s.rtl, refined, cells),
    };
    r.unwrap_or_else(|e| panic!("{name}: {e}"))
}

struct Synthesized {
    dir: PathBuf,
    rtl: PathBuf,
    stages: Stages,
    netlist: PathBuf,
}

fn synthesize(name: &str, top: &str, files: &[PathBuf]) -> Synthesized {
    synthesize_with(name, top, files, &BTreeMap::new())
}

fn synthesize_with(
    name: &str,
    top: &str,
    files: &[PathBuf],
    io_registers: &BTreeMap<String, IoRegisters>,
) -> Synthesized {
    synthesize_inner(name, top, files, files, io_registers)
}

/// [`synthesize`], the RTL it is checked against read from `reference`
/// (with the same top and register names) instead of the design's own
/// sources.
fn synthesize_against(
    name: &str,
    top: &str,
    files: &[PathBuf],
    reference: &[PathBuf],
) -> Synthesized {
    synthesize_inner(name, top, files, reference, &BTreeMap::new())
}

fn synthesize_inner(
    name: &str,
    top: &str,
    files: &[PathBuf],
    reference: &[PathBuf],
    io_registers: &BTreeMap<String, IoRegisters>,
) -> Synthesized {
    let dir = scratch(name);
    let rtl = support::rtl(&dir, top, reference);
    let mut stages = Stages::default();
    let out = synthesize_observed(support::frontend(top, files), io_registers, &mut stages)
        .unwrap_or_else(|e| panic!("{name}: {e:#}"))
        .design;
    let netlist = dir.join("chipsmith.json");
    support::write(&out, &netlist);
    Synthesized {
        dir,
        rtl,
        stages,
        netlist,
    }
}

fn pack(dir: &Path, name: &str, netlist: &Path) -> chipsmith_cyclone2::pack::Stats {
    chipsmith_cyclone2::pack::pack_file(netlist, &dir.join(format!("{name}.packed.json")))
        .unwrap_or_else(|e| panic!("packing {}: {e:#}", netlist.display()))
}

/// Synthesizes, proves, packs; with `compare`, also within a few percent
/// of the yosys flow (LUTs and registers after packing, carry chains and
/// M4Ks exactly as many or fewer).
fn design(name: &str, top: &str, files: &[PathBuf], compare: bool) {
    design_with(name, top, files, compare, &BTreeMap::new());
}

fn design_with(
    name: &str,
    top: &str,
    files: &[PathBuf],
    compare: bool,
    io_registers: &BTreeMap<String, IoRegisters>,
) {
    let s = synthesize_with(name, top, files, io_registers);
    let proof = prove(name, &s);
    eprintln!(
        "{name}: {} $equiv proven, {} registers proven constant",
        proof.equivalences, proof.constant_registers
    );
    // Through the blocks: co-simulated.
    if blocks_in(&s.netlist) > 0 {
        let sim = support::cosim(
            &s.dir,
            &s.rtl,
            s.stages.refined.as_ref().unwrap(),
            s.stages.cells.as_ref().unwrap(),
            COSIM_CYCLES,
            1,
        )
        .unwrap_or_else(|e| panic!("{name}: co-simulation: {e}"));
        eprintln!(
            "{name}: co-simulated {} cycles, {} output bits compared",
            sim.cycles, sim.compared
        );
    }
    let ours = pack(&s.dir, "chipsmith", &s.netlist);
    if compare {
        let flow = support::yosys_flow(&s.dir, top, files).unwrap_or_else(|e| panic!("{e}"));
        let theirs = pack(&s.dir, "yosys", &flow);
        eprintln!("{name}: chipsmith {ours:?}\n{name}: yosys flow {theirs:?}");
        let within = |a: usize, b: usize| a as f64 <= b as f64 * 1.05 + 2.0;
        assert!(
            within(ours.luts, theirs.luts),
            "{name}: LUTs {} vs {}",
            ours.luts,
            theirs.luts
        );
        assert!(
            within(ours.ffs, theirs.ffs),
            "{name}: registers {} vs {}",
            ours.ffs,
            theirs.ffs
        );
        // More chains are fine where they save LUTs: yosys turns some
        // short counters into LUTs (uart: 9 chains and 160 LUTs, against
        // 6 and 194).
        assert!(
            ours.chains <= theirs.chains || ours.luts < theirs.luts,
            "{name}: carry chains"
        );
        // chipsmith puts every memory a block takes into blocks, as
        // Quartus does; the yosys flow keeps small ones in logic.
        assert!(ours.m4ks >= theirs.m4ks, "{name}: M4Ks");
    }
}

fn example(dir: &str) -> Vec<PathBuf> {
    let src = Path::new(ROOT).join("examples").join(dir).join("src");
    let mut files: Vec<PathBuf> = std::fs::read_dir(&src)
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.extension().is_some_and(|e| e == "v"))
        .collect();
    files.sort();
    files
}

#[test]
fn blinky_verilog() {
    design("blinky-verilog", "blinky", &example("blinky-verilog"), true);
}

/// GHDL's Verilog of the VHDL example.
#[test]
fn blinky_vhdl() {
    let dir = scratch("blinky-vhdl-src");
    let src = Path::new(ROOT).join("examples/blinky-vhdl/src");
    let vhdl: Vec<PathBuf> = std::fs::read_dir(&src)
        .unwrap()
        .map(|e| e.unwrap().path())
        .collect();
    let out = std::process::Command::new("ghdl")
        .args(["--synth", "--std=08", "--out=verilog"])
        .args(&vhdl)
        .args(["-e", "blinky"])
        .current_dir(&dir)
        .output()
        .expect("ghdl on PATH: run in `nix develop`");
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let v = dir.join("blinky.v");
    std::fs::write(&v, out.stdout).unwrap();
    design("blinky-vhdl", "blinky", &[v], true);
}

#[test]
fn bidir() {
    design("bidir", "top", &example("bidir"), true);
}

/// I/O-cell registers, as examples/de2-io's manifest asks for them: input
/// registers moved, output and OE registers copied (one powering up high,
/// one behind an inverter), proven against the RTL with the registers in
/// the pin buffers' models.
#[test]
fn io_registers() {
    let mut regs = BTreeMap::new();
    let bits =
        |port: &str, n: usize| -> Vec<String> { (0..n).map(|i| format!("{port}[{i}]")).collect() };
    for p in bits("SW_HI", 5) {
        regs.insert(
            p,
            IoRegisters {
                input: true,
                ..Default::default()
            },
        );
    }
    for p in bits("LEDR", 5).into_iter().chain(bits("LEDG", 5)) {
        regs.insert(
            p,
            IoRegisters {
                output: true,
                ..Default::default()
            },
        );
    }
    regs.insert(
        "GPIO_IO".into(),
        IoRegisters {
            input: true,
            output: true,
            output_enable: true,
        },
    );
    design_with("de2-io", "top", &example("de2-io"), false, &regs);
    let s = synthesize_with("de2-io-cells", "top", &example("de2-io"), &regs);
    let cells = s.stages.cells.expect("a cells stage");
    let registered = cells.modules[0]
        .cells
        .iter()
        .filter(|c| match &c.kind {
            CellKind::Other(g) => {
                g.ty.starts_with("CY2_") && g.params.keys().any(|k| k.ends_with("_REGISTER"))
            }
            _ => false,
        })
        .count();
    // SW_HI[4:0], LEDR[4:0], LEDG[4:0] and GPIO_IO.
    assert_eq!(registered, 16);
}

/// Output and OE registers with clock enables and synchronous resets
/// (every order `$sdffe`/`$sdffce` has, either polarity, powering up 0 and
/// 1, one register on two pins): the I/O cell's plain register samples
/// the LE register's next state. Proven against the RTL with the
/// registers in the pin buffers' models, and co-simulated.
#[test]
fn io_registers_with_enables_and_resets() {
    let dir = scratch("io-register-gated-src");
    let v = dir.join("top.v");
    std::fs::write(
        &v,
        "module top(input clk, input en, input en_n, input rst, input [5:0] a,\n\
                    output [6:0] y, output [1:0] t);\n\
         reg q0 = 0; always @(posedge clk) if (en) q0 <= a[0];\n\
         reg q1 = 1; always @(posedge clk) if (!en_n) q1 <= a[1];\n\
         reg q2 = 0; always @(posedge clk) if (rst) q2 <= 1'b1; else q2 <= a[2];\n\
         reg q3 = 1; always @(posedge clk) if (rst) q3 <= 1'b0; else if (en) q3 <= a[3];\n\
         reg q4 = 0; always @(posedge clk) if (en) begin if (rst) q4 <= 1'b1; else q4 <= a[4]; end\n\
         reg [1:0] st = 0;\n\
         always @(posedge clk) case (st) 0: if (en) st <= 1; 1: st <= a[5] ? 2 : 0; default: st <= 0; endcase\n\
         reg oe = 0; always @(posedge clk) if (rst) oe <= 0; else if (en) oe <= a[5];\n\
         reg [1:0] tq = 0; always @(posedge clk) if (en_n) tq <= a[1:0];\n\
         assign y = {st[1], st[0], q4, q3, q2, q1, q0};\n\
         assign t[0] = oe ? tq[0] : 1'bz; assign t[1] = oe ? tq[1] : 1'bz;\nendmodule\n",
    )
    .unwrap();
    let out = IoRegisters {
        output: true,
        ..Default::default()
    };
    let mut regs: BTreeMap<String, IoRegisters> =
        (0..7).map(|i| (format!("y[{i}]"), out)).collect();
    for i in 0..2 {
        regs.insert(
            format!("t[{i}]"),
            IoRegisters {
                output: true,
                output_enable: true,
                ..Default::default()
            },
        );
    }
    let files = [v];
    design_with("io-register-gated", "top", &files, false, &regs);
    let s = synthesize_with("io-register-gated-cosim", "top", &files, &regs);
    let sim = support::cosim(
        &s.dir,
        &s.rtl,
        s.stages.refined.as_ref().unwrap(),
        s.stages.cells.as_ref().unwrap(),
        COSIM_CYCLES,
        1,
    )
    .unwrap_or_else(|e| panic!("co-simulation: {e}"));
    assert!(sim.compared > 0);
    let cells = s.stages.cells.expect("a cells stage");
    let registered = cells.modules[0]
        .cells
        .iter()
        .filter(|c| match &c.kind {
            CellKind::Other(g) => {
                g.ty.starts_with("CY2_") && g.params.keys().any(|k| k.ends_with("_REGISTER"))
            }
            _ => false,
        })
        .count();
    assert_eq!(registered, 9);
}

/// What an I/O register can't be is an error, not a register left behind.
#[test]
fn io_registers_that_cant_be_are_errors() {
    let dir = scratch("io-register-errors");
    let v = dir.join("top.v");
    std::fs::write(
        &v,
        "module top(input clk, input en, input rst, input a, input b, output y, output z, \
                    output w, output e);\n\
         reg q; always @(posedge clk or posedge rst) if (rst) q <= 0; else q <= a;\n\
         reg n; always @(negedge clk) n <= a;\n\
         reg s; always @(posedge clk) if (en) s <= b;\n\
         assign y = q; assign z = n; assign w = a; assign e = s;\nendmodule\n",
    )
    .unwrap();
    for (port, want, says) in [
        (
            "y",
            IoRegisters {
                output: true,
                ..Default::default()
            },
            "make the reset synchronous",
        ),
        (
            "b",
            IoRegisters {
                input: true,
                ..Default::default()
            },
            "sample the pin in a plain register",
        ),
        (
            "z",
            IoRegisters {
                output: true,
                ..Default::default()
            },
            "falling edge",
        ),
        (
            "w",
            IoRegisters {
                output: true,
                ..Default::default()
            },
            "no register drives",
        ),
        (
            "en",
            IoRegisters {
                input: true,
                ..Default::default()
            },
            "samples the pin",
        ),
        (
            "a",
            IoRegisters {
                output: true,
                ..Default::default()
            },
            "on an input",
        ),
        (
            "y",
            IoRegisters {
                output_enable: true,
                ..Default::default()
            },
            "never tri-stated",
        ),
        (
            "nope",
            IoRegisters {
                input: true,
                ..Default::default()
            },
            "doesn't have",
        ),
    ] {
        let regs = BTreeMap::from([(port.to_string(), want)]);
        let err = match synthesize_observed(
            support::frontend("top", std::slice::from_ref(&v)),
            &regs,
            &mut (),
        ) {
            Ok(_) => panic!("{port} {want:?}: synthesized"),
            Err(e) => format!("{e:#}"),
        };
        assert!(err.contains(says), "{port} {want:?}: {err}");
    }
}

#[test]
fn picorv32() {
    design("picorv32", "top", &example("picorv32"), true);
}

#[test]
fn ram() {
    design("ram", "top", &example("ram"), true);
}

#[test]
fn ram_widths() {
    design("ram_widths", "top", &example("ram_widths"), true);
}

#[test]
fn ram_leds() {
    design("ram-leds", "top", &example("ram-leds"), true);
}

// The corpus: real open-source cores, unmodified, each behind a DE2
// wrapper (examples/<name>/chipsmith.toml has the upstream and commit).

#[test]
fn uart() {
    design("uart", "top", &example("uart"), true);
}

#[test]
fn spi_master() {
    // The yosys flow can't build it: dfflegalize has no register for an
    // asynchronous reset to 1 with a power-up value of 0.
    design("spi-master", "top", &example("spi-master"), false);
}

#[test]
fn i2c_master() {
    design("i2c-master", "top", &example("i2c-master"), true);
}

#[test]
fn sha256() {
    // As spi_master: the yosys flow stops in dfflegalize.
    design("sha256", "top", &example("sha256"), false);
}

#[test]
fn femtorv32() {
    design("femtorv32", "top", &example("femtorv32"), true);
}

#[test]
fn serv() {
    design("serv", "top", &example("serv"), true);
}

/// The serial flash loader `flash --persist` builds: JTAG and ASMI hard
/// blocks.
#[test]
fn loader() {
    let f = Path::new(ROOT).join("crates/chipsmith-cyclone2/src/persist/loader/top.v");
    design("loader", "top", &[f], true);
}

/// cyclone2-re specimens the netlist crate keeps (the ones in plain
/// Verilog).
#[test]
fn specimens() {
    for s in [
        "specimen-cnt-c20",
        "specimen-de2-d1",
        "specimen-empty",
        "specimen-io-roles2",
        "specimen-oe-oe1",
    ] {
        let f = Path::new(ROOT)
            .join("crates/chipsmith-netlist/tests/corpus")
            .join(s)
            .join("top.v");
        design(s, "top", &[f], false);
    }
}

/// Small designs for what the yosys flow can't synthesize for the LE
/// (latches, asynchronous loads: `dfflegalize` fails), so there is only
/// the proof.
fn feature(name: &str) {
    let f = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/designs")
        .join(format!("{name}.v"));
    design(name, "top", &[f], false);
}

#[test]
fn latches() {
    feature("latches");
}

#[test]
fn async_load() {
    feature("async_load");
}

/// Falling-edge registers: the LAB clock's inversion (CLK_INV), not an
/// inverter in front of the clock. The proof steps every register on one
/// edge, so it can't tell the edges apart; the co-simulation does, and
/// fails with the inversion dropped.
#[test]
fn negedge() {
    let f = Path::new(ROOT).join("crates/chipsmith-cyclone2/tests/designs/negedge.v");
    let s = synthesize("negedge", "top", std::slice::from_ref(&f));
    let proof = prove("negedge", &s);
    eprintln!("negedge: {} $equiv proven", proof.equivalences);
    let refined = s.stages.refined.as_ref().unwrap();
    let gate = s.stages.cells.as_ref().unwrap();
    let sim = support::cosim(&s.dir, &s.rtl, refined, gate, COSIM_CYCLES, 1)
        .unwrap_or_else(|e| panic!("negedge: co-simulation: {e}"));
    eprintln!(
        "negedge: co-simulated {} cycles, {} output bits compared",
        sim.cycles, sim.compared
    );
    let cells = &gate.modules[0].cells;
    let inverted = |c: &chipsmith_netlist::Cell| matches!(&c.kind, CellKind::Other(g) if g.ty == "LCELL_FF" && g.params.get("CLK_INV") == Some(&ParamValue::int(1)));
    // n (8) and ne (4) on the falling edge.
    assert_eq!(cells.iter().filter(|c| inverted(c)).count(), 12);
    // No LUT reads the clock: an inverter in front of a register would.
    let ff_clk = |c: &chipsmith_netlist::Cell| match &c.kind {
        CellKind::Other(g) if g.ty == "LCELL_FF" => g.port("CLK").map(|p| p.sig[0]),
        _ => None,
    };
    let clk = cells.iter().find_map(ff_clk).expect("a register");
    assert!(
        cells.iter().filter_map(ff_clk).all(|c| c == clk),
        "one clock net"
    );
    for c in cells {
        if let CellKind::Other(g) = &c.kind {
            if g.ty == "LCELL_COMB" {
                assert!(
                    g.ports.iter().all(|p| !p.sig.contains(&clk)),
                    "{} reads the clock",
                    c.name
                );
            }
        }
    }
    let mut rising = gate.clone();
    for c in rising.modules[0].cells.iter_mut() {
        if let CellKind::Other(g) = &mut c.kind {
            g.params.remove("CLK_INV");
        }
    }
    let dir = scratch("negedge-rising");
    match support::cosim(&dir, &s.rtl, refined, &rising, COSIM_CYCLES, 1) {
        Ok(_) => panic!("negedge: every register on the rising edge, not caught"),
        Err(e) => {
            assert!(e.starts_with("MISMATCH"), "{e}");
            eprintln!("negedge, inversion dropped: caught ({e})");
        }
    }
    pack(&s.dir, "chipsmith", &s.netlist);
}

/// Forencich's lfsr.v (examples/crc32), which declares arrays inside its
/// mask function, as Ethernet's CRC-32: its elaboration proven against a
/// textbook bitwise CRC-32 with the same top (`designs/crc32_ref.v`; the
/// RTL against the refined stage), and the netlist co-simulated against
/// that reference. yosys' own front end takes over 20 minutes on lfsr.v's
/// constant functions, so the oracle's RTL is the reference's. The refined
/// stage against the netlist isn't proven: `crc` powers up all ones, so
/// synthesis keeps it inverted in an unnamed register, `shown` loads from
/// that register directly, and equiv_induct leaves `shown`'s 27 bits
/// unproven (the co-simulation compares them every cycle). Remove the
/// split once the check proves that step.
#[test]
fn crc32() {
    let reference = Path::new(ROOT).join("crates/chipsmith-cyclone2/tests/designs/crc32_ref.v");
    let s = synthesize_against(
        "crc32",
        "top",
        &example("crc32"),
        std::slice::from_ref(&reference),
    );
    let refined = s.stages.refined.as_ref().unwrap();
    let proof = support::check(&s.dir, &s.rtl, refined, refined)
        .unwrap_or_else(|e| panic!("crc32: elaboration against the reference: {e}"));
    eprintln!(
        "crc32: elaboration, {} $equiv proven against the reference",
        proof.equivalences
    );
    let sim = support::cosim(
        &s.dir,
        &s.rtl,
        refined,
        s.stages.cells.as_ref().unwrap(),
        COSIM_CYCLES,
        1,
    )
    .unwrap_or_else(|e| panic!("crc32: co-simulation: {e}"));
    eprintln!(
        "crc32: co-simulated {} cycles, {} output bits compared",
        sim.cycles, sim.compared
    );
    pack(&s.dir, "chipsmith", &s.netlist);
}

#[test]
fn divmod() {
    feature("divmod");
}

#[test]
fn sums() {
    feature("sums");
}

/// The check catches a wrong latch (the one inside an emulated register
/// too) and a wrong divider: the first LUT `pick` takes, complemented.
#[test]
fn a_complemented_lut_is_caught() {
    let is_loop = |g: &chipsmith_netlist::Generic| {
        let y = g.port("COMBOUT").map(|p| p.sig[0]);
        g.ports
            .iter()
            .any(|p| p.name != "COMBOUT" && Some(p.sig[0]) == y)
    };
    let any = |_: &chipsmith_netlist::Generic| true;
    for (name, pick) in [
        (
            "latches",
            &is_loop as &dyn Fn(&chipsmith_netlist::Generic) -> bool,
        ),
        ("async_load", &is_loop),
        ("divmod", &any),
    ] {
        let f = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/designs")
            .join(format!("{name}.v"));
        let s = synthesize(&format!("{name}-broken"), "top", &[f]);
        let mut gate = s.stages.cells.clone().unwrap();
        let g = gate.modules[0]
            .cells
            .iter_mut()
            .find_map(|c| match &mut c.kind {
                CellKind::Other(g) if g.ty == "LCELL_COMB" && pick(g) => Some(g),
                _ => None,
            })
            .expect("a LUT to break");
        let Some(ParamValue::Bits(mask)) = g.params.get_mut("LUT_MASK") else {
            panic!("LCELL_COMB without LUT_MASK")
        };
        for b in &mut mask.0 {
            *b = match b {
                chipsmith_netlist::State::One => chipsmith_netlist::State::Zero,
                _ => chipsmith_netlist::State::One,
            };
        }
        let dir = scratch(&format!("{name}-broken-check"));
        let err = support::check(&dir, &s.rtl, s.stages.refined.as_ref().unwrap(), &gate)
            .expect_err("a complemented LUT");
        assert!(err.contains("not proven equivalent"), "{name}: {err}");
    }
}

/// The M4K cells, parameters and contents included, are the yosys flow's:
/// cyclone2-re validated those bit for bit against Quartus.
#[test]
fn m4k_cells_are_the_yosys_flows() {
    for ex in ["ram", "ram_widths", "ram-leds"] {
        let files = example(ex);
        let s = synthesize(&format!("{ex}-m4k"), "top", &files);
        let flow = support::yosys_flow(&s.dir, "top", &files).unwrap_or_else(|e| panic!("{e}"));
        let blocks = |path: &Path| {
            let d = support::read(path);
            let m = d.modules.into_iter().find(|m| !m.is_blackbox()).unwrap();
            // Name, parameters, and each port's constant/net pattern.
            type Block = (String, Vec<(String, ParamValue)>, Vec<(String, String)>);
            let mut v: Vec<Block> = m
                .cells
                .into_iter()
                .filter_map(|c| match c.kind {
                    CellKind::Other(g) if g.ty == "CY2_M4K" => {
                        // SLICE says what the yosys flow's cells leave to
                        // their pins.
                        let params = g.params.into_iter().filter(|(k, _)| k != "SLICE").collect();
                        // Which pins are constant, and which they are.
                        let shape = g
                            .ports
                            .into_iter()
                            .map(|p| {
                                let s: String = p
                                    .sig
                                    .iter()
                                    .map(|b| match b {
                                        chipsmith_netlist::SigBit::Net(_) => 'n',
                                        // Unused data columns: x here, 0 in
                                        // the yosys flow; neither is wired.
                                        chipsmith_netlist::SigBit::Const(
                                            chipsmith_netlist::State::X,
                                        ) => '0',
                                        chipsmith_netlist::SigBit::Const(s) => s.char(),
                                    })
                                    .collect();
                                (p.name, s)
                            })
                            .collect();
                        Some((c.name.trim_start_matches('\\').to_owned(), params, shape))
                    }
                    _ => None,
                })
                .collect();
            v.sort_by(|a, b| a.0.cmp(&b.0));
            v
        };
        let (ours, theirs) = (blocks(&s.netlist), blocks(&flow));
        assert_eq!(ours.len(), theirs.len(), "{ex}: number of M4Ks");
        for (o, t) in ours.iter().zip(&theirs) {
            assert_eq!(o.0, t.0, "{ex}: block names");
            assert_eq!(o.1, t.1, "{ex}: {} parameters", o.0);
            let mut os = o.2.clone();
            let mut ts = t.2.clone();
            os.sort();
            ts.sort();
            assert_eq!(os, ts, "{ex}: {} pins", o.0);
        }
    }
}

/// Every stage of the pipeline, not only the last, is equivalent to the
/// RTL: the check works on any of them (they keep the wire names).
#[test]
fn every_stage_is_equivalent() {
    #[derive(Default)]
    struct All(Vec<(String, Design)>);
    impl Observer for All {
        fn stage(&mut self, name: &str, m: &Module) {
            self.0.push((
                name.to_owned(),
                Design {
                    modules: vec![m.clone()],
                },
            ));
        }
    }
    for (ex, top) in [
        ("blinky-verilog", "blinky"),
        ("bidir", "top"),
        ("ram-leds", "top"),
    ] {
        let files = example(ex);
        let dir = scratch(&format!("{ex}-stages"));
        let rtl = support::rtl(&dir, top, &files);
        let mut all = All::default();
        synthesize_observed(
            support::frontend(top, &files),
            &Default::default(),
            &mut all,
        )
        .unwrap();
        let refined = all
            .0
            .iter()
            .find(|(n, _)| n == "refined")
            .unwrap()
            .1
            .clone();
        // From the M4K mapping on, stages are compared with it around the
        // blocks; the mapping itself is the `m4k_shape_*` proofs'.
        let m4k_at = all.0.iter().position(|(n, _)| n == "m4k");
        for (i, (name, stage)) in all.0.iter().enumerate() {
            let d = scratch(&format!("{ex}-stage-{name}"));
            let r = match m4k_at {
                Some(at) if i >= at => support::check_after_m4k(&d, &all.0[at].1, stage),
                _ => support::check(&d, &rtl, &refined, stage),
            };
            r.unwrap_or_else(|e| panic!("{ex}, stage {name}: {e}"));
        }
    }
}

/// The check isn't vacuous: one bit of one LUT changed, it fails.
#[test]
fn a_wrong_lut_bit_is_caught() {
    let files = example("blinky-verilog");
    let s = synthesize("blinky-broken", "blinky", &files);
    let mut gate = s.stages.cells.clone().unwrap();
    let m = &mut gate.modules[0];
    let le = m
        .cells
        .iter_mut()
        .find(|c| c.kind.type_name() == "LCELL_COMB")
        .expect("an LE");
    let CellKind::Other(g) = &mut le.kind else {
        unreachable!()
    };
    // The mask entry COMBOUT reads with every signal input 0 (and the
    // constant ones as they are): one the counter's bit 0 reaches.
    let one = |p: &str| g.port(p).unwrap().sig[0] == chipsmith_netlist::SigBit::ONE;
    let index = usize::from(one("DATAA"))
        | usize::from(one("DATAB")) << 1
        | usize::from(one("DATAC") && one("CIN")) << 2
        | usize::from(one("DATAD")) << 3;
    let Some(ParamValue::Bits(mask)) = g.params.get_mut("LUT_MASK") else {
        panic!("LCELL_COMB without LUT_MASK")
    };
    mask.0[index] = match mask.0[index] {
        chipsmith_netlist::State::One => chipsmith_netlist::State::Zero,
        _ => chipsmith_netlist::State::One,
    };
    let refined = s.stages.refined.as_ref().unwrap();
    support::check(&s.dir, &s.rtl, refined, s.stages.cells.as_ref().unwrap())
        .expect("the unchanged netlist is equivalent");
    let dir = scratch("blinky-broken-check");
    let err = support::check(&dir, &s.rtl, refined, &gate).expect_err("a changed LUT bit");
    assert!(err.contains("not proven equivalent"), "{err}");
}

/// One multiplier configuration per test (`support::mult::shapes`): the
/// `$mul` maps to the expected number of blocks and the netlist is proven
/// against the RTL, the blocks modelled by `SIM_MODELS`' CY2_MULT.
fn mult_shape(name: &str) {
    let (_, verilog, blocks) = mult::shapes()
        .into_iter()
        .find(|(n, _, _)| *n == name)
        .unwrap_or_else(|| panic!("no multiplier shape {name}"));
    let dir = scratch(&format!("mult-{name}"));
    let v = dir.join("top.v");
    std::fs::write(&v, verilog).unwrap();
    let s = synthesize(&format!("mult-{name}"), "top", std::slice::from_ref(&v));
    let cells = s.stages.cells.as_ref().expect("a cells stage");
    let mults = cells.modules[0]
        .cells
        .iter()
        .filter(|c| matches!(&c.kind, CellKind::Other(g) if g.ty == "CY2_MULT"))
        .count();
    assert_eq!(mults, blocks, "{name}: multiplier blocks");
    // Only the `reg_*` shapes move registers in (`support::mult`: through a
    // block's register a proof is SAT on the whole product).
    if !name.starts_with("reg_") {
        for c in &cells.modules[0].cells {
            if let CellKind::Other(g) = &c.kind {
                for k in ["A_REG", "B_REG", "P_REG"] {
                    let set = g
                        .params
                        .get(k)
                        .and_then(ParamValue::bits)
                        .is_some_and(|b| b.as_bool());
                    assert!(!set, "{name}: {k} set on {}", c.name);
                }
            }
        }
    }
    // Co-simulation first: it is seconds, and a wrong netlist shows as a
    // mismatch on a vector rather than as an unproven equivalence.
    let started = std::time::Instant::now();
    let sim = support::cosim(
        &s.dir,
        &s.rtl,
        s.stages.refined.as_ref().unwrap(),
        cells,
        2000,
        1,
    )
    .unwrap_or_else(|e| panic!("{name}: {e}"));
    eprintln!(
        "mult {name}: co-simulated {} cycles, {} output bits compared in {:.1} s",
        sim.cycles,
        sim.compared,
        started.elapsed().as_secs_f64()
    );
    // Registers in the block with an asynchronous clear and an enable:
    // equiv_induct finds no invariant between the RTL's registers and the
    // model's (the block's registers have no RTL wire to be matched to,
    // and the induction can't relate them), and a bounded check through
    // the multiplier doesn't finish; the co-simulation above is the check.
    const COSIM_ONLY: [&str; 1] = ["reg_enable_clear"];
    if COSIM_ONLY.contains(&name) {
        eprintln!("mult {name}: co-simulated only (no proof)");
    } else {
        let started = std::time::Instant::now();
        let proof = support::check(
            &s.dir,
            &s.rtl,
            s.stages.refined.as_ref().expect("a refined stage"),
            cells,
        )
        .unwrap_or_else(|e| panic!("{name}: {e}"));
        eprintln!(
            "mult {name}: {blocks} blocks, {} $equiv proven in {:.1} s",
            proof.equivalences,
            started.elapsed().as_secs_f64()
        );
    }
    let stats = pack(&s.dir, "chipsmith", &s.netlist);
    assert_eq!(stats.mults, blocks, "{name}: packed multipliers");
}

/// Dense products over several blocks, which the proofs can't do: the
/// RTL against the netlist on thousands of random operand pairs.
fn mult_dense(name: &str, wa: usize, wb: usize, signed: bool, wp: usize, blocks: usize) {
    let sg = if signed { "signed " } else { "" };
    let dir = scratch(&format!("mult-dense-{name}"));
    let v = dir.join("top.v");
    std::fs::write(
        &v,
        format!(
            "module top(input clk, input [{wa1}:0] a, input [{wb1}:0] b, output reg [{wp1}:0] p);
  reg {sg}[{wa1}:0] ra; reg {sg}[{wb1}:0] rb;
  always @(posedge clk) begin ra <= a; rb <= b; p <= ra * rb; end
endmodule
",
            wa1 = wa - 1,
            wb1 = wb - 1,
            wp1 = wp - 1
        ),
    )
    .unwrap();
    let s = synthesize(
        &format!("mult-dense-{name}"),
        "top",
        std::slice::from_ref(&v),
    );
    let cells = s.stages.cells.as_ref().expect("a cells stage");
    let stats = pack(&s.dir, "chipsmith", &s.netlist);
    assert_eq!(stats.mults, blocks, "{name}: packed multipliers");
    let started = std::time::Instant::now();
    let sim = support::cosim(
        &s.dir,
        &s.rtl,
        s.stages.refined.as_ref().unwrap(),
        cells,
        5000,
        7,
    )
    .unwrap_or_else(|e| panic!("{name}: {e}"));
    eprintln!(
        "mult dense {name}: {blocks} blocks, co-simulated {} cycles, {} output bits compared in {:.1} s",
        sim.cycles,
        sim.compared,
        started.elapsed().as_secs_f64()
    );
}

/// One block with every register in it (operands and product): the
/// per-shape proofs keep their registers out of the block.
#[test]
fn mult_dense_s18x18() {
    mult_dense("s18x18", 18, 18, true, 36, 1);
}

#[test]
fn mult_dense_u20x20() {
    mult_dense("u20x20", 20, 20, false, 40, 4);
}

#[test]
fn mult_dense_s24x24() {
    mult_dense("s24x24", 24, 24, true, 48, 4);
}

#[test]
fn mult_dense_u32x32_32() {
    mult_dense("u32x32_32", 32, 32, false, 32, 3);
}

#[test]
fn mult_dense_s32x32() {
    mult_dense("s32x32", 32, 32, true, 64, 4);
}

#[test]
fn mult_dense_u36x18() {
    mult_dense("u36x18", 36, 18, false, 54, 2);
}

/// The co-simulation has teeth: two bits of one operand swapped, or one
/// operand's sign wrong, and it fails.
#[test]
fn mult_cosim_catches_a_swapped_bit_and_a_wrong_sign() {
    let (_, verilog, _) = mult::shapes()
        .into_iter()
        .find(|(n, _, _)| *n == "s12x7")
        .unwrap();
    let dir = scratch("mult-mutations");
    let v = dir.join("top.v");
    std::fs::write(&v, verilog).unwrap();
    let s = synthesize("mult-mutations", "top", std::slice::from_ref(&v));
    let cells = s.stages.cells.as_ref().expect("a cells stage");
    let mutate = |what: &str, slug: &str, f: &dyn Fn(&mut chipsmith_netlist::Generic)| {
        let mut d = cells.clone();
        let m = d
            .modules
            .iter_mut()
            .find(|m| !m.is_blackbox())
            .unwrap()
            .cells
            .iter_mut()
            .find_map(|c| match &mut c.kind {
                CellKind::Other(g) if g.ty == "CY2_MULT" => Some(g),
                _ => None,
            })
            .expect("a multiplier");
        f(m);
        let dir = scratch(&format!("mult-mutations-{slug}"));
        match support::cosim(&dir, &s.rtl, s.stages.refined.as_ref().unwrap(), &d, 500, 3) {
            Ok(_) => panic!("{what}: not caught"),
            Err(e) => {
                assert!(e.starts_with("MISMATCH"), "{what}: {e}");
                eprintln!("{what}: caught ({e})");
            }
        }
    };
    mutate("swapped operand bits", "swap", &|g| {
        let a = g.ports.iter_mut().find(|p| p.name == "A").unwrap();
        a.sig.swap(2, 5);
    });
    mutate("wrong sign", "sign", &|g| {
        g.params.insert("B_SIGNED".into(), ParamValue::int(0));
    });
}

macro_rules! mult_shapes {
    ($($name:ident),* $(,)?) => {
        $(
            #[test]
            fn $name() {
                mult_shape(&stringify!($name)["mult_".len()..]);
            }
        )*
    };
}

mult_shapes!(
    mult_u9x9,
    mult_s9x9,
    mult_s9x3,
    mult_u4x7,
    mult_u18x18,
    mult_s18x18,
    mult_s12x7,
    mult_u18x10,
    mult_u16x16_16,
    mult_u22x22_sparse,
    mult_s22x22_sparse,
    mult_u32x32_32_sparse,
    mult_u40x18_sparse,
    mult_s40x18_sparse,
    mult_s12xu8,
    mult_const_k,
    mult_one_bit,
    mult_two_products,
    mult_reg_in,
    mult_reg_out,
    mult_reg_in_out,
    mult_reg_enable_clear,
    mult_reg_in_shared,
    mult_reg_out_other_enable,
);

/// Which registers moved into the block, per shape: (A_REG, B_REG, P_REG)
/// of its one CY2_MULT, and the LE registers left.
#[test]
fn mult_registers_move_in_only_where_legal() {
    for (name, want, left) in [
        ("reg_in", [true, true, false], 0),
        ("reg_out", [false, false, true], 0),
        ("reg_in_out", [true, true, true], 0),
        ("reg_enable_clear", [true, true, true], 0),
        // ra also drives the output o: it stays, rb moves in.
        ("reg_in_shared", [false, true, false], 9),
        // q's enable differs from the operands' (none): q stays.
        ("reg_out_other_enable", [true, true, false], 18),
    ] {
        let (_, verilog, _) = mult::shapes()
            .into_iter()
            .find(|(n, _, _)| *n == name)
            .unwrap();
        let dir = scratch(&format!("mult-regs-{name}"));
        let v = dir.join("top.v");
        std::fs::write(&v, verilog).unwrap();
        let s = synthesize(
            &format!("mult-regs-{name}"),
            "top",
            std::slice::from_ref(&v),
        );
        let cells = &s.stages.cells.as_ref().unwrap().modules[0].cells;
        let m = cells
            .iter()
            .find_map(|c| match &c.kind {
                CellKind::Other(g) if g.ty == "CY2_MULT" => Some(g),
                _ => None,
            })
            .unwrap();
        let got = ["A_REG", "B_REG", "P_REG"].map(|k| m.params[k] == ParamValue::int(1));
        assert_eq!(got, want, "{name}: registers in the block");
        let ffs = cells
            .iter()
            .filter(|c| c.kind.type_name() == "LCELL_FF")
            .count();
        assert_eq!(ffs, left, "{name}: LE registers left");
    }
}

/// Co-simulation cycles per design.
const COSIM_CYCLES: usize = 10_000;

fn blocks_in(netlist: &Path) -> usize {
    support::read(netlist)
        .modules
        .iter()
        .find(|m| !m.is_blackbox())
        .unwrap()
        .cells
        .iter()
        .filter(|c| c.kind.type_name() == "CY2_M4K")
        .count()
}

/// An M4K example or a held-out random memory: in blocks, proven
/// equivalent to the RTL around the blocks (`support::check_staged`),
/// co-simulated against the RTL through them, packed.
fn m4k_design(name: &str, files: &[PathBuf]) {
    let t = std::time::Instant::now();
    let s = synthesize(name, "top", files);
    let blocks = blocks_in(&s.netlist);
    assert!(blocks > 0, "{name}: no M4K");
    let proof = prove(name, &s);
    let proved = t.elapsed();
    let sim = support::cosim(
        &s.dir,
        &s.rtl,
        s.stages.refined.as_ref().unwrap(),
        s.stages.cells.as_ref().unwrap(),
        COSIM_CYCLES,
        1,
    )
    .unwrap_or_else(|e| panic!("{name}: co-simulation: {e}"));
    pack(&s.dir, name, &s.netlist);
    eprintln!(
        "{name}: {blocks} M4Ks; around the blocks {} $equiv proven in {:.0?}; \
         co-simulated {} cycles, {} output bits compared, in {:.0?}",
        proof.equivalences,
        proved,
        sim.cycles,
        sim.compared,
        t.elapsed() - proved
    );
}

macro_rules! m4k_examples {
    ($($f:ident = $ex:literal),*) => {$(
        #[test]
        fn $f() {
            m4k_design($ex, &example($ex));
        }
    )*};
}

m4k_examples!(
    m4k_bytes = "m4k-bytes",
    m4k_enables = "m4k-enables",
    m4k_outreg = "m4k-outreg",
    m4k_tdp = "m4k-tdp",
    m4k_mixed = "m4k-mixed",
    m4k_dualclock = "m4k-dualclock",
    m4k_deep = "m4k-deep",
    m4k_shallow = "m4k-shallow"
);

#[path = "support/m4k_random.rs"]
mod m4k_random;

#[path = "support/m4k_shapes.rs"]
mod m4k_shapes;

/// Held-out random memories (`support/m4k_random.rs`), as built for the
/// DE2 (counters and switches in, LEDs out).
fn m4k_random(seed: u64) {
    let cfg = m4k_random::Config::new(seed);
    let name = cfg.name();
    let dir = scratch(&format!("{name}-src"));
    let v = dir.join("top.v");
    std::fs::write(&v, cfg.verilog()).unwrap();
    eprintln!("{name}: {cfg:?}");
    m4k_design(&name, &[v]);
}

/// A memory with its ports straight on the design's, co-simulated against
/// its RTL: every address reachable every cycle.
fn m4k_direct(
    cfg: &m4k_random::Config,
    tag: &str,
    mutate: impl Fn(&mut Design),
) -> Result<support::Cosim, String> {
    // A scratch directory of its own per use: tests run side by side.
    let name = format!("{}-direct{tag}", cfg.name());
    let dir = scratch(&format!("{name}-src"));
    let v = dir.join("top.v");
    std::fs::write(&v, cfg.verilog_direct()).unwrap();
    let s = synthesize(&name, "top", &[v]);
    assert!(blocks_in(&s.netlist) > 0, "{name}: no M4K");
    let mut gate = s.stages.cells.clone().unwrap();
    mutate(&mut gate);
    support::cosim(
        &s.dir,
        &s.rtl,
        s.stages.refined.as_ref().unwrap(),
        &gate,
        COSIM_CYCLES,
        (cfg.seed as u32).wrapping_mul(2_654_435_761),
    )
}

macro_rules! m4k_random_tests {
    ($($f:ident, $g:ident = $seed:literal),*) => {$(
        #[test]
        fn $f() {
            m4k_random($seed);
        }
        #[test]
        fn $g() {
            let cfg = m4k_random::Config::new($seed);
            let r = m4k_direct(&cfg, "", |_| {}).unwrap_or_else(|e| panic!("{cfg:?}: {e}"));
            eprintln!("{}: {r:?}", cfg.name());
        }
    )*};
}

m4k_random_tests!(
    m4k_random_1,
    m4k_cosim_random_1 = 1,
    m4k_random_2,
    m4k_cosim_random_2 = 2,
    m4k_random_3,
    m4k_cosim_random_3 = 3,
    m4k_random_4,
    m4k_cosim_random_4 = 4,
    m4k_random_5,
    m4k_cosim_random_5 = 5,
    m4k_random_6,
    m4k_cosim_random_6 = 6,
    m4k_random_7,
    m4k_cosim_random_7 = 7,
    m4k_random_8,
    m4k_cosim_random_8 = 8,
    m4k_random_9,
    m4k_cosim_random_9 = 9,
    m4k_random_10,
    m4k_cosim_random_10 = 10,
    m4k_random_11,
    m4k_cosim_random_11 = 11,
    m4k_random_12,
    m4k_cosim_random_12 = 12,
    m4k_random_13,
    m4k_cosim_random_13 = 13,
    m4k_random_14,
    m4k_cosim_random_14 = 14,
    m4k_random_15,
    m4k_cosim_random_15 = 15,
    m4k_random_16,
    m4k_cosim_random_16 = 16
);

/// The examples' large memories (deep, mixed widths), co-simulated with
/// their ports on the design's.
#[test]
fn m4k_cosim_large_shapes() {
    for cfg in m4k_random::Config::shapes() {
        let r = m4k_direct(&cfg, "", |_| {}).unwrap_or_else(|e| panic!("{cfg:?}: {e}"));
        eprintln!("{cfg:?}: {r:?}");
    }
}

/// Write collisions, co-simulated as well as proven: the bypass of the
/// port that lost them is LE logic the proof only sees around one block.
#[test]
fn m4k_cosim_collisions() {
    for name in [
        "tdp_collide_x9",
        "tdp_collide_x16_x4",
        "two_writes_one_read_x9",
    ] {
        let (_, v, blocks) = m4k_shapes::shapes()
            .into_iter()
            .find(|s| s.0 == name)
            .expect("a shape");
        let dir = scratch(&format!("m4k-collide-{name}-src"));
        let f = dir.join("top.v");
        std::fs::write(&f, v).unwrap();
        let s = synthesize(&format!("m4k-collide-{name}"), "top", &[f]);
        assert_eq!(blocks_in(&s.netlist), blocks, "{name}: blocks");
        let r = support::cosim(
            &s.dir,
            &s.rtl,
            s.stages.refined.as_ref().unwrap(),
            s.stages.cells.as_ref().unwrap(),
            COSIM_CYCLES,
            7,
        )
        .unwrap_or_else(|e| panic!("{name}: {e}"));
        eprintln!("m4k collide {name}: {r:?}");
    }
}

/// The co-simulation isn't vacuous: two address pins of a block swapped,
/// or the write enables of two block rows (a wrong block select), and it
/// fails.
#[test]
fn m4k_cosim_catches_wrong_blocks() {
    let deep = m4k_random::Config::shapes().remove(0);
    let swap_addr = |d: &mut Design| {
        let m = &mut d.modules[0];
        let c = m
            .cells
            .iter_mut()
            .find(|c| c.kind.type_name() == "CY2_M4K")
            .unwrap();
        let CellKind::Other(g) = &mut c.kind else {
            unreachable!()
        };
        let a = g.ports.iter_mut().find(|p| p.name == "B_ADDR").unwrap();
        a.sig.swap(10, 11);
    };
    let swap_rows = |d: &mut Design| {
        let m = &mut d.modules[0];
        let we = |m: &Module, row: &str| {
            m.cells
                .iter()
                .position(|c| c.name.ends_with(row) && c.kind.type_name() == "CY2_M4K")
                .unwrap()
        };
        let (i, j) = (we(m, ".0.0"), we(m, ".1.0"));
        let get = |m: &Module, i: usize| match &m.cells[i].kind {
            CellKind::Other(g) => g.port("A_WE").unwrap().sig.clone(),
            _ => unreachable!(),
        };
        let (a, b) = (get(m, i), get(m, j));
        for (k, v) in [(i, b), (j, a)] {
            if let CellKind::Other(g) = &mut m.cells[k].kind {
                g.ports.iter_mut().find(|p| p.name == "A_WE").unwrap().sig = v;
            }
        }
    };
    for (what, tag, f) in [
        (
            "swapped address pins",
            "-swap-addr",
            &swap_addr as &dyn Fn(&mut Design),
        ),
        ("swapped block rows", "-swap-rows", &swap_rows),
    ] {
        let err = m4k_direct(&deep, tag, f).expect_err(what);
        assert!(err.starts_with("MISMATCH seed="), "{what}: {err}");
        eprintln!("{what}: {err}");
    }
}

/// One block per configuration class (`support/m4k_shapes.rs`), the
/// mapping proven: the blocks' model against yosys' memory, word by word.
fn m4k_shape(name: &str) {
    let (_, v, blocks) = m4k_shapes::shapes()
        .into_iter()
        .find(|s| s.0 == name)
        .expect("a shape");
    let t = std::time::Instant::now();
    let dir = scratch(&format!("m4k-shape-{name}-src"));
    let f = dir.join("top.v");
    std::fs::write(&f, v).unwrap();
    let s = synthesize(&format!("m4k-shape-{name}"), "top", &[f]);
    assert_eq!(blocks_in(&s.netlist), blocks, "{name}: blocks");
    let p = support::check(
        &s.dir,
        &s.rtl,
        s.stages.refined.as_ref().unwrap(),
        s.stages.cells.as_ref().unwrap(),
    )
    .unwrap_or_else(|e| panic!("{name}: {e}"));
    eprintln!(
        "m4k shape {name}: {} $equiv proven in {:.0?}",
        p.equivalences,
        t.elapsed()
    );
}

macro_rules! m4k_shape_tests {
    ($($f:ident = $n:literal),*) => {$(
        #[test]
        fn $f() {
            m4k_shape($n);
        }
    )*};
}

m4k_shape_tests!(
    m4k_shape_sp_x1 = "sp_x1",
    m4k_shape_sp_x2 = "sp_x2",
    m4k_shape_sdp_x4 = "sdp_x4",
    m4k_shape_sdp_x9 = "sdp_x9",
    m4k_shape_sp_x18 = "sp_x18",
    m4k_shape_sdp_x36 = "sdp_x36",
    m4k_shape_rom_x36 = "rom_x36",
    m4k_shape_rom_x8_read_enable = "rom_x8_read_enable",
    m4k_shape_bytes_x16 = "bytes_x16",
    m4k_shape_bytes_x32 = "bytes_x32",
    m4k_shape_mixed_w32_r8 = "mixed_w32_r8",
    m4k_shape_mixed_w8_r32 = "mixed_w8_r32",
    m4k_shape_tdp_x9 = "tdp_x9",
    m4k_shape_tdp_x16_x4 = "tdp_x16_x4",
    m4k_shape_tdp_collide_x9 = "tdp_collide_x9",
    m4k_shape_tdp_collide_x16_x4 = "tdp_collide_x16_x4",
    m4k_shape_two_writes_one_read_x9 = "two_writes_one_read_x9",
    m4k_shape_two_clocks_x9 = "two_clocks_x9",
    m4k_shape_clock_enable_x9 = "clock_enable_x9",
    m4k_shape_read_enable_x18 = "read_enable_x18",
    m4k_shape_read_enable_held_x9 = "read_enable_held_x9",
    m4k_shape_outreg_clear_x9 = "outreg_clear_x9",
    m4k_shape_sync_reset_x18 = "sync_reset_x18",
    m4k_shape_shallow_x8 = "shallow_x8"
);

/// The multiplier examples, co-simulated through the blocks (as
/// `SIM_MODELS`' CY2_MULT) and fir proven against the RTL around them
/// (`prove`). mult-selftest isn't proven: the RTL against its own refined
/// stage, before any block, didn't finish its induction in 30 minutes (a
/// serial reference multiplier against nine products). It is co-simulated
/// long enough for every slot to run many times, its error flag (LEDR[0])
/// and pass count compared every cycle.
fn mult_design(name: &str, proof: bool, cycles: usize) {
    let t = std::time::Instant::now();
    let s = synthesize(name, "top", &example(name));
    let cells = s.stages.cells.as_ref().unwrap();
    let mults = cells.modules[0]
        .cells
        .iter()
        .filter(|c| c.kind.type_name() == "CY2_MULT")
        .count();
    assert!(mults > 0, "{name}: no multiplier");
    if proof {
        let p = prove(name, &s);
        eprintln!(
            "{name}: {mults} multipliers, {} $equiv proven in {:.0?}",
            p.equivalences,
            t.elapsed()
        );
    }
    let t = std::time::Instant::now();
    let sim = support::cosim(
        &s.dir,
        &s.rtl,
        s.stages.refined.as_ref().unwrap(),
        cells,
        cycles,
        1,
    )
    .unwrap_or_else(|e| panic!("{name}: co-simulation: {e}"));
    eprintln!(
        "{name}: co-simulated {} cycles, {} output bits compared, in {:.0?}",
        sim.cycles,
        sim.compared,
        t.elapsed()
    );
    pack(&s.dir, name, &s.netlist);
}

#[test]
fn mult_fir() {
    mult_design("fir", true, COSIM_CYCLES);
}

/// 9 slots of 128 cycles a pass: 120,000 cycles are 104 passes.
#[test]
fn mult_selftest() {
    mult_design("mult-selftest", false, 120_000);
}

/// The self-test's co-simulation has teeth: one block's operand made
/// unsigned, and the error flag rises in the netlist but not the RTL.
#[test]
fn mult_selftest_catches_a_wrong_sign() {
    let s = synthesize("mult-selftest-mutated", "top", &example("mult-selftest"));
    let mut d = s.stages.cells.clone().unwrap();
    let g = d.modules[0]
        .cells
        .iter_mut()
        .find_map(|c| match &mut c.kind {
            CellKind::Other(g)
                if g.ty == "CY2_MULT"
                    && g.params
                        .get("A_SIGNED")
                        .and_then(ParamValue::bits)
                        .is_some_and(|c| c.as_bool()) =>
            {
                Some(g)
            }
            _ => None,
        })
        .expect("a signed multiplier");
    g.params.insert("A_SIGNED".into(), ParamValue::int(0));
    let dir = scratch("mult-selftest-mutated-sim");
    match support::cosim(
        &dir,
        &s.rtl,
        s.stages.refined.as_ref().unwrap(),
        &d,
        120_000,
        1,
    ) {
        Ok(_) => panic!("a wrong sign not caught"),
        Err(e) => {
            assert!(e.starts_with("MISMATCH"), "{e}");
            eprintln!("mult-selftest, one block's A unsigned: caught ({e})");
        }
    }
}
