//! `chipsmith.toml`. `Manifest::load` is the only place its rules are
//! enforced; a `Manifest` that exists is buildable as far as the file can say.

use std::collections::BTreeMap;
use std::fmt;
use std::num::NonZeroU64;
use std::path::{Path, PathBuf};

use chipsmith_cyclone2::pin::{
    IoRegisters, LvttlCurrent, PinSettings, Pull, Standard, UnusedPins, Vccio, QUARTUS_STANDARDS,
};
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
    /// The DE2's FPGA: its pins have I/O settings (`chipsmith_cyclone2::pin`).
    Ep2c35f672c6 {
        pins: Vec<PinAssignment<QuartusPin, De2Pin>>,
        /// Clocks entering at pins. PLL outputs take their frequency from
        /// the PLL; clocks from logic have none.
        clocks: Vec<Clock>,
        unused_pins: UnusedPins,
    },
    Xc7z010clg400_1 {
        pins: Vec<PinAssignment<Clg400Pin>>,
        clocks: Vec<Clock>,
    },
    /// The DE25-Nano's Agilex 5, booted HPS-first: the build is phase 2,
    /// the core, which the SDM takes only after `hps`'s phase 1.
    A5eb013bb23be4scs {
        pins: Vec<PinAssignment<A5Pin>>,
        hps: HpsFirst,
    },
}

impl Target {
    pub fn device(&self) -> Device {
        match self {
            Target::Ep2c35f672c6 { .. } => Device::Ep2c35f672c6,
            Target::Xc7z010clg400_1 { .. } => Device::Xc7z010clg400_1,
            Target::A5eb013bb23be4scs { .. } => Device::A5eb013bb23be4scs,
        }
    }
}

/// An Agilex 5 pin: a package ball at its bank's I/O voltage, or an input
/// of the HPS that the fabric drives.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum A5Pin {
    Ball { ball: String, vccio: A5Vccio },
    Hps(HpsInput),
}

/// An Agilex 5 ball's bank voltage, as the board powers the bank.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum A5Vccio {
    V3_3,
    V1_8,
    V1_1,
}

impl A5Vccio {
    fn parse(s: &str) -> Option<Self> {
        match s {
            "3.3 V" => Some(A5Vccio::V3_3),
            "1.8 V" => Some(A5Vccio::V1_8),
            "1.1 V" => Some(A5Vccio::V1_1),
            _ => None,
        }
    }

    /// In volts, as place and route's pins take it.
    pub fn volts(self) -> &'static str {
        match self {
            A5Vccio::V3_3 => "3.3",
            A5Vccio::V1_8 => "1.8",
            A5Vccio::V1_1 => "1.1",
        }
    }
}

/// The HPS inputs a design can drive: those whose entry into the HPS is
/// known (agilex5-re README, "HPS-first": f2h_irq0[k] reaches
/// SOC_HPS_WRAPPER_RE through these RPIIs, read back as GICD_ISPENDR1 bit
/// 17 + k).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HpsInput {
    F2hIrq0Bit0,
    F2hIrq0Bit3,
}

impl HpsInput {
    const ALL: [HpsInput; 2] = [HpsInput::F2hIrq0Bit0, HpsInput::F2hIrq0Bit3];

    pub fn name(self) -> &'static str {
        match self {
            HpsInput::F2hIrq0Bit0 => "f2h_irq0[0]",
            HpsInput::F2hIrq0Bit3 => "f2h_irq0[3]",
        }
    }

    /// The routing node the signal ends on.
    pub fn node(self) -> &'static str {
        match self {
            HpsInput::F2hIrq0Bit0 => "RPII:X52Y51Z0I44",
            HpsInput::F2hIrq0Bit3 => "RPII:X52Y51Z0I24",
        }
    }
}

/// Where an Agilex 5 pin entry points, before its settings are checked.
#[derive(Debug, Clone, PartialEq, Eq)]
enum A5Site {
    Ball(String),
    Hps(HpsInput),
}

impl fmt::Display for A5Site {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            A5Site::Ball(b) => f.write_str(b),
            A5Site::Hps(h) => f.write_str(h.name()),
        }
    }
}

