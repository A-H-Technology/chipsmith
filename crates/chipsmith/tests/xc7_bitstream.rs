//! The Zybo flow checked at the bitstream: each example is built by the
//! `chipsmith` binary, its `.bit` read back by prjxray's `bit2fasm`, the
//! FASM decoded into a netlist (`chipsmith_xc7::decode`) and that netlist
//! co-simulated against yosys' RTL of the example's Verilog
//! (`support::cosim`, iverilog). What the bitstream does is what the RTL
//! does, whatever the packer, placer, router and FASM writer chose. And
//! prjxray's `fasm2frames` and `xc7frames2bit` make the same `.bit` of the
//! FASM as chipsmith's own writer.
//!
//! `CHIPSMITH_XC7_BIT=<path>` decodes that `.bit` instead of building (for
//! `decoder_reads_a_given_bitstream`: openXC7 nextpnr's bitstreams, which
//! have run on the board, check the decoder itself). Needs the devshell
//! (yosys, iverilog, prjxray, prjxray-db).

#[path = "../../chipsmith-cyclone2/tests/support/mod.rs"]
mod support;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;

use chipsmith::manifest::{Manifest, Target};

const ROOT: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../..");

fn setup() {
    let out = Command::new("yosys-config").arg("--datdir").output().unwrap();
    let share = PathBuf::from(String::from_utf8(out.stdout).unwrap().trim());
    support::use_lib(support::Lib {
        name: chipsmith_xc7::synth::cells::LIBRARY_NAME,
        library: chipsmith_xc7::synth::cells::LIBRARY,
        sim: chipsmith_xc7::synth::cells::yosys_sim_without_models(
            &std::fs::read_to_string(share.join("xilinx/cells_sim.v")).unwrap(),
        ),
        models: chipsmith_xc7::synth::cells::MODELS,
    });
}

fn family() -> PathBuf {
    PathBuf::from(std::env::var("CHIPSMITH_PRJXRAY_DB").expect("run in the devshell")).join("zynq7")
}

fn copy_dir(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for e in std::fs::read_dir(from).unwrap() {
        let e = e.unwrap();
        let (src, dst) = (e.path(), to.join(e.file_name()));
        if e.file_type().unwrap().is_dir() {
            if e.file_name() != "build" {
                copy_dir(&src, &dst);
            }
        } else {
            std::fs::copy(&src, &dst).unwrap();
        }
    }
}

struct Example {
    dir: PathBuf,
    top: String,
    files: Vec<PathBuf>,
    pins: BTreeMap<String, String>,
}

fn example(name: &str) -> Example {
    example_at(&Path::new(ROOT).join("examples").join(name))
}

fn example_at(src: &Path) -> Example {
    let name = src.display().to_string();
    let src = src.to_path_buf();
    let m = Manifest::load(&src).unwrap();
    let Target::Xc7z010clg400_1 { pins, .. } = &m.target else {
        panic!("{name} isn't a Zybo example");
    };
    let chipsmith::manifest::Sources::Verilog(files) = &m.sources else {
        panic!("{name}: Verilog only");
    };
    Example {
        dir: src,
        top: m.top.clone(),
        files: files.clone(),
        pins: pins.iter().map(|a| (a.port.to_string(), a.pin.to_string())).collect(),
    }
}

/// Builds `name` in a scratch copy; returns its `.bit`.
fn build(name: &str, scratch: &Path) -> PathBuf {
    // As examples/<name>, beside the examples its sources point into
    // (`../sha256/src/*.v`).
    let examples = scratch.join("examples");
    if examples.exists() {
        std::fs::remove_dir_all(&examples).unwrap();
    }
    let src = Path::new(ROOT).join("examples").join(name);
    let dir = examples.join(name);
    copy_dir(&src, &dir);
    let manifest = std::fs::read_to_string(src.join("chipsmith.toml")).unwrap();
    for sibling in manifest.split("\"../").skip(1).filter_map(|s| s.split('/').next()) {
        copy_dir(&Path::new(ROOT).join("examples").join(sibling), &examples.join(sibling));
    }
    let out = Command::new(env!("CARGO_BIN_EXE_chipsmith"))
        .arg("-C")
        .arg(&dir)
        .arg("build")
        .env("CHIPSMITH_CACHE", "0")
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{name}: build failed:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let bit = String::from_utf8(out.stdout).unwrap();
    PathBuf::from(bit.trim())
}

