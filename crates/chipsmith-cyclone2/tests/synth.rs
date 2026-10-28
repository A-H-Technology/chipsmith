//! chipsmith's Cyclone II synthesis against the yosys flow it replaces
//! (ADR-0004: yosys is the oracle). For each design, from chipsmith's own
//! front end: the netlist is proven sequentially equivalent to yosys' RTL
//! of the same Verilog, the packer takes it, and it is about as small as
//! the yosys flow's; M4K blocks come out as the yosys flow's do, cell for
//! cell. Needs yosys (and GHDL for the VHDL example): run in the devshell.

mod support;
#[path = "support/mult.rs"]
mod mult;

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
    let dir = scratch(name);
    let rtl = support::rtl(&dir, top, files);
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
    let proof = support::check(
        &s.dir,
        &s.rtl,
        s.stages.refined.as_ref().expect("a refined stage"),
        s.stages.cells.as_ref().expect("a cells stage"),
    )
    .unwrap_or_else(|e| panic!("{name}: {e}"));
    eprintln!(
        "{name}: {} $equiv proven, {} registers proven constant",
        proof.equivalences, proof.constant_registers
    );
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
        assert_eq!(ours.m4ks, theirs.m4ks, "{name}: M4Ks");
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

/// What an I/O register can't be is an error, not a register left behind.
#[test]
fn io_registers_that_cant_be_are_errors() {
    let dir = scratch("io-register-errors");
    let v = dir.join("top.v");
    std::fs::write(
        &v,
        "module top(input clk, input en, input a, output y, output z, output w);\n\
         reg q; always @(posedge clk) if (en) q <= a;\n\
         reg n; always @(negedge clk) n <= a;\n\
         assign y = q; assign z = n; assign w = a;\nendmodule\n",
    )
    .unwrap();
    for (port, want, says) in [
        (
            "y",
            IoRegisters {
                output: true,
                ..Default::default()
            },
            "clock enable",
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
            "a",
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
                        let params = g.params.into_iter().collect();
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
        for (name, stage) in &all.0 {
            let d = scratch(&format!("{ex}-stage-{name}"));
            support::check(&d, &rtl, &refined, stage)
                .unwrap_or_else(|e| panic!("{ex}, stage {name}: {e}"));
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
    let stats = pack(&s.dir, "chipsmith", &s.netlist);
    assert_eq!(stats.mults, blocks, "{name}: packed multipliers");
    let started = std::time::Instant::now();
    let sim = support::cosim::cosim(&s.dir, &s.rtl, cells, 2000, 1)
        .unwrap_or_else(|e| panic!("{name}: {e}"));
    eprintln!(
        "mult {name}: co-simulated {} cycles, {} output bits compared in {:.1} s",
        sim.cycles,
        sim.compared,
        started.elapsed().as_secs_f64()
    );
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
    let s = synthesize(&format!("mult-dense-{name}"), "top", std::slice::from_ref(&v));
    let cells = s.stages.cells.as_ref().expect("a cells stage");
    let stats = pack(&s.dir, "chipsmith", &s.netlist);
    assert_eq!(stats.mults, blocks, "{name}: packed multipliers");
    let started = std::time::Instant::now();
    let sim = support::cosim::cosim(&s.dir, &s.rtl, cells, 5000, 7)
        .unwrap_or_else(|e| panic!("{name}: {e}"));
    eprintln!(
        "mult dense {name}: {blocks} blocks, co-simulated {} cycles, {} output bits compared in {:.1} s",
        sim.cycles,
        sim.compared,
        started.elapsed().as_secs_f64()
    );
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
        match support::cosim::cosim(&dir, &s.rtl, &d, 500, 3) {
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
);
