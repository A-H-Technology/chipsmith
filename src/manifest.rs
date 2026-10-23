//! `chipsmith.toml`. `Manifest::load` is the only place its rules are
//! enforced; a `Manifest` that exists is buildable as far as the file can say.

use std::collections::BTreeMap;
use std::fmt;
use std::num::NonZeroU64;
use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::device::{Clg400Pin, Device, UnknownDevice};

pub const FILE_NAME: &str = "chipsmith.toml";

#[derive(Debug)]
pub struct Manifest {
    /// Names the bitstream: `build/<name>.rbf`.
    pub name: String,
    /// The module (or entity) whose ports are the chip's pins.
    pub top: String,
    pub sources: Sources,
    pub target: Target,
}

/// The device, its pin assignments (one entry per port bit in the order
/// the file lists them) and its clock constraints. Each device writes its
/// pins its own way, so the pin type is per device: a Zynq ball on a
/// Cyclone II can't be represented.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Target {
    Ep2c35f672c6 {
        pins: Vec<PinAssignment<QuartusPin>>,
        /// chipsmith's Cyclone II timing model times one clock domain, so
        /// there is at most one clock to constrain.
        clock: Option<Clock>,
    },
    Xc7z010clg400_1 {
        pins: Vec<PinAssignment<Clg400Pin>>,
        clocks: Vec<Clock>,
    },
}

impl Target {
    pub fn device(&self) -> Device {
        match self {
            Target::Ep2c35f672c6 { .. } => Device::Ep2c35f672c6,
            Target::Xc7z010clg400_1 { .. } => Device::Xc7z010clg400_1,
        }
    }
}

/// A clock entering at a pin, and the frequency it runs at.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Clock {
    /// A port bit with a pin assignment.
    pub port: PortBit,
    pub frequency: Frequency,
}

/// A clock frequency, written `"125 MHz"` (up to six decimals: whole Hz).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Frequency(NonZeroU64);

impl Frequency {
    fn parse(s: &str) -> Option<Self> {
        let number = s.strip_suffix("MHz")?.trim_end();
        let (whole, fraction) = number.split_once('.').unwrap_or((number, ""));
        let digits = |d: &str| d.bytes().all(|b| b.is_ascii_digit());
        if whole.is_empty() || !digits(whole) || !digits(fraction) || fraction.len() > 6 {
            return None;
        }
        let hz = whole.parse::<u64>().ok()?.checked_mul(1_000_000)?
            + format!("{fraction:0<6}").parse::<u64>().ok()?;
        NonZeroU64::new(hz).map(Frequency)
    }

    pub fn hz(self) -> u64 {
        self.0.get()
    }

    pub fn mhz(self) -> f64 {
        self.hz() as f64 / 1e6
    }

    pub fn period_ns(self) -> f64 {
        1e9 / self.hz() as f64
    }
}

