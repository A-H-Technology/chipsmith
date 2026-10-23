//! `chipsmith.toml`. `Manifest::load` is the only place its rules are
//! enforced; a `Manifest` that exists is buildable as far as the file can say.

use std::collections::BTreeMap;
use std::fmt;
use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::device::{Device, UnknownDevice};

pub const FILE_NAME: &str = "chipsmith.toml";

#[derive(Debug)]
pub struct Manifest {
    /// Names the bitstream: `build/<name>.rbf`.
    pub name: String,
    /// The module (or entity) whose ports are the chip's pins.
    pub top: String,
    pub device: Device,
    pub sources: Sources,
    /// One entry per port bit, in the order the file lists them.
    pub pins: Vec<PinAssignment>,
}

/// The design's HDL, already expanded from globs to existing files. One
/// language per design: VHDL goes through GHDL into Verilog, and the two
/// can't be elaborated together yet.
#[derive(Debug, PartialEq, Eq)]
pub enum Sources {
    Verilog(Vec<PathBuf>),
    Vhdl(Vec<PathBuf>),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PinAssignment {
    pub port: PortBit,
    pub pin: Pin,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PortBit {
    Scalar(String),
    /// Bit `index` of a vector port; written `name[index]`.
    Bit(String, usize),
}

impl fmt::Display for PortBit {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            PortBit::Scalar(name) => f.write_str(name),
            PortBit::Bit(name, index) => write!(f, "{name}[{index}]"),
        }
    }
}

/// A package pin by its board-manual name, e.g. `PIN_N2`. Whether the
/// package has it is checked by place-and-route against the device database.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pin(String);

