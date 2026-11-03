//! Quality of results doesn't slide back: a fresh build of an example,
//! through the `chipsmith` binary, routes to at most a bound on its
//! estimated critical path, or takes at most so many LEs. The bounds sit a
//! little above what branch `qor3` builds (docs/vs-quartus.md, round 3) and
//! below what came before, so losing the sums ordered by arrival (sha256,
//! femtorv32) or the shared sums (picorv32's LEs) fails here, as does
//! losing `qor2`'s routing. The build is deterministic, so a bound only
//! moves when the flow does. Needs `CHIPSMITH_CYCLONE2_DB`: run in the
//! devshell. sha256 takes a few minutes.

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
    copy_dir(&Path::new(ROOT).join("examples").join(example), &dir);
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

/// The routed critical path of a fresh build of `examples/<example>`, ns.
fn routed_critical(example: &str) -> f64 {
    let report = build(example);
    // "routed <n> nets, <n> pips, <n> iterations in <s> s; critical path <ns> ns (estimate)"
    number(example, &report, "routed ", "critical path ")
}

fn at_most(example: &str, (bound, before): (f64, f64)) {
    let ns = routed_critical(example);
    eprintln!("{example}: routed critical path {ns:.2} ns (bound {bound}, before {before})");
    assert!(ns <= bound, "{example}: routed critical path {ns:.2} ns, over {bound} (before: {before})");
}

#[test]
fn femtorv32() {
    at_most("femtorv32", FEMTORV32);
}

#[test]
fn sha256() {
    at_most("sha256", SHA256);
}

#[test]
fn picorv32_les() {
    // "placed <n> LEs (<n> carry chains), ..."
    let les = number("picorv32", &build("picorv32"), "placed ", "placed ");
    let (bound, before) = PICORV32_LES;
    eprintln!("picorv32: {les} LEs (bound {bound}, before {before})");
    assert!(les <= bound, "picorv32: {les} LEs, over {bound} (before: {before})");
}

/// (bound, before), ns: `open` 80351e1's (sums as written; femtorv32
/// 13.76, sha256 19.27) above, `qor3`'s (13.05, 13.98) below.
const FEMTORV32: (f64, f64) = (13.6, 13.76);
const SHA256: (f64, f64) = (15.0, 19.27);
/// (bound, before): `qor3` 916 without shared sums 971.
const PICORV32_LES: (f64, f64) = (930.0, 971.0);