impl fmt::Display for Frequency {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} MHz", self.mhz())
    }
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
pub struct PinAssignment<P> {
    pub port: PortBit,
    pub pin: P,
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

/// A Cyclone II package pin by its Quartus name, e.g. `PIN_N2`. Whether the
/// package has it is checked by place-and-route against the device database.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QuartusPin(String);

impl QuartusPin {
    fn parse(s: &str) -> Result<Self, ManifestError> {
        match s.strip_prefix("PIN_") {
            Some(ball) if !ball.is_empty() && ball.chars().all(|c| c.is_ascii_alphanumeric()) => {
                Ok(QuartusPin(s.to_string()))
            }
            _ => Err(ManifestError::BadPin {
                pin: s.to_string(),
                device: Device::Ep2c35f672c6,
                expected: "pins are written like PIN_N2",
            }),
        }
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for QuartusPin {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
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
    #[error("{device} has no user I/O pin {pin:?}; {expected}")]
    BadPin {
        pin: String,
        device: Device,
        expected: &'static str,
    },
    #[error("[clocks] {port}: {value:?} is not a frequency like \"50 MHz\"")]
    BadFrequency { port: String, value: String },
    #[error("[clocks] {0}: a clock enters at a pin, and [pins] assigns none to {0}")]
    ClockWithoutPin(String),
    #[error("[clocks] has {0} clocks; the {1} timing model times one clock domain")]
    TooManyClocks(usize, Device),
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
    /// Port bit (as in error messages: `clk`, `bus[0]`) to frequency.
    #[serde(default)]
    clocks: BTreeMap<String, String>,
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
        let device: Device = file.target.device.parse()?;
        let target = match device {
            Device::Ep2c35f672c6 => {
                let pins = flatten_pins(file.pins, QuartusPin::parse)?;
                let mut clocks = parse_clocks(&file.clocks, &pins)?;
                if clocks.len() > 1 {
                    return Err(ManifestError::TooManyClocks(clocks.len(), device));
                }
                Target::Ep2c35f672c6 {
                    pins,
                    clock: clocks.pop(),
                }
            }
            Device::Xc7z010clg400_1 => {
                let pins = flatten_pins(file.pins, |s| {
                    Clg400Pin::parse(s).ok_or_else(|| ManifestError::BadPin {
                        pin: s.to_string(),
                        device,
                        expected: "pins are the package's programmable-logic balls, like L16 (PS/MIO pins are not)",
                    })
                })?;
                let clocks = parse_clocks(&file.clocks, &pins)?;
                Target::Xc7z010clg400_1 { pins, clocks }
            }
        };
        Ok(Manifest {
            name: file.project.name,
            top: file.project.top,
            sources: expand_sources(project_dir, &file.project.sources)?,
            target,
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

fn parse_clocks<P>(
    table: &BTreeMap<String, String>,
    pins: &[PinAssignment<P>],
) -> Result<Vec<Clock>, ManifestError> {
    table
        .iter()
        .map(|(port, value)| {
            let port = pins
                .iter()
                .map(|a| &a.port)
                .find(|p| p.to_string() == *port)
                .ok_or_else(|| ManifestError::ClockWithoutPin(port.clone()))?;
            let frequency = Frequency::parse(value).ok_or_else(|| ManifestError::BadFrequency {
                port: port.to_string(),
                value: value.clone(),
            })?;
            Ok(Clock {
                port: port.clone(),
                frequency,
            })
        })
        .collect()
}

fn flatten_pins<P: PartialEq + fmt::Display>(
    table: BTreeMap<String, PinsEntry>,
    parse: impl Fn(&str) -> Result<P, ManifestError>,
) -> Result<Vec<PinAssignment<P>>, ManifestError> {
    let mut assignments: Vec<PinAssignment<P>> = Vec::new();
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
            let pin = parse(pin)?;
            if let Some(first) = assignments.iter().find(|a| a.pin == pin) {
                return Err(ManifestError::PinTwice {
                    pin: pin.to_string(),
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
        let Target::Ep2c35f672c6 { pins, .. } = &m.target else {
            panic!("{:?} is not the DE2's part", m.target);
        };
        let pins: Vec<_> = pins
            .iter()
            .map(|a| format!("{} {}", a.port, a.pin))
            .collect();
        assert_eq!(
            pins,
            ["CLOCK_50 PIN_N2", "LEDR[0] PIN_AE23", "LEDR[1] PIN_AF23"]
        );
        assert_eq!(m.sources, Sources::Verilog(vec![dir.join("src/blinky.v")]));
    }

    const ZYBO: &str = r#"
        [project]
        name = "blinky"
        top = "blinky"
        sources = ["src/*"]

        [target]
        device = "xc7z010clg400-1"

        [pins]
        clk = "L16"
        led = ["M14", "M15"]
    "#;

    #[test]
    fn a_device_takes_its_own_pin_names_only() {
        let dir = scratch("zybo", &["src/blinky.v"]);
        let m = Manifest::parse(ZYBO, &dir).unwrap();
        let Target::Xc7z010clg400_1 { pins, .. } = &m.target else {
            panic!("{:?} is not the Zybo's part", m.target);
        };
        let pins: Vec<_> = pins
            .iter()
            .map(|a| format!("{} {}", a.port, a.pin))
            .collect();
        assert_eq!(pins, ["clk L16", "led[0] M14", "led[1] M15"]);

        // A DE2 pin on the Zynq, a Zynq ball on the DE2, and the PS pin the
        // Zybo's USB-UART is wired to.
        for text in [
            ZYBO.replace("\"L16\"", "\"PIN_N2\""),
            DE2.replace("PIN_N2", "L16"),
            ZYBO.replace("\"L16\"", "\"B12\""),
        ] {
            assert!(
                matches!(
                    Manifest::parse(&text, &dir),
                    Err(ManifestError::BadPin { .. })
                ),
                "{text}"
            );
        }
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
        // A key chipsmith would silently ignore is an error.
        let typo = format!("{DE2}\n[clock]\nCLOCK_50 = \"50 MHz\"\n");
        assert!(matches!(
            Manifest::parse(&typo, &dir),
            Err(ManifestError::Syntax { .. })
        ));
    }

    #[test]
    fn frequencies_are_whole_hz_in_mhz() {
        let f = |s| Frequency::parse(s).map(Frequency::hz);
        assert_eq!(f("125 MHz"), Some(125_000_000));
        assert_eq!(f("12.5MHz"), Some(12_500_000));
        assert_eq!(f("0.000001 MHz"), Some(1));
        for bad in [
            "0 MHz",
            "125",
            "125 mhz",
            "-5 MHz",
            "1.0000001 MHz",
            ". MHz",
            "1e3 MHz",
        ] {
            assert_eq!(f(bad), None, "{bad}");
        }
        assert_eq!(Frequency::parse("125 MHz").unwrap().period_ns(), 8.0);
    }

    #[test]
    fn a_clock_names_a_pin_and_cyclone_ii_takes_one() {
        let dir = scratch("clocks", &["src/a.v"]);
        let m =
            Manifest::parse(&format!("{DE2}\n[clocks]\nCLOCK_50 = \"50 MHz\"\n"), &dir).unwrap();
        let Target::Ep2c35f672c6 { clock, .. } = &m.target else {
            unreachable!()
        };
        assert_eq!(
            clock
                .as_ref()
                .map(|c| (c.port.to_string(), c.frequency.hz())),
            Some(("CLOCK_50".into(), 50_000_000))
        );

        let two = format!("{DE2}\n[clocks]\nCLOCK_50 = \"50 MHz\"\n\"LEDR[0]\" = \"1 MHz\"\n");
        assert!(matches!(
            Manifest::parse(&two, &dir),
            Err(ManifestError::TooManyClocks(2, Device::Ep2c35f672c6))
        ));
        let m = Manifest::parse(
            &format!("{ZYBO}\n[clocks]\nclk = \"125 MHz\"\n\"led[1]\" = \"10 MHz\"\n"),
            &dir,
        )
        .unwrap();
        let Target::Xc7z010clg400_1 { clocks, .. } = &m.target else {
            unreachable!()
        };
        assert_eq!(clocks.len(), 2);

        for (text, port) in [
            (
                format!("{DE2}\n[clocks]\nCLOCK_27 = \"27 MHz\"\n"),
                "CLOCK_27",
            ),
            // The bus, not one of its bits, has no pin.
            (format!("{DE2}\n[clocks]\nLEDR = \"1 MHz\"\n"), "LEDR"),
        ] {
            match Manifest::parse(&text, &dir) {
                Err(ManifestError::ClockWithoutPin(p)) => assert_eq!(p, port),
                other => panic!("{other:?}"),
            }
        }
        assert!(matches!(
            Manifest::parse(&format!("{DE2}\n[clocks]\nCLOCK_50 = \"fast\"\n"), &dir),
            Err(ManifestError::BadFrequency { .. })
        ));
    }
}