impl Pin {
    fn parse(s: &str) -> Result<Self, ManifestError> {
        match s.strip_prefix("PIN_") {
            Some(ball) if !ball.is_empty() && ball.chars().all(|c| c.is_ascii_alphanumeric()) => {
                Ok(Pin(s.to_string()))
            }
            _ => Err(ManifestError::BadPin(s.to_string())),
        }
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ManifestError {
    #[error("reading {path}: {source}")]
    Read {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("{path}: {source}")]
    Syntax {
        path: PathBuf,
        source: Box<toml::de::Error>,
    },
    #[error(transparent)]
    Device(#[from] UnknownDevice),
    #[error("bad source pattern {pattern:?}: {reason}")]
    BadPattern { pattern: String, reason: String },
    #[error("source pattern {0:?} matches no files")]
    NoMatch(String),
    #[error("[project] sources is empty")]
    NoSources,
    #[error("{0}: not a Verilog (.v) or VHDL (.vhd, .vhdl) file")]
    UnknownLanguage(PathBuf),
    #[error("sources mix Verilog ({verilog}) and VHDL ({vhdl}); use one language per design")]
    MixedLanguages { verilog: PathBuf, vhdl: PathBuf },
    #[error("bad pin {0:?}; pins are written like PIN_N2")]
    BadPin(String),
    #[error("pin {pin} is assigned to both {first} and {second}")]
    PinTwice {
        pin: String,
        first: PortBit,
        second: PortBit,
    },
}

/// The file as written; only exists to become a `Manifest`.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ManifestFile {
    project: ProjectTable,
    target: TargetTable,
    #[serde(default)]
    pins: BTreeMap<String, PinsEntry>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ProjectTable {
    name: String,
    top: String,
    sources: Vec<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct TargetTable {
    device: String,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum PinsEntry {
    Scalar(String),
    /// Index i of the list is bit i of the port.
    Bus(Vec<String>),
}

impl Manifest {
    pub fn load(project_dir: &Path) -> Result<Self, ManifestError> {
        let path = project_dir.join(FILE_NAME);
        let text = std::fs::read_to_string(&path).map_err(|source| ManifestError::Read {
            path: path.clone(),
            source,
        })?;
        Self::parse(&text, project_dir).map_err(|e| match e {
            ManifestError::Syntax { source, .. } => ManifestError::Syntax { path, source },
            e => e,
        })
    }

    /// `project_dir` anchors the source globs.
    pub fn parse(text: &str, project_dir: &Path) -> Result<Self, ManifestError> {
        let file: ManifestFile = toml::from_str(text).map_err(|source| ManifestError::Syntax {
            path: PathBuf::from(FILE_NAME),
            source: Box::new(source),
        })?;
        Ok(Manifest {
            name: file.project.name,
            top: file.project.top,
            device: file.target.device.parse()?,
            sources: expand_sources(project_dir, &file.project.sources)?,
            pins: flatten_pins(file.pins)?,
        })
    }
}

fn expand_sources(project_dir: &Path, patterns: &[String]) -> Result<Sources, ManifestError> {
    let mut files = Vec::new();
    for pattern in patterns {
        let anchored = project_dir.join(pattern);
        let matches =
            glob::glob(&anchored.to_string_lossy()).map_err(|e| ManifestError::BadPattern {
                pattern: pattern.clone(),
                reason: e.to_string(),
            })?;
        let before = files.len();
        // An unreadable directory entry is the same as no entry here.
        files.extend(matches.filter_map(Result::ok).filter(|p| p.is_file()));
        if files.len() == before {
            return Err(ManifestError::NoMatch(pattern.clone()));
        }
    }

    let (mut verilog, mut vhdl) = (Vec::new(), Vec::new());
    for file in files {
        match file.extension().and_then(|e| e.to_str()) {
            Some("v") => verilog.push(file),
            Some("vhd" | "vhdl") => vhdl.push(file),
            _ => return Err(ManifestError::UnknownLanguage(file)),
        }
    }
    match (verilog.is_empty(), vhdl.is_empty()) {
        (true, true) => Err(ManifestError::NoSources),
        (false, true) => Ok(Sources::Verilog(verilog)),
        (true, false) => Ok(Sources::Vhdl(vhdl)),
        (false, false) => Err(ManifestError::MixedLanguages {
            verilog: verilog.swap_remove(0),
            vhdl: vhdl.swap_remove(0),
        }),
    }
}

fn flatten_pins(table: BTreeMap<String, PinsEntry>) -> Result<Vec<PinAssignment>, ManifestError> {
    let mut assignments: Vec<PinAssignment> = Vec::new();
    for (port, entry) in table {
        let bits: Vec<(PortBit, &String)> = match &entry {
            PinsEntry::Scalar(pin) => vec![(PortBit::Scalar(port.clone()), pin)],
            PinsEntry::Bus(pins) => pins
                .iter()
                .enumerate()
                .map(|(i, pin)| (PortBit::Bit(port.clone(), i), pin))
                .collect(),
        };
        for (port, pin) in bits {
            let pin = Pin::parse(pin)?;
            if let Some(first) = assignments.iter().find(|a| a.pin == pin) {
                return Err(ManifestError::PinTwice {
                    pin: pin.0,
                    first: first.port.clone(),
                    second: port,
                });
            }
            assignments.push(PinAssignment { port, pin });
        }
    }
    Ok(assignments)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str, files: &[&str]) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("chipsmith-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("src")).unwrap();
        for f in files {
            std::fs::write(dir.join(f), "").unwrap();
        }
        dir
    }

    const DE2: &str = r#"
        [project]
        name = "blinky"
        top = "blinky"
        sources = ["src/*"]

        [target]
        device = "EP2C35F672C6"

        [pins]
        CLOCK_50 = "PIN_N2"
        LEDR = ["PIN_AE23", "PIN_AF23"]
    "#;

    #[test]
    fn a_bus_entry_assigns_list_index_i_to_port_bit_i() {
        let dir = scratch("bus", &["src/blinky.v"]);
        let m = Manifest::parse(DE2, &dir).unwrap();
        let pins: Vec<_> = m
            .pins
            .iter()
            .map(|a| format!("{} {}", a.port, a.pin.as_str()))
            .collect();
        assert_eq!(
            pins,
            ["CLOCK_50 PIN_N2", "LEDR[0] PIN_AE23", "LEDR[1] PIN_AF23"]
        );
        assert_eq!(m.sources, Sources::Verilog(vec![dir.join("src/blinky.v")]));
    }

    #[test]
    fn verilog_and_vhdl_in_one_design_is_rejected() {
        let dir = scratch("mixed", &["src/a.v", "src/b.vhd"]);
        assert!(matches!(
            Manifest::parse(DE2, &dir),
            Err(ManifestError::MixedLanguages { .. })
        ));
    }

    #[test]
    fn a_pattern_matching_nothing_is_an_error_not_an_empty_design() {
        let dir = scratch("nomatch", &[]);
        assert!(matches!(
            Manifest::parse(DE2, &dir),
            Err(ManifestError::NoMatch(_))
        ));
    }

    #[test]
    fn a_pin_used_twice_is_rejected() {
        let dir = scratch("twice", &["src/a.v"]);
        let text = DE2.replace("PIN_AF23", "PIN_N2");
        assert!(matches!(
            Manifest::parse(&text, &dir),
            Err(ManifestError::PinTwice { .. })
        ));
    }

    #[test]
    fn unknown_devices_and_keys_are_rejected() {
        let dir = scratch("unknown", &["src/a.v"]);
        let other_part = DE2.replace("EP2C35F672C6", "EP4CE22F17C6");
        assert!(matches!(
            Manifest::parse(&other_part, &dir),
            Err(ManifestError::Device(_))
        ));
        // A key chipsmith would silently ignore (no timing model yet) is an error.
        let clocks = format!("{DE2}\n[clocks]\nCLOCK_50 = \"50 MHz\"\n");
        assert!(matches!(
            Manifest::parse(&clocks, &dir),
            Err(ManifestError::Syntax { .. })
        ));
    }
}