impl A5Site {
    fn parse(s: &str) -> Result<Self, ManifestError> {
        if let Some(h) = HpsInput::ALL.into_iter().find(|h| h.name() == s) {
            return Ok(A5Site::Hps(h));
        }
        let letters = s.bytes().take_while(u8::is_ascii_uppercase).count();
        let digits = &s[letters..];
        if (1..=2).contains(&letters)
            && (1..=2).contains(&digits.len())
            && digits.bytes().all(|b| b.is_ascii_digit())
        {
            return Ok(A5Site::Ball(s.to_string()));
        }
        Err(ManifestError::BadPin {
            pin: s.to_string(),
            device: Device::A5eb013bb23be4scs,
            expected:
                "pins are package balls like V16, or the HPS inputs f2h_irq0[0] and f2h_irq0[3]",
        })
    }
}

/// An HPS-first boot's phase 1, as the core must match it. Both come from
/// the board's one Quartus Prime Pro HPS-first build: `shell` is its HPS
/// and I/O configuration as design-file lines (agilex5-re
/// `tools/hps_shell.sh`), the hash is its HPS IO hash (`quartus_pfg -i`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HpsFirst {
    pub shell: PathBuf,
    pub hps_io_hash: [u8; 32],
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
/// can't be elaborated together yet. `Verilog` holds `.v` and `.sv` files
/// alike; the front end reads each in its own dialect.
#[derive(Debug, PartialEq, Eq)]
pub enum Sources {
    Verilog(Vec<PathBuf>),
    Vhdl(Vec<PathBuf>),
}

/// A port bit's pin and, where the device has them, its I/O settings.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PinAssignment<P, S = ()> {
    pub port: PortBit,
    pub pin: P,
    pub io: S,
}

/// A DE2 pin's I/O settings: electrical ones for the I/O cell, and the
/// I/O-cell registers synthesis should fill.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct De2Pin {
    pub settings: PinSettings,
    pub registers: IoRegisters,
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
    #[error("{0}: not a Verilog (.v), SystemVerilog (.sv) or VHDL (.vhd, .vhdl) file")]
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
    #[error("[pins] {port}: {reason}")]
    BadPinEntry { port: String, reason: String },
    #[error("[target] unused_pins = {0:?}: one of \"tri-stated\", \"pull-up\", \"bus-hold\" (driving unused pins to ground would short the DE2's other chips)")]
    BadUnusedPins(String),
    #[error("{0} has no I/O settings in the manifest yet; [pins] takes pin names only")]
    NoIoSettings(Device),
    #[error("[target] {0}")]
    BadTarget(String),
    #[error("[clocks]: {0} builds don't take clock constraints yet; place and route times the design against its delay model alone")]
    NoClocks(Device),
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
    /// Port to pin(s): a string, a list (index i is bit i), or a table
    /// with `pin` or `pins` and I/O settings (`PinTable`).
    #[serde(default)]
    pins: BTreeMap<String, toml::Value>,
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
    unused_pins: Option<String>,
    hps_shell: Option<String>,
    hps_io_hash: Option<String>,
}

/// A `[pins]` entry written as a table: the pin or pins, and settings that
/// apply to every bit.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PinTable {
    pin: Option<String>,
    pins: Option<Vec<String>>,
    standard: Option<String>,
    drive: Option<String>,
    pull: Option<String>,
    registers: Option<Vec<String>>,
    /// An Agilex 5 ball's bank voltage.
    vccio: Option<String>,
}

impl PinTable {
    fn has_settings(&self) -> bool {
        self.standard.is_some()
            || self.drive.is_some()
            || self.pull.is_some()
            || self.registers.is_some()
            || self.vccio.is_some()
    }
}

/// A `[pins]` entry, its form checked.
struct PinsEntry {
    /// One pin (a scalar port) or a list (index i is bit i of the port).
    pins: Result<String, Vec<String>>,
    table: Option<PinTable>,
}

