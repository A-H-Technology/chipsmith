//! The router's critical paths don't slide back: a fresh build of an
//! example, through the `chipsmith` binary, routes to at most a bound on
//! its estimated critical path. The bounds sit a little above what branch
//! `qor2` routes (docs/vs-quartus.md, "Routing round 2") and below what
//! `open` 3ba072c did, so losing the slower present-cost growth or the
//! critical-path repair fails here. The build is deterministic, so a
//! bound only moves when the flow does. Needs `CHIPSMITH_CYCLONE2_DB`: run
//! in the devshell. sha256 takes a few minutes.

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

/// The routed critical path of a fresh build of `examples/<example>`, ns.
fn routed_critical(example: &str) -> f64 {
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
    // "routed <n> nets, <n> pips, <n> iterations in <s> s; critical path <ns> ns (estimate)"
    report
        .lines()
        .find(|l| l.trim_start().starts_with("routed "))
        .and_then(|l| l.split("critical path ").nth(1))
        .and_then(|s| s.split(' ').next())
        .and_then(|ns| ns.parse().ok())
        .unwrap_or_else(|| panic!("{example}: no routed critical path in\n{report}"))
}

fn at_most(example: &str, bound: f64, before: f64) {
    let ns = routed_critical(example);
    eprintln!("{example}: routed critical path {ns:.2} ns (bound {bound}, open 3ba072c {before})");
    assert!(ns <= bound, "{example}: routed critical path {ns:.2} ns, over {bound} (open 3ba072c: {before})");
}

#[test]
fn femtorv32() {
    at_most("femtorv32", FEMTORV32.0, FEMTORV32.1);
}

#[test]
fn sha256() {
    at_most("sha256", SHA256.0, SHA256.1);
}

/// (bound, `open` 3ba072c's), ns.
const FEMTORV32: (f64, f64) = (14.0, 14.31);
const SHA256: (f64, f64) = (19.6, 22.58);