/// prjxray's `fasm2frames` and `xc7frames2bit` make the same `.bit`
/// payload of the build's FASM as chipsmith's writer did (the header
/// differs: theirs carries a date).
fn prjxray_agrees(bit: &Path, scratch: &Path) {
    let fasm = bit.with_extension("fasm");
    let frm = scratch.join("prjxray.frames");
    let theirs = scratch.join("prjxray.bit");
    let run = |cmd: &mut Command| {
        let out = cmd.output().unwrap();
        assert!(out.status.success(), "{cmd:?}: {}", String::from_utf8_lossy(&out.stderr));
    };
    run(Command::new("fasm2frames")
        .args(["--part", "xc7z010clg400-1", "--db-root"])
        .arg(family())
        .arg(&fasm)
        .arg(&frm));
    run(Command::new("xc7frames2bit")
        .arg("--part_file")
        .arg(family().join("xc7z010clg400-1/part.yaml"))
        .args(["--part_name", "xc7z010clg400-1", "--frm_file"])
        .arg(&frm)
        .arg("--output_file")
        .arg(&theirs));
    let payload = |b: &[u8]| -> Vec<u8> {
        let sync = b.windows(4).position(|w| w == [0xaa, 0x99, 0x55, 0x66]).unwrap();
        b[sync - 48..].to_vec()
    };
    assert!(
        payload(&std::fs::read(bit).unwrap()) == payload(&std::fs::read(&theirs).unwrap()),
        "{}: prjxray's .bit of the same FASM differs",
        bit.display()
    );
}

/// Decodes `bit` and co-simulates it against the RTL of `ex`.
fn check(name: &str, ex: &Example, bit: &Path, scratch: &Path, cycles: usize) {
    setup();
    let fasm = Command::new("bit2fasm")
        .arg("--db-root")
        .arg(family())
        .args(["--part", "xc7z010clg400-1"])
        .arg(bit)
        .output()
        .unwrap();
    assert!(fasm.status.success(), "bit2fasm: {}", String::from_utf8_lossy(&fasm.stderr));
    let fasm = String::from_utf8(fasm.stdout).unwrap();
    std::fs::write(scratch.join("decoded.fasm"), &fasm).unwrap();
    let db = chipsmith_xc7::xray::load(chipsmith_xc7::part::XC7Z010CLG400_1).unwrap();
    let decoded = chipsmith_xc7::decode::decode(&db, &family(), &fasm, &ex.pins)
        .unwrap_or_else(|e| panic!("{name}: decoding: {e:#}"));
    let files: Vec<PathBuf> = ex
        .files
        .iter()
        .flat_map(|f| glob::glob(&ex.dir.join(f).to_string_lossy()).unwrap())
        .map(|p| p.unwrap())
        .collect();
    let rtl = support::rtl(scratch, &ex.top, &files);
    // The RTL's BSCANE2 cut out the way the decoder cuts the bitstream's,
    // and its TCK made the co-simulation's second clock (`clk2`) on both
    // sides: as random data it would clock registers on x.
    let rtl = cut_bscan(scratch, &rtl);
    let mut decoded = decoded;
    for w in &mut decoded.modules[0].wires {
        if w.name == "__bscan_TCK" {
            w.name = "clk2".into();
        }
    }
    let r = support::cosim(scratch, &rtl, &decoded, &decoded, cycles, 1)
        .unwrap_or_else(|e| panic!("{name}: co-simulation: {e}"));
    eprintln!("{name}: {} output bits compared over {cycles} cycles", r.compared);
    assert!(r.compared > 0, "{name}: nothing compared");
}

