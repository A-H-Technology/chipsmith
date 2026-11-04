//! Quality of results doesn't slide back: a fresh build of an example,
//! through the `chipsmith` binary, routes to at most a bound on its
//! estimated critical path, or takes at most so many LEs. The bounds sit a
//! little above what branch `qor3` builds (docs/vs-quartus.md, round 3) and
//! below what came before, so losing the sums ordered by arrival (sha256,
//! femtorv32), the shared sums or the synchronous load (picorv32's and
//! sha256's LEs) fails here, as does losing `qor2`'s routing. On the Zybo
//! the bounds are on the estimate (fitted to Vivado, ADR-0016) and sit
//! between branch `xc7-qor`'s and what came before. The build is
//! deterministic, so a bound only moves when the flow does. Needs
//! `CHIPSMITH_CYCLONE2_DB` and `CHIPSMITH_PRJXRAY_DB`: run in the devshell.
//! sha256 takes a few minutes on either board.

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

/// A fresh build of `examples/<example>`'s report.
fn build(example: &str) -> String {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR"))
        .join("route-quality")
        .join(example);
    if dir.exists() {
        std::fs::remove_dir_all(&dir).unwrap();
    }
    let examples = Path::new(ROOT).join("examples");
    copy_dir(&examples.join(example), &dir);
    // The examples its manifest takes sources from ("../sha256/src/*.v").
    let manifest = std::fs::read_to_string(dir.join("chipsmith.toml")).unwrap();
    for sibling in manifest.split("\"../").skip(1).filter_map(|s| s.split('/').next()) {
        copy_dir(&examples.join(sibling), &dir.parent().unwrap().join(sibling));
    }
    let out = Command::new(env!("CARGO_BIN_EXE_chipsmith"))
        .arg("-C")
        .arg(&dir)
        .arg("build")
        .env("CHIPSMITH_CACHE", "0")
        .output()
        .unwrap();
    let report = String::from_utf8_lossy(&out.stderr).to_string() + &String::from_utf8_lossy(&out.stdout);
    assert!(out.status.success(), "{example}: build failed:\n{report}");
    report
}

/// The number after `before` on the first line that starts with `line`.
fn number(example: &str, report: &str, line: &str, before: &str) -> f64 {
    report
        .lines()
        .find(|l| l.trim_start().starts_with(line))
        .and_then(|l| l.split(before).nth(1))
        .and_then(|s| s.split(' ').next())
        .and_then(|n| n.parse().ok())
        .unwrap_or_else(|| panic!("{example}: no \"{line}...{before}\" in\n{report}"))
}

/// The routed critical path of a build's report, ns.
fn routed_critical(example: &str, report: &str) -> f64 {
    // "routed <n> nets, <n> pips, <n> iterations in <s> s; critical path <ns> ns (estimate)"
    number(example, report, "routed ", "critical path ")
}

/// A build's LEs, from "placed <n> LEs (<n> carry chains), ...".
fn les(example: &str, report: &str) -> f64 {
    number(example, report, "placed ", "placed ")
}

fn at_most(example: &str, what: &str, value: f64, (bound, before): (f64, f64)) {
    eprintln!("{example}: {what} {value:.2} (bound {bound}, before {before})");
    assert!(value <= bound, "{example}: {what} {value:.2}, over {bound} (before: {before})");
}

#[test]
fn femtorv32() {
    let report = build("femtorv32");
    at_most("femtorv32", "routed critical path, ns", routed_critical("femtorv32", &report), FEMTORV32);
}

#[test]
fn sha256() {
    let report = build("sha256");
    at_most("sha256", "routed critical path, ns", routed_critical("sha256", &report), SHA256);
    at_most("sha256", "LEs", les("sha256", &report), SHA256_LES);
}

/// A Zybo build's critical path for `clock`, ns, from "clock <name>: <ns>
/// ns, Fmax ...".
fn clock_ns(example: &str, report: &str, clock: &str) -> f64 {
    number(example, report, &format!("clock {clock}:"), &format!("clock {clock}: "))
}

#[test]
fn zybo_sha256() {
    let report = build("zybo-sha256");
    at_most("zybo-sha256", "critical path, ns", clock_ns("zybo-sha256", &report, "CLOCK_50"), ZYBO_SHA256);
}

#[test]
fn zybo_jtag_counter() {
    let report = build("zybo-jtag-counter");
    at_most("zybo-jtag-counter", "critical path, ns", clock_ns("zybo-jtag-counter", &report, "clk"), ZYBO_JTAG_COUNTER);
}

#[test]
fn picorv32_les() {
    let report = build("picorv32");
    at_most("picorv32", "LEs", les("picorv32", &report), PICORV32_LES);
}

/// (bound, before), ns: `open` 80351e1's (sums as written; femtorv32
/// 13.76, sha256 19.27) above, `qor3`'s (12.99, 13.76) below.
const FEMTORV32: (f64, f64) = (13.6, 13.76);
const SHA256: (f64, f64) = (15.0, 19.27);
/// (bound, before), LEs: `qor3`'s picorv32 875 (971 without shared sums,
/// 916 without the synchronous load), sha256 3,422 (3,672 without the
/// load, 3,538 without its constant legs).
const PICORV32_LES: (f64, f64) = (890.0, 971.0);
const SHA256_LES: (f64, f64) = (3440.0, 3672.0);
/// (bound, before), ns, the default seed: `open` df16e49's (sums as
/// written, every register one, LUT pins by criticality alone: sha256
/// 18.62, jtag-counter 3.03) above, `xc7-qor`'s (13.76, 2.79) below; sums
/// by arrival without the register copies read 14.41 on sha256.
const ZYBO_SHA256: (f64, f64) = (14.0, 18.62);
const ZYBO_JTAG_COUNTER: (f64, f64) = (2.9, 3.03);