impl PinsEntry {
    fn parse(port: &str, value: toml::Value) -> Result<Self, ManifestError> {
        let bad = |reason: String| ManifestError::BadPinEntry {
            port: port.to_string(),
            reason,
        };
        let list = |a: Vec<toml::Value>| -> Result<Vec<String>, ManifestError> {
            a.into_iter()
                .map(|v| match v {
                    toml::Value::String(s) => Ok(s),
                    other => Err(bad(format!("{other} is not a pin name"))),
                })
                .collect()
        };
        match value {
            toml::Value::String(s) => Ok(PinsEntry {
                pins: Ok(s),
                table: None,
            }),
            toml::Value::Array(a) => Ok(PinsEntry {
                pins: Err(list(a)?),
                table: None,
            }),
            v @ toml::Value::Table(_) => {
                let mut t: PinTable = v
                    .try_into()
                    .map_err(|e: toml::de::Error| bad(e.message().to_string()))?;
                let pins = match (t.pin.take(), t.pins.take()) {
                    (Some(p), None) => Ok(p),
                    (None, Some(ps)) => Err(ps),
                    _ => {
                        return Err(bad(
                            "give one of `pin` (a scalar port) or `pins` (a bus)".into()
                        ))
                    }
                };
                Ok(PinsEntry {
                    pins,
                    table: Some(t),
                })
            }
            other => Err(bad(format!(
                "{other} is not a pin, a list of pins or a table"
            ))),
        }
    }
}