/// The RTL with each BSCANE2 replaced by ports `__bscan_<PORT>` (outputs
/// in, TDO out).
fn cut_bscan(dir: &Path, rtl: &Path) -> PathBuf {
    use chipsmith_netlist::{CellKind, Dir, SigBit, Wire};
    let mut d = support::read(rtl);
    let m = d.modules.iter_mut().find(|m| !m.is_blackbox()).unwrap();
    let mut subst = chipsmith_synth::rewrite::Subst::new(m);
    let mut ports = Vec::new();
    let cells = std::mem::take(&mut m.cells);
    for c in cells {
        let CellKind::Other(g) = &c.kind else {
            m.cells.push(c);
            continue;
        };
        if g.ty != "BSCANE2" {
            m.cells.push(c);
            continue;
        }
        for p in &g.ports {
            if p.name == "TDO" {
                ports.push((format!("__bscan_{}", p.name), Dir::Out, p.sig.clone()));
            } else {
                let fresh: Vec<SigBit> = p.sig.iter().map(|_| SigBit::Net(m.new_net())).collect();
                for (old, new) in p.sig.iter().zip(&fresh) {
                    if let SigBit::Net(n) = old {
                        subst.set(*n, *new);
                    }
                }
                let name = if p.name == "TCK" {
                    "clk2".to_string()
                } else {
                    format!("__bscan_{}", p.name)
                };
                ports.push((name, Dir::In, fresh));
            }
        }
    }
    for (name, dir, bits) in ports {
        let mut w = Wire::new(name, bits);
        w.port = Some(dir);
        m.wires.push(w);
    }
    subst.apply(m);
    let out = dir.join("rtl_cut.json");
    support::write(&d, &out);
    out
}

fn scratch(name: &str) -> PathBuf {
    support::scratch(Path::new(env!("CARGO_TARGET_TMPDIR")), &format!("xc7_bit_{name}"))
}

fn built(name: &str, cycles: usize) {
    let s = scratch(name);
    let ex = example(name);
    let bit = build(name, &s);
    prjxray_agrees(&bit, &s);
    check(name, &ex, &bit, &s, cycles);
}

#[test]
fn zybo_blinky() {
    built("zybo-blinky", 3000);
}

#[test]
fn zybo_jtag_counter() {
    built("zybo-jtag-counter", 3000);
}

/// Two passes of writing and reading back every memory (8,192 cycles
/// each): an error would light led[0] and count in USER1.
#[test]
fn zybo_bram_selftest() {
    built("zybo-bram-selftest", 17000);
}

/// Three passes over the nine products (1,152 cycles each).
#[test]
fn zybo_mult_selftest() {
    built("zybo-mult-selftest", 3600);
}

/// Two passes of the program out of block RAM (about 20,000 cycles each:
/// the CPU runs on half the clock).
#[test]
fn zybo_riscv_bram() {
    built("zybo-riscv-bram", 50000);
}

/// sha256 on Zybo pins: takes a while to place (the annealer).
#[test]
fn zybo_sha256() {
    built("zybo-sha256", 3000);
}

