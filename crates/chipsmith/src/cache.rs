//! Skipping a build step whose inputs haven't changed (ADR-0011).
//!
//! Each in-process step of a build leaves a stamp next to its outputs:
//! a key (this binary, the `CHIPSMITH_*` environment, the step and its
//! settings), the content hash of every file the step read, and the
//! lines the step reported. A later build whose step has the same key,
//! the same input contents and all of its outputs still there replays
//! the lines instead of running the step. Inputs are hashed, not dated,
//! so a step whose input came out byte-identical (a comment edit doesn't
//! change the netlist) is skipped too, and so is everything after it.

use std::cell::RefCell;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use anyhow::{Context, Result};

/// The first line of a stamp; a stamp of another format is a miss.
const FORMAT: &str = "chipsmith stamp 1";

/// `CHIPSMITH_CACHE=0`: run every step.
pub const CACHE_VAR: &str = "CHIPSMITH_CACHE";

thread_local! {
    /// What the running step reported, for its stamp.
    static REPORT: RefCell<Vec<String>> = const { RefCell::new(Vec::new()) };
}

/// Print one line of a step's report (indented under the step's label)
/// and keep it for the step's stamp.
pub fn report(line: String) {
    eprintln!("    {line}");
    REPORT.with(|r| r.borrow_mut().push(line));
}

/// `say!("...", args)`: `cache::report(format!(...))`.
macro_rules! say {
    ($($arg:tt)*) => {
        $crate::cache::report(format!($($arg)*))
    };
}
pub(crate) use say;

/// 64-bit SipHash with fixed keys: stable for one build of chipsmith,
/// which is all a key needs, since the binary is part of every key.
fn hasher() -> std::collections::hash_map::DefaultHasher {
    std::collections::hash_map::DefaultHasher::new()
}

fn hash_file(path: &Path) -> Option<u64> {
    let bytes = std::fs::read(path).ok()?;
    let mut h = hasher();
    bytes.hash(&mut h);
    Some(h.finish())
}

/// This executable's contents: a rebuilt chipsmith may build differently.
fn binary() -> u64 {
    static HASH: OnceLock<u64> = OnceLock::new();
    *HASH.get_or_init(|| {
        std::env::current_exe()
            .ok()
            .and_then(|p| hash_file(&p))
            .unwrap_or(0)
    })
}

/// What a step's outcome depends on besides its input files: this
/// binary, every `CHIPSMITH_*` variable (the databases' store paths, the
/// placer's and router's settings) and the step itself, by `what`
/// (its `Debug` form: paths and settings).
pub fn key(what: &str) -> u64 {
    let mut env: Vec<(String, String)> = std::env::vars()
        .filter(|(k, _)| k.starts_with("CHIPSMITH_") && k != CACHE_VAR)
        .collect();
    env.sort();
    let mut h = hasher();
    binary().hash(&mut h);
    env.hash(&mut h);
    what.hash(&mut h);
    h.finish()
}

/// A step's stamp: `<first output>.stamp`.
pub struct Stamp {
    path: PathBuf,
    key: u64,
    outputs: Vec<PathBuf>,
}

impl Stamp {
    pub fn new(key: u64, outputs: &[&Path]) -> Self {
        let first = outputs.first().expect("a step writes something");
        let mut path = first.as_os_str().to_owned();
        path.push(".stamp");
        Stamp {
            path: path.into(),
            key,
            outputs: outputs.iter().map(|p| p.to_path_buf()).collect(),
        }
    }

    /// The report lines of the run this stamp records, if it still holds:
    /// same key, every output there, every input as it was.
    pub fn check(&self) -> Option<Vec<String>> {
        if std::env::var(CACHE_VAR).is_ok_and(|v| v == "0") {
            return None;
        }
        let text = std::fs::read_to_string(&self.path).ok()?;
        let mut lines = text.lines();
        if lines.next()? != FORMAT || lines.next()? != format!("key {:016x}", self.key) {
            return None;
        }
        if !self.outputs.iter().all(|o| o.is_file()) {
            return None;
        }
        let mut report = Vec::new();
        for line in lines {
            if let Some(rest) = line.strip_prefix("input ") {
                let (hash, path) = rest.split_once(' ')?;
                let hash = u64::from_str_radix(hash, 16).ok()?;
                if hash_file(Path::new(path)) != Some(hash) {
                    return None;
                }
            } else {
                report.push(line.strip_prefix("said ")?.to_string());
            }
        }
        Some(report)
    }

    /// Run `step` unless the stamp still holds, then record the files it
    /// says it read and what it reported.
    pub fn run(&self, step: impl FnOnce() -> Result<Vec<PathBuf>>) -> Result<()> {
        if let Some(report) = self.check() {
            eprintln!("    up to date ({})", self.path.display());
            for line in report {
                eprintln!("    {line}");
            }
            return Ok(());
        }
        // A step that fails halfway leaves no stamp to trust.
        let _ = std::fs::remove_file(&self.path);
        REPORT.with(|r| r.borrow_mut().clear());
        let inputs = step()?;
        let report = REPORT.with(|r| std::mem::take(&mut *r.borrow_mut()));
        let mut text = format!("{FORMAT}\nkey {:016x}\n", self.key);
        for input in inputs {
            // Unreadable now means it won't match later: no stamp.
            let Some(hash) = hash_file(&input) else {
                return Ok(());
            };
            text += &format!("input {hash:016x} {}\n", input.display());
        }
        for line in report {
            text += &format!("said {line}\n");
        }
        std::fs::write(&self.path, text).with_context(|| format!("writing {}", self.path.display()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A scratch directory of its own per test.
    fn dir(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("chipsmith-cache-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    /// Runs the step "copy `input` to `output`" through a stamp; true if
    /// it ran.
    fn copy(input: &Path, output: &Path, key: u64) -> bool {
        let mut ran = false;
        Stamp::new(key, &[output])
            .run(|| {
                ran = true;
                std::fs::copy(input, output)?;
                say!("copied");
                Ok(vec![input.to_path_buf()])
            })
            .unwrap();
        ran
    }

    #[test]
    fn a_step_reruns_only_when_its_input_or_key_changes() {
        let d = dir("rerun");
        let (input, output) = (d.join("in"), d.join("out"));
        std::fs::write(&input, "a").unwrap();
        assert!(copy(&input, &output, 1));
        assert!(!copy(&input, &output, 1), "nothing changed");
        assert_eq!(Stamp::new(1, &[&output]).check(), Some(vec!["copied".to_string()]));
        // Same contents, written again: still up to date.
        std::fs::write(&input, "a").unwrap();
        assert!(!copy(&input, &output, 1));
        std::fs::write(&input, "b").unwrap();
        assert!(copy(&input, &output, 1), "the input changed");
        assert!(copy(&input, &output, 2), "the key changed");
        std::fs::remove_file(&output).unwrap();
        assert!(copy(&input, &output, 2), "the output is gone");
    }

    #[test]
    fn a_failed_step_leaves_no_stamp() {
        let d = dir("failed");
        let (input, output) = (d.join("in"), d.join("out"));
        std::fs::write(&input, "a").unwrap();
        assert!(copy(&input, &output, 1));
        let failed = Stamp::new(3, &[&output]).run(|| anyhow::bail!("no"));
        assert!(failed.is_err());
        assert!(Stamp::new(1, &[&output]).check().is_none());
    }

    #[test]
    fn the_key_covers_the_chipsmith_environment() {
        let a = key("step");
        assert_eq!(a, key("step"));
        assert_ne!(a, key("other step"));
    }
}