/// A DE2 entry's settings. Every rule about which settings go together is
/// checked here, so a `De2Pin` is one the board can take.
fn de2_pin(port: &str, t: &PinTable) -> Result<De2Pin, ManifestError> {
    let bad = |reason: String| ManifestError::BadPinEntry {
        port: port.to_string(),
        reason,
    };
    if t.vccio.is_some() {
        return Err(bad(
            "vccio is an Agilex 5 setting; a DE2 pin takes `standard`".into(),
        ));
    }
    let current = t
        .drive
        .as_deref()
        .map(|d| {
            d.strip_suffix("mA")
                .map(str::trim_end)
                .and_then(|n| n.parse::<u32>().ok())
                .ok_or_else(|| bad(format!("drive = {d:?} is not a current like \"8 mA\"")))
        })
        .transpose()?;
    let lvttl_currents = || {
        LvttlCurrent::ALL
            .iter()
            .map(|c| c.ma().to_string())
            .collect::<Vec<_>>()
            .join(", ")
    };
    let standard = match t.standard.as_deref().unwrap_or("3.3-V LVTTL") {
        "3.3-V LVTTL" => Standard::Lvttl(match current {
            None => LvttlCurrent::Ma24,
            Some(ma) => LvttlCurrent::from_ma(ma).ok_or_else(|| {
                bad(format!("3.3-V LVTTL drives {} mA, not {ma} mA", lvttl_currents()))
            })?,
        }),
        "3.3-V LVCMOS" if current.is_none() => Standard::Lvcmos,
        "3.3-V LVCMOS" => {
            return Err(bad(
                "3.3-V LVCMOS takes its default current only (no other drive code is decoded); \
                 3.3-V LVTTL has every current"
                    .into(),
            ))
        }
        "2.5 V" if current.is_none() => Standard::V25Input,
        "2.5 V" => return Err(bad("a 2.5-V pin is an input on the DE2, it has no drive".into())),
        s => {
            return Err(bad(match QUARTUS_STANDARDS.iter().find(|(n, _)| *n == s) {
                Some((_, vccio)) => format!(
                    "{s} needs a bank at VCCIO {vccio}; the DE2 powers every bank at {}",
                    Vccio::V33
                ),
                None => format!(
                    "unknown I/O standard {s:?}; the DE2 takes \"3.3-V LVTTL\", \"3.3-V LVCMOS\" or \"2.5 V\" (inputs)"
                ),
            }))
        }
    };
    let pull = match t.pull.as_deref() {
        None => Pull::None,
        Some("up") => Pull::Up,
        Some("bus-hold") => Pull::BusHold,
        Some(p) => return Err(bad(format!("pull = {p:?}: \"up\" or \"bus-hold\""))),
    };
    let mut registers = IoRegisters::default();
    for r in t.registers.iter().flatten() {
        let flag = match r.as_str() {
            "input" => &mut registers.input,
            "output" => &mut registers.output,
            "output-enable" => &mut registers.output_enable,
            _ => {
                return Err(bad(format!(
                    "registers: {r:?} is not \"input\", \"output\" or \"output-enable\""
                )))
            }
        };
        if std::mem::replace(flag, true) {
            return Err(bad(format!("registers lists {r:?} twice")));
        }
    }
    Ok(De2Pin {
        settings: PinSettings { standard, pull },
        registers,
    })
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
        if device != Device::A5eb013bb23be4scs
            && (file.target.hps_shell.is_some() || file.target.hps_io_hash.is_some())
        {
            return Err(ManifestError::BadTarget(format!(
                "hps_shell and hps_io_hash are for an HPS-first Agilex 5, not {device}"
            )));
        }
        let mut entries = Vec::new();
        for (port, value) in file.pins {
            let entry = PinsEntry::parse(&port, value)?;
            entries.push((port, entry));
        }
        let target = match device {
            Device::Ep2c35f672c6 => {
                let pins = flatten_pins(entries, QuartusPin::parse, |port, t| match t {
                    Some(t) => de2_pin(port, t),
                    None => Ok(De2Pin::default()),
                })?;
                let clocks = parse_clocks(&file.clocks, &pins)?;
                let unused_pins = match file.target.unused_pins.as_deref() {
                    None | Some("tri-stated") => UnusedPins::TriStated,
                    Some("pull-up") => UnusedPins::PullUp,
                    Some("bus-hold") => UnusedPins::BusHold,
                    Some(other) => return Err(ManifestError::BadUnusedPins(other.to_string())),
                };
                Target::Ep2c35f672c6 {
                    pins,
                    clocks,
                    unused_pins,
                }
            }
            Device::Xc7z010clg400_1 => {
                if file.target.unused_pins.is_some() {
                    return Err(ManifestError::NoIoSettings(device));
                }
                let pins = flatten_pins(
                    entries,
                    |s| {
                        Clg400Pin::parse(s).ok_or_else(|| ManifestError::BadPin {
                            pin: s.to_string(),
                            device,
                            expected: "pins are the package's programmable-logic balls, like L16 (PS/MIO pins are not)",
                        })
                    },
                    |_, t| match t {
                        Some(t) if t.has_settings() => Err(ManifestError::NoIoSettings(device)),
                        _ => Ok(()),
                    },
                )?;
                let clocks = parse_clocks(&file.clocks, &pins)?;
                Target::Xc7z010clg400_1 { pins, clocks }
            }
            Device::A5eb013bb23be4scs => {
                if file.target.unused_pins.is_some() {
                    return Err(ManifestError::NoIoSettings(device));
                }
                if !file.clocks.is_empty() {
                    return Err(ManifestError::NoClocks(device));
                }
                let sites = flatten_pins(entries, A5Site::parse, |port, t| {
                    let bad = |reason: String| ManifestError::BadPinEntry {
                        port: port.to_string(),
                        reason,
                    };
                    match t {
                        Some(t)
                            if t.standard.is_some()
                                || t.drive.is_some()
                                || t.pull.is_some()
                                || t.registers.is_some() =>
                        {
                            Err(bad("an Agilex 5 pin takes only `vccio` so far".into()))
                        }
                        Some(PinTable { vccio: Some(v), .. }) => {
                            A5Vccio::parse(v).map(Some).ok_or_else(|| {
                                bad(format!(
                                    "vccio = {v:?}: one of \"3.3 V\", \"1.8 V\", \"1.1 V\""
                                ))
                            })
                        }
                        _ => Ok(None),
                    }
                })?;
                let pins = sites
                    .into_iter()
                    .map(|a| {
                        let bad = |reason: String| ManifestError::BadPinEntry {
                            port: a.port.to_string(),
                            reason,
                        };
                        let pin = match (a.pin, a.io) {
                            (A5Site::Ball(ball), Some(vccio)) => A5Pin::Ball { ball, vccio },
                            (A5Site::Ball(ball), None) => {
                                return Err(bad(format!(
                                    "{ball}: give its bank's voltage, {{ pin = \"{ball}\", vccio = \"3.3 V\" }}"
                                )))
                            }
                            (A5Site::Hps(h), None) => A5Pin::Hps(h),
                            (A5Site::Hps(h), Some(_)) => {
                                return Err(bad(format!("{} is inside the chip and has no vccio", h.name())))
                            }
                        };
                        Ok(PinAssignment { port: a.port, pin, io: () })
                    })
                    .collect::<Result<_, _>>()?;
                let shell = file.target.hps_shell.as_deref().ok_or_else(|| {
                    ManifestError::BadTarget(
                        "hps_shell: the board's HPS shell lines, from its Quartus HPS-first build"
                            .into(),
                    )
                })?;
                let shell = project_dir.join(shell);
                if !shell.is_file() {
                    return Err(ManifestError::BadTarget(format!(
                        "hps_shell: no file {}",
                        shell.display()
                    )));
                }
                let hash = file.target.hps_io_hash.as_deref().ok_or_else(|| {
                    ManifestError::BadTarget(
                        "hps_io_hash: phase 1's HPS IO hash (quartus_pfg -i), 64 hex digits".into(),
                    )
                })?;
                Target::A5eb013bb23be4scs {
                    pins,
                    hps: HpsFirst {
                        shell,
                        hps_io_hash: parse_hash(hash).ok_or_else(|| {
                            ManifestError::BadTarget(format!(
                                "hps_io_hash = {hash:?} is not 64 hex digits"
                            ))
                        })?,
                    },
                }
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

/// 64 hex digits, either case.
fn parse_hash(s: &str) -> Option<[u8; 32]> {
    if s.len() != 64 || !s.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    let mut out = [0u8; 32];
    for (i, b) in out.iter_mut().enumerate() {
        *b = u8::from_str_radix(&s[2 * i..2 * i + 2], 16).ok()?;
    }
    Some(out)
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
            Some("v" | "sv") => verilog.push(file),
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

fn parse_clocks<P, S>(
    table: &BTreeMap<String, String>,
    pins: &[PinAssignment<P, S>],
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

/// One assignment per port bit; `io` turns an entry's table (if it has
/// one) into the device's settings, which every bit of the entry gets.
fn flatten_pins<P: PartialEq + fmt::Display, S: Copy>(
    entries: Vec<(String, PinsEntry)>,
    parse: impl Fn(&str) -> Result<P, ManifestError>,
    io: impl Fn(&str, Option<&PinTable>) -> Result<S, ManifestError>,
) -> Result<Vec<PinAssignment<P, S>>, ManifestError> {
    let mut assignments: Vec<PinAssignment<P, S>> = Vec::new();
    for (port, entry) in entries {
        let settings = io(&port, entry.table.as_ref())?;
        let bits: Vec<(PortBit, &String)> = match &entry.pins {
            Ok(pin) => vec![(PortBit::Scalar(port.clone()), pin)],
            Err(pins) => pins
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
            assignments.push(PinAssignment {
                port,
                pin,
                io: settings,
            });
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

    /// A Spade design is swim's `build/spade.sv`, alongside any Verilog
    /// of its own.
    #[test]
    fn systemverilog_is_verilog_read_in_its_own_dialect() {
        let dir = scratch("sv", &["src/spade.sv", "src/extern.v"]);
        let m = Manifest::parse(DE2, &dir).unwrap();
        let Sources::Verilog(files) = &m.sources else {
            panic!("{:?} is not Verilog", m.sources);
        };
        assert_eq!(files.len(), 2);
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
    fn a_clock_names_a_pin() {
        let dir = scratch("clocks", &["src/a.v"]);
        let m =
            Manifest::parse(&format!("{DE2}\n[clocks]\nCLOCK_50 = \"50 MHz\"\n"), &dir).unwrap();
        let Target::Ep2c35f672c6 { clocks, .. } = &m.target else {
            unreachable!()
        };
        assert_eq!(
            clocks
                .iter()
                .map(|c| (c.port.to_string(), c.frequency.hz()))
                .collect::<Vec<_>>(),
            [("CLOCK_50".to_string(), 50_000_000)]
        );

        let two = format!("{DE2}\n[clocks]\nCLOCK_50 = \"50 MHz\"\n\"LEDR[0]\" = \"1 MHz\"\n");
        let Target::Ep2c35f672c6 { clocks, .. } = Manifest::parse(&two, &dir).unwrap().target
        else {
            unreachable!()
        };
        assert_eq!(clocks.len(), 2);
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

    #[test]
    fn a_table_entry_gives_every_bit_its_settings() {
        let dir = scratch("settings", &["src/a.v"]);
        let text = format!(
            "{DE2}\n{}",
            r#"GPIO = { pins = ["PIN_D25", "PIN_J22"], pull = "up", drive = "8 mA", registers = ["output", "input"] }
               KEY = { pin = "PIN_G26", standard = "2.5 V", pull = "bus-hold" }
               HEX = { pin = "PIN_AF10", standard = "3.3-V LVCMOS" }"#
        );
        let text = text.replace("[target]\n", "[target]\nunused_pins = \"pull-up\"\n");
        let m = Manifest::parse(&text, &dir).unwrap();
        let Target::Ep2c35f672c6 {
            pins, unused_pins, ..
        } = &m.target
        else {
            unreachable!()
        };
        assert_eq!(*unused_pins, UnusedPins::PullUp);
        let io = |port: &str| pins.iter().find(|a| a.port.to_string() == port).unwrap().io;
        let gpio = De2Pin {
            settings: PinSettings {
                standard: Standard::Lvttl(LvttlCurrent::Ma8),
                pull: Pull::Up,
            },
            registers: IoRegisters {
                input: true,
                output: true,
                output_enable: false,
            },
        };
        assert_eq!(io("GPIO[0]"), gpio);
        assert_eq!(io("GPIO[1]"), gpio);
        assert_eq!(io("KEY").settings.standard, Standard::V25Input);
        assert_eq!(io("KEY").settings.pull, Pull::BusHold);
        assert_eq!(io("HEX").settings.standard, Standard::Lvcmos);
        // A plain entry is 3.3-V LVTTL at 24 mA with nothing else.
        assert_eq!(io("CLOCK_50"), De2Pin::default());
    }

    #[test]
    fn settings_the_de2_cant_take_are_rejected_when_the_manifest_loads() {
        let dir = scratch("bad-settings", &["src/a.v"]);
        for (entry, says) in [
            (r#"{ pin = "PIN_D25", standard = "1.8 V" }"#, "VCCIO 1.8 V"),
            (r#"{ pin = "PIN_D25", standard = "1.5 V" }"#, "VCCIO 1.5 V"),
            (
                r#"{ pin = "PIN_D25", standard = "SSTL-2 Class I" }"#,
                "unknown I/O standard",
            ),
            (
                r#"{ pin = "PIN_D25", standard = "3.3-V LVCMOS", drive = "8 mA" }"#,
                "default current",
            ),
            (
                r#"{ pin = "PIN_D25", standard = "2.5 V", drive = "8 mA" }"#,
                "input",
            ),
            (r#"{ pin = "PIN_D25", drive = "2 mA" }"#, "not 2 mA"),
            (r#"{ pin = "PIN_D25", drive = "fast" }"#, "current like"),
            (r#"{ pin = "PIN_D25", pull = "down" }"#, "bus-hold"),
            (
                r#"{ pin = "PIN_D25", registers = ["input", "input"] }"#,
                "twice",
            ),
            (
                r#"{ pin = "PIN_D25", registers = ["clock"] }"#,
                "output-enable",
            ),
            (r#"{ pin = "PIN_D25", pins = ["PIN_J22"] }"#, "one of"),
            (r#"{ pull = "up" }"#, "one of"),
            (r#"{ pin = "PIN_D25", slew = "fast" }"#, "slew"),
        ] {
            let text = format!("{DE2}\nGPIO = {entry}\n");
            match Manifest::parse(&text, &dir) {
                Err(e @ ManifestError::BadPinEntry { .. }) => {
                    assert!(e.to_string().contains(says), "{entry}: {e}")
                }
                other => panic!("{entry}: {other:?}"),
            }
        }
        let ground = DE2.replace("[target]\n", "[target]\nunused_pins = \"ground\"\n");
        assert!(matches!(
            Manifest::parse(&ground, &dir),
            Err(ManifestError::BadUnusedPins(_))
        ));
    }

    #[test]
    fn the_zybo_takes_no_io_settings() {
        let dir = scratch("zybo-settings", &["src/a.v"]);
        let plain = format!("{ZYBO}\nbtn = {{ pin = \"R18\" }}\n");
        assert!(Manifest::parse(&plain, &dir).is_ok());
        for text in [
            format!("{ZYBO}\nbtn = {{ pin = \"R18\", pull = \"up\" }}\n"),
            ZYBO.replace("[target]\n", "[target]\nunused_pins = \"pull-up\"\n"),
        ] {
            assert!(
                matches!(
                    Manifest::parse(&text, &dir),
                    Err(ManifestError::NoIoSettings(_))
                ),
                "{text}"
            );
        }
    }

    const DE25: &str = r#"
        [project]
        name = "irq"
        top = "irq"
        sources = ["src/*"]

        [target]
        device = "A5EB013BB23BE4SCS"
        hps_shell = "hps-shell.txt"
        hps_io_hash = "D921CDF7CADFEECA11572B24600BFD337FFD119E47601CA5075AA911EEC3D07A"

        [pins]
        CLOCK1_50 = { pin = "V16", vccio = "3.3 V" }
        SW = { pins = ["DK24", "DD24"], vccio = "1.1 V" }
        irq0 = "f2h_irq0[0]"
    "#;

    #[test]
    fn the_de25_takes_balls_with_their_voltage_and_known_hps_inputs() {
        let dir = scratch("de25", &["src/irq.v", "hps-shell.txt"]);
        let m = Manifest::parse(DE25, &dir).unwrap();
        let Target::A5eb013bb23be4scs { pins, hps } = &m.target else {
            panic!("{:?} is not the DE25-Nano's part", m.target);
        };
        let pins: Vec<_> = pins
            .iter()
            .map(|a| match &a.pin {
                A5Pin::Ball { ball, vccio } => format!("{} {ball} {}", a.port, vccio.volts()),
                A5Pin::Hps(h) => format!("{} {}", a.port, h.node()),
            })
            .collect();
        assert_eq!(
            pins,
            [
                "CLOCK1_50 V16 3.3",
                "SW[0] DK24 1.1",
                "SW[1] DD24 1.1",
                "irq0 RPII:X52Y51Z0I44"
            ]
        );
        assert_eq!(hps.shell, dir.join("hps-shell.txt"));
        assert_eq!(hps.hps_io_hash[..2], [0xd9, 0x21]);
        assert_eq!(hps.hps_io_hash[31], 0x7a);
    }

    #[test]
    fn a_de25_manifest_without_what_its_core_needs_is_rejected() {
        let dir = scratch("de25-bad", &["src/irq.v", "hps-shell.txt"]);
        let hash = "D921CDF7CADFEECA11572B24600BFD337FFD119E47601CA5075AA911EEC3D07A";
        let pin_entry = [
            // A ball without its bank's voltage, a voltage on an HPS input,
            // a voltage the board has no bank at, a DE2 setting.
            DE25.replace(r#"{ pin = "V16", vccio = "3.3 V" }"#, r#""V16""#),
            DE25.replace(
                r#"irq0 = "f2h_irq0[0]""#,
                r#"irq0 = { pin = "f2h_irq0[0]", vccio = "3.3 V" }"#,
            ),
            DE25.replace(r#"vccio = "3.3 V""#, r#"vccio = "2.5 V""#),
            DE25.replace(r#"vccio = "3.3 V""#, r#"vccio = "3.3 V", pull = "up""#),
        ];
        for text in pin_entry {
            assert!(
                matches!(
                    Manifest::parse(&text, &dir),
                    Err(ManifestError::BadPinEntry { .. })
                ),
                "{text}"
            );
        }
        // An HPS input whose node isn't known, a pin that isn't a ball.
        for text in [
            DE25.replace("f2h_irq0[0]", "f2h_irq0[1]"),
            DE25.replace(r#""V16""#, r#""PIN_N2""#),
        ] {
            assert!(
                matches!(
                    Manifest::parse(&text, &dir),
                    Err(ManifestError::BadPin { .. })
                ),
                "{text}"
            );
        }
        let target = [
            DE25.replace("hps_shell = \"hps-shell.txt\"\n", ""),
            DE25.replace("hps-shell.txt\"", "missing.txt\""),
            DE25.replace(&format!("hps_io_hash = \"{hash}\"\n"), ""),
            DE25.replace(hash, &hash[..62]),
            DE25.replace(hash, &format!("+{}", &hash[1..])),
        ];
        for text in target {
            assert!(
                matches!(
                    Manifest::parse(&text, &dir),
                    Err(ManifestError::BadTarget(_))
                ),
                "{text}"
            );
        }
        assert!(matches!(
            Manifest::parse(&format!("{DE25}\n[clocks]\nCLOCK1_50 = \"50 MHz\"\n"), &dir),
            Err(ManifestError::NoClocks(Device::A5eb013bb23be4scs))
        ));
        // The DE25's settings on the other boards.
        let de2_dir = scratch("de2-a5", &["src/blinky.v"]);
        for text in [
            DE2.replace(
                r#"device = "EP2C35F672C6""#,
                "device = \"EP2C35F672C6\"\nhps_shell = \"x\"",
            ),
            DE2.replace(
                r#"CLOCK_50 = "PIN_N2""#,
                r#"CLOCK_50 = { pin = "PIN_N2", vccio = "3.3 V" }"#,
            ),
        ] {
            assert!(Manifest::parse(&text, &de2_dir).is_err(), "{text}");
        }
    }
}