/// zybo-picorv32 with its reset key held released, so that out of
/// power-up reset the core runs its loop and the LEDs show its stores (its
/// RAM32M register file at work); under random keys it would rarely leave
/// reset. Made in the scratch directory from the example's sources.
#[test]
fn zybo_picorv32_running() {
    let s = scratch("zybo-picorv32-running");
    let project = s.join("src_project");
    let src = Path::new(ROOT).join("examples/picorv32/src");
    std::fs::create_dir_all(project.join("src")).unwrap();
    std::fs::copy(src.join("picorv32.v"), project.join("src/picorv32.v")).unwrap();
    let top = std::fs::read_to_string(src.join("top.v")).unwrap();
    let top = top.replacen("input [3:0] KEY, ", "", 1).replacen(
        "  wire clk = CLOCK_50;",
        "  wire [3:0] KEY = 4'b1111;\n  wire clk = CLOCK_50;",
        1,
    );
    assert!(top.contains("4'b1111"), "examples/picorv32/src/top.v changed shape");
    std::fs::write(project.join("src/top.v"), top).unwrap();
    let manifest = std::fs::read_to_string(Path::new(ROOT).join("examples/zybo-picorv32/chipsmith.toml"))
        .unwrap()
        .replace("../picorv32/src/*.v", "src/*.v");
    let manifest: String = manifest.lines().filter(|l| !l.starts_with("KEY")).map(|l| format!("{l}\n")).collect();
    std::fs::write(project.join("chipsmith.toml"), manifest).unwrap();
    let dir = s.join("project");
    if dir.exists() {
        std::fs::remove_dir_all(&dir).unwrap();
    }
    copy_dir(&project, &dir);
    let out = Command::new(env!("CARGO_BIN_EXE_chipsmith"))
        .arg("-C")
        .arg(&dir)
        .arg("build")
        .env("CHIPSMITH_CACHE", "0")
        .output()
        .unwrap();
    assert!(out.status.success(), "build failed:\n{}", String::from_utf8_lossy(&out.stderr));
    eprintln!("{}", String::from_utf8_lossy(&out.stderr));
    let bit = PathBuf::from(String::from_utf8(out.stdout).unwrap().trim());
    prjxray_agrees(&bit, &s);
    let ex = example_at(&dir);
    check("zybo-picorv32-running", &ex, &bit, &s, 20000);
}

/// The Zybo's PL user I/O balls (prjxray-db's package pins, banks 34
/// and 35), for designs with pins of no board.
const BALLS: &str = "A20 B19 B20 C20 D18 D19 D20 E17 E18 E19 F16 F17 F19 F20 G14 G15 G17 G18 G19 G20 H15 H16 H17 H18 H20 J14 J15 J16 J18 J19 J20 K14 K16 K17 K18 K19 L14 L15 L16 L17 L19 L20 M14 M15 M17 M18 M19 M20 N15 N16 N17 N18 N20 P14 P15 P16 P18 P19 P20 R14 R16 R17 R18 R19 T10 T11 T12 T14 T15 T16 T17 T19 T20 U12 U13 U14 U15 U17 U18 U19 U20 V12 V13 V15 V16 V17 V18 V20 W13 W14 W15 W16 W18 W19 W20 Y14 Y16 Y17 Y18 Y19";

/// A project of one of chipsmith-xc7's test designs, every port bit on a
/// ball of its own (the clock on L16, the Zybo's clock pin).
fn design_project(file: &str, top: &str, dir: &Path) {
    setup();
    let src = Path::new(ROOT).join("crates/chipsmith-xc7/tests/designs").join(file);
    let file = Path::new(file).file_name().unwrap();
    std::fs::create_dir_all(dir.join("src")).unwrap();
    std::fs::copy(&src, dir.join("src").join(file)).unwrap();
    let d = support::frontend(top, &[src]);
    let m = support::top(&d);
    let mut balls = BALLS.split(' ').filter(|b| *b != "L16");
    let mut pins = String::new();
    for (_, w, _) in m.ports() {
        if w.name == "clk" {
            pins += "clk = \"L16\"\n";
            continue;
        }
        let bs: Vec<String> = (0..w.bits.len())
            .map(|_| format!("\"{}\"", balls.next().expect("enough balls")))
            .collect();
        if w.bits.len() == 1 {
            pins += &format!("{} = {}\n", w.name, bs[0]);
        } else {
            pins += &format!("{} = [{}]\n", w.name, bs.join(", "));
        }
    }
    std::fs::write(
        dir.join("chipsmith.toml"),
        format!(
            "[project]\nname = \"{top}\"\ntop = \"{top}\"\nsources = [\"src/*.v\"]\n\n[target]\ndevice = \"xc7z010clg400-1\"\n\n[pins]\n{pins}"
        ),
    )
    .unwrap();
}

