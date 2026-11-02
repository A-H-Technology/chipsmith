//! The whole DE2 and Zybo flows are deterministic: an example built twice,
//! and on 1 and 8 threads, gives the same RBF or `.bit` byte for byte. Global placement
//! claims it (docs/gpu-pnr.md, "Determinism": fixed-order sums, density in
//! fixed point), the anneal picks the best of a fixed 8 seeds by a total
//! order, and the router doesn't depend on thread timing; this holds the
//! flow to it, synthesis to bitstream, through the `chipsmith` binary.
//!
//! Each build runs in a fresh copy of the example with the build cache
//! off (`CHIPSMITH_CACHE=0`), with `RAYON_NUM_THREADS` setting the global
//! placer's threads. The GPU engine (`CHIPSMITH_ACCEL=cuda`) isn't covered:
//! it needs the `gpu` feature and a GPU, and only repeats on the same one.
//! Needs `CHIPSMITH_CYCLONE2_DB`: run in the devshell. sha256 takes a few
//! minutes; the rest seconds each.

use std::path::{Path, PathBuf};
use std::process::Command;

const ROOT: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../..");

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

/// Builds a fresh copy of `examples/<example>` as run `run` with the
/// global placer on `threads` threads and `env` set; returns its RBF (a
/// DE2's) or `.bit` (a Zybo's).
fn build(example: &str, run: &str, threads: usize, env: &[(&str, &str)]) -> Vec<u8> {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR"))
        .join("determinism")
        .join(example)
        .join(run);
    if dir.exists() {
        std::fs::remove_dir_all(&dir).unwrap();
    }
    let src = Path::new(ROOT).join("examples").join(example);
    copy_dir(&src, &dir);
    // Beside the examples its sources point into (`../picorv32/src/*.v`).
    let manifest = std::fs::read_to_string(src.join("chipsmith.toml")).unwrap();
    for sibling in manifest.split("\"../").skip(1).filter_map(|s| s.split('/').next()) {
        let to = dir.parent().unwrap().join(sibling);
        if !to.exists() {
            copy_dir(&Path::new(ROOT).join("examples").join(sibling), &to);
        }
    }
    let out = Command::new(env!("CARGO_BIN_EXE_chipsmith"))
        .arg("-C")
        .arg(&dir)
        .arg("build")
        .env("CHIPSMITH_CACHE", "0")
        .env("RAYON_NUM_THREADS", threads.to_string())
        .envs(env.iter().copied())
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{example} ({run}): build failed:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let manifest = std::fs::read_to_string(dir.join("chipsmith.toml")).unwrap();
    let name = manifest
        .lines()
        .find_map(|l| l.strip_prefix("name = \""))
        .and_then(|l| l.strip_suffix('"'))
        .expect("a project name");
    let build_dir = dir.join("build");
    let rbf = build_dir.join(format!("{name}.rbf"));
    let bitstream = if rbf.exists() {
        rbf
    } else {
        build_dir.join(format!("{name}.bit"))
    };
    std::fs::read(&bitstream)
        .unwrap_or_else(|e| panic!("{example} ({run}): no bitstream: {e}"))
}

fn deterministic(example: &str) {
    deterministic_with(example, &[]);
}

fn deterministic_with(example: &str, env: &[(&str, &str)]) {
    let t = std::time::Instant::now();
    let first = build(example, "8-threads", 8, env);
    let again = build(example, "8-threads-again", 8, env);
    let one = build(example, "1-thread", 1, env);
    let differ = |a: &[u8], b: &[u8]| a.iter().zip(b).filter(|(x, y)| x != y).count();
    assert!(
        first == again,
        "{example}: two builds on 8 threads differ in {} bytes",
        differ(&first, &again)
    );
    assert!(
        first == one,
        "{example}: 8 threads and 1 differ in {} bytes",
        differ(&first, &one)
    );
    eprintln!(
        "{example}: three builds, one bitstream ({} bytes), in {:.0?}",
        first.len(),
        t.elapsed()
    );
}

#[test]
fn blinky_verilog() {
    deterministic("blinky-verilog");
}

#[test]
fn uart() {
    deterministic("uart");
}

#[test]
fn i2c_master() {
    deterministic("i2c-master");
}

/// M4K blocks and an LE-only self-test around them.
#[test]
fn m4k_selftest() {
    deterministic("m4k-selftest");
}

/// Embedded multipliers.
#[test]
fn mult_selftest() {
    deterministic("mult-selftest");
}

/// Falling-edge registers.
#[test]
fn negedge_selftest() {
    deterministic("negedge-selftest");
}

/// A PLL.
#[test]
fn de2_pll_counter() {
    deterministic("de2-pll-counter");
}

/// I/O-cell registers with their enables, clears and presets.
#[test]
fn de2_ioregs() {
    deterministic("de2-ioregs");
}

#[test]
fn picorv32() {
    deterministic("picorv32");
}

/// The largest example but picorv32x24: 4,242 LEs.
#[test]
fn sha256() {
    deterministic("sha256");
}

/// The Zybo flow: synthesis to the `.bit`, a BSCANE2 and two clocks.
#[test]
fn zybo_jtag_counter() {
    deterministic("zybo-jtag-counter");
}

/// RAM32Ms; and, forced, the electrostatic global placement the Zybo
/// placer starts from in large designs.
#[test]
fn zybo_picorv32() {
    deterministic("zybo-picorv32");
    deterministic_with("zybo-picorv32", &[("CHIPSMITH_XC7_PLACER", "electro")]);
}
