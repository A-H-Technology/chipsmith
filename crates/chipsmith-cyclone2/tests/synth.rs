//! chipsmith's Cyclone II synthesis against the yosys flow it replaces
//! (ADR-0004: yosys is the oracle). For each design, from chipsmith's own
//! front end: the netlist is proven sequentially equivalent to yosys' RTL
//! of the same Verilog, the packer takes it, and it is about as small as
//! the yosys flow's; M4K blocks come out as the yosys flow's do, cell for
//! cell. Needs yosys (and GHDL for the VHDL example): run in the devshell.

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
            "alumacc" => self.alumacc = d,
            "m4k" => self.m4k = d,
            "cells" => self.cells = d,
            _ => {}
        }
    }
}

/// The equivalence check for a synthesized design: with M4Ks, around the
/// blocks (`support::check_staged`), else straight.
fn prove(name: &str, s: &Synthesized) -> support::Proof {
    let st = &s.stages;
    let cells = st.cells.as_ref().expect("a cells stage");
    let blocks = cells.modules[0]
        .cells
        .iter()
        .any(|c| c.kind.type_name() == "CY2_M4K");
    let r = if blocks {
        support::check_staged(
            &s.dir,
            &s.rtl,
            st.refined.as_ref().expect("a refined stage"),
            st.alumacc.as_ref().expect("an alumacc stage"),
            st.m4k.as_ref().expect("an m4k stage"),
            cells,
        )
    } else {
        support::check(
            &s.dir,
            &s.rtl,
            st.refined.as_ref().expect("a refined stage"),
            cells,
        )
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
    let proof = prove(name, &s);
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
                                        chipsmith_netlist::SigBit::Const(chipsmith_netlist::State::X) => '0',
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
    mutate: impl Fn(&mut Design),
) -> Result<support::Cosim, String> {
    let name = format!("{}-direct", cfg.name());
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
            let r = m4k_direct(&cfg, |_| {}).unwrap_or_else(|e| panic!("{cfg:?}: {e}"));
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
        let r = m4k_direct(&cfg, |_| {}).unwrap_or_else(|e| panic!("{cfg:?}: {e}"));
        eprintln!("{cfg:?}: {r:?}");
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
    for (what, f) in [
        ("swapped address pins", &swap_addr as &dyn Fn(&mut Design)),
        ("swapped block rows", &swap_rows),
    ] {
        let err = m4k_direct(&deep, f).expect_err(what);
        assert!(err.starts_with("MISMATCH seed="), "{what}: {err}");
        eprintln!("{what}: {err}");
    }
}

/// One block per configuration class (`support/m4k_shapes.rs`), the
/// mapping proven: the blocks' model against yosys' memory, word by word.
#[test]
fn m4k_shapes_proven() {
    let only = std::env::var("M4K_SHAPE").ok();
    for (name, v, blocks) in m4k_shapes::shapes() {
        if only.as_deref().is_some_and(|o| o != name) {
            continue;
        }
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
}