fn design_built(file: &str, top: &str) {
    let s = scratch(top);
    let project = s.join("src_project");
    design_project(file, top, &project);
    let dir = s.join("project");
    if dir.exists() {
        std::fs::remove_dir_all(&dir).unwrap();
    }
    copy_dir(&project, &dir);
    let out = Command::new(env!("CARGO_BIN_EXE_chipsmith"))
        .arg("-C")
        .arg(&dir)
        .arg("build")
        .env("CHIPSMITH_CACHE", "0")
        .output()
        .unwrap();
    assert!(out.status.success(), "{top}: build failed:\n{}", String::from_utf8_lossy(&out.stderr));
    let bit = PathBuf::from(String::from_utf8(out.stdout).unwrap().trim());
    prjxray_agrees(&bit, &s);
    check(top, &example_at(&dir), &bit, &s, 3000);
}

/// A PLL's clocks (phase, x2, a 25% duty cycle) through the bitstream: its
/// counters decoded and modelled, against the request's model.
#[test]
fn cmt() {
    design_built("cmt.v", "cmt");
}

/// zybo-pll's two clocks.
#[test]
fn zybo_pll() {
    built("zybo-pll", 3000);
}

/// Every register shape and the carry arithmetic, through the bitstream.
#[test]
fn registers_and_arithmetic() {
    design_built("regs_arith.v", "regs_arith");
}

/// RAM32Ms of every shape synthesis makes, through the bitstream.
#[test]
fn distributed_ram() {
    design_built("lutram.v", "lutram");
}

/// Every block RAM shape (`chipsmith-xc7/tests/designs/bram`) whose ports
/// fit the package, through the bitstream; `CHIPSMITH_BRAM_SHAPE` picks
/// one.
#[test]
fn block_ram() {
    let mut failures = Vec::new();
    for top in [
        "bytes", "deep", "dualclock", "mixed", "outreg", "rom", "sdp", "sp_no_change", "sp_read_first",
        "sp_write_first", "srst", "tdp",
    ] {
        if std::env::var("CHIPSMITH_BRAM_SHAPE").is_ok_and(|s| s != top) {
            continue;
        }
        let r = std::panic::catch_unwind(|| design_built(&format!("bram/{top}.v"), top));
        if r.is_err() {
            failures.push(top);
        }
    }
    assert!(failures.is_empty(), "failed: {failures:?}");
}

/// Every multiplier shape (`chipsmith-xc7/tests/designs/dsp`) that fits the
/// balls through the bitstream (mul_wide's 128 port bits don't; mul_split
/// is its several-DSP case); `CHIPSMITH_DSP_SHAPE` picks one.
#[test]
fn dsp() {
    let mut failures = Vec::new();
    for top in ["mul_comb", "mul_mixed", "mul_pipe", "mul_regs", "mul_split"] {
        if std::env::var("CHIPSMITH_DSP_SHAPE").is_ok_and(|s| s != top) {
            continue;
        }
        let r = std::panic::catch_unwind(|| design_built(&format!("dsp/{top}.v"), top));
        if r.is_err() {
            failures.push(top);
        }
    }
    assert!(failures.is_empty(), "failed: {failures:?}");
}

/// `CHIPSMITH_XC7_BIT=<.bit> CHIPSMITH_XC7_EXAMPLE=<example>`: decode a
/// given bitstream of that example (nextpnr's, to check the decoder).
#[test]
fn decoder_reads_a_given_bitstream() {
    let (Ok(bit), Ok(name)) = (std::env::var("CHIPSMITH_XC7_BIT"), std::env::var("CHIPSMITH_XC7_EXAMPLE")) else {
        eprintln!("CHIPSMITH_XC7_BIT and CHIPSMITH_XC7_EXAMPLE unset: nothing to decode");
        return;
    };
    let s = scratch(&format!("given_{}", name.replace('/', "_")));
    let ex = if name.starts_with('/') {
        example_at(Path::new(&name))
    } else {
        example(&name)
    };
    let cycles = std::env::var("CHIPSMITH_XC7_CYCLES").ok().and_then(|c| c.parse().ok()).unwrap_or(3000);
    check(&name, &ex, Path::new(&bit), &s, cycles);
}
