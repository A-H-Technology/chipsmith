use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use facet::Facet;

use crate::error::ChipsmithError;

/// A Manifest that has been through `parse`. Every field is what it claims to
/// be; nothing downstream needs to re-check the `[toolchain]` table.
#[derive(Debug)]
pub struct Manifest {
    pub project: Project,
    pub toolchain: ToolchainSpec,
    pub target: Target,
    pub hdl: Hdl,
    pub pins: BTreeMap<String, PinMapping>,
    /// `[clocks]` — port name to frequency, e.g. `clk = "50 MHz"`. Without these
    /// the design compiles unconstrained and timing analysis reports nothing useful.
    pub clocks: BTreeMap<String, String>,
    /// `[io-standards]` — per-signal overrides of `target.io_standard`.
    pub io_standards: BTreeMap<String, String>,
}

/// The `chipsmith.toml` as written on disk. Exists only to be turned into a
/// `Manifest`: it is the shape the file has, not the shape the program wants.
#[derive(Debug, Facet)]
struct ManifestFile {
    project: Project,
    toolchain: ToolchainTable,
    target: Target,
    hdl: Hdl,
    #[facet(default)]
    pins: BTreeMap<String, PinMapping>,
    #[facet(default)]
    clocks: BTreeMap<String, String>,
    #[facet(default, rename = "io-standards")]
    io_standards: BTreeMap<String, String>,
}

#[derive(Debug, Facet)]
pub struct Project {
    pub name: String,
    pub top: String,
}

/// `[toolchain]` as written — a Backend name mapped to a Version,
/// e.g. `quartus-prime = "23.1"`. Only ever one entry; see `ToolchainSpec`.
#[derive(Debug, Facet)]
#[facet(transparent)]
struct ToolchainTable(BTreeMap<String, String>);

/// The Backend and Version a Project builds with. Holding one is proof that
/// `[toolchain]` named exactly one Backend — there is no other way to get one
/// out of a file, and no way to build one that says otherwise.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolchainSpec {
    backend: String,
    version: String,
}

impl ToolchainSpec {
    pub fn new(backend: impl Into<String>, version: impl Into<String>) -> Self {
        Self {
            backend: backend.into(),
            version: version.into(),
        }
    }

    /// The Backend name, e.g. `quartus-prime`.
    pub fn backend(&self) -> &str {
        &self.backend
    }

    /// The Version, e.g. `23.1`.
    pub fn version(&self) -> &str {
        &self.version
    }
}

impl TryFrom<ToolchainTable> for ToolchainSpec {
    type Error = String;

    /// The one place the "exactly one Backend" rule is enforced.
    fn try_from(table: ToolchainTable) -> Result<Self, Self::Error> {
        let mut entries = table.0.into_iter();
        match (entries.next(), entries.next()) {
            (Some((backend, version)), None) => Ok(Self { backend, version }),
            _ => Err(
                "[toolchain] must have exactly one entry (e.g. quartus-prime = \"23.1\")"
                    .to_string(),
            ),
        }
    }
}

#[derive(Debug, Facet)]
pub struct Target {
    pub family: String,
    pub device: String,
    /// I/O standard applied to every assigned pin, e.g. `"3.3-V LVTTL"`.
    /// Individual signals can override it in `[io-standards]`.
    #[facet(default)]
    pub io_standard: Option<String>,
}

#[derive(Debug, Facet)]
pub struct Hdl {
    #[facet(default = "VHDL_2008".to_string())]
    pub standard: String,
    pub sources: Vec<String>,
}

#[derive(Debug, Facet)]
#[facet(untagged)]
#[repr(u8)]
pub enum PinMapping {
    Single(String),
    Bus(Vec<String>),
}

/// A clock constraint with its frequency already resolved to an SDC period.
#[derive(Debug, Clone, PartialEq)]
pub struct Clock {
    pub port: String,
    pub period_ns: f64,
}

/// Parse a frequency as written on a board silkscreen — `50 MHz`, `125MHz`, `32.768 kHz`.
fn parse_frequency_hz(text: &str) -> Result<f64, String> {
    let trimmed = text.trim();
    let split = trimmed
        .find(|c: char| !c.is_ascii_digit() && c != '.')
        .ok_or_else(|| format!("'{trimmed}' has no frequency unit (expected e.g. \"50 MHz\")"))?;
    let (number, unit) = trimmed.split_at(split);

    let value: f64 = number
        .parse()
        .map_err(|_| format!("'{trimmed}' does not start with a number"))?;
    if value <= 0.0 {
        return Err(format!("'{trimmed}' must be a positive frequency"));
    }

    let multiplier = match unit.trim().to_ascii_lowercase().as_str() {
        "hz" => 1.0,
        "khz" => 1_000.0,
        "mhz" => 1_000_000.0,
        "ghz" => 1_000_000_000.0,
        other => {
            return Err(format!(
                "unknown frequency unit '{other}' (use Hz, kHz, MHz or GHz)"
            ))
        }
    };

    Ok(value * multiplier)
}

impl Manifest {
    pub fn load(project_dir: &Path) -> Result<Self, ChipsmithError> {
        let path = project_dir.join("chipsmith.toml");
        let content = std::fs::read_to_string(&path)
            .map_err(|_| ChipsmithError::ManifestNotFound { path: path.clone() })?;
        Self::parse(&content, &path)
    }

    /// Turn the text of a `chipsmith.toml` into a Manifest, or say why it isn't
    /// one. Every rule about a well-formed Manifest is enforced here and only
    /// here — the filesystem sits outside so tests reach all of it.
    pub fn parse(content: &str, path: &Path) -> Result<Self, ChipsmithError> {
        let bad = |message: String| ChipsmithError::ManifestParse {
            path: path.to_path_buf(),
            message,
        };

        let file: ManifestFile = facet_toml::from_str(content).map_err(|e| bad(e.to_string()))?;

        let manifest = Manifest {
            project: file.project,
            toolchain: ToolchainSpec::try_from(file.toolchain).map_err(bad)?,
            target: file.target,
            hdl: file.hdl,
            pins: file.pins,
            clocks: file.clocks,
            io_standards: file.io_standards,
        };

        // Surface a bad [clocks] or [io-standards] entry now rather than mid-compile
        manifest.resolve_clocks().map_err(bad)?;
        if let Some(unknown) = manifest
            .io_standards
            .keys()
            .find(|signal| !manifest.pins.contains_key(*signal))
        {
            return Err(bad(format!(
                "[io-standards] '{unknown}' has no matching entry in [pins]"
            )));
        }

        Ok(manifest)
    }

    /// The I/O standard for a signal: its `[io-standards]` override, else the
    /// project-wide `target.io_standard`, else none.
    pub fn io_standard_for(&self, signal: &str) -> Option<&str> {
        self.io_standards
            .get(signal)
            .or(self.target.io_standard.as_ref())
            .map(String::as_str)
    }

    /// Resolve `[clocks]` into SDC periods, checking each port has a pin assignment.
    pub fn resolve_clocks(&self) -> Result<Vec<Clock>, String> {
        self.clocks
            .iter()
            .map(|(port, frequency)| {
                if !self.pins.contains_key(port) {
                    return Err(format!(
                        "[clocks] '{port}' has no matching entry in [pins]; \
                         known pins: {}",
                        self.pins.keys().cloned().collect::<Vec<_>>().join(", ")
                    ));
                }
                let hz =
                    parse_frequency_hz(frequency).map_err(|e| format!("[clocks] '{port}': {e}"))?;
                Ok(Clock {
                    port: port.clone(),
                    period_ns: 1_000_000_000.0 / hz,
                })
            })
            .collect()
    }

    pub fn resolve_sources(&self, project_dir: &Path) -> Result<Vec<PathBuf>, ChipsmithError> {
        let mut files = Vec::new();
        for pattern in &self.hdl.sources {
            let full_pattern = project_dir.join(pattern);
            let matches: Vec<_> = glob::glob(full_pattern.to_str().unwrap_or(pattern))
                .map_err(|e| ChipsmithError::ManifestParse {
                    path: project_dir.join("chipsmith.toml"),
                    message: format!("invalid glob pattern '{}': {}", pattern, e),
                })?
                .filter_map(|r| r.ok())
                .collect();

            if matches.is_empty() {
                return Err(ChipsmithError::NoSourceFiles {
                    pattern: pattern.clone(),
                });
            }
            files.extend(matches);
        }
        Ok(files)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const VALID_TOML: &str = r#"
[project]
name = "blinky"
top = "blinky"

[toolchain]
quartus-prime = "23.1"

[target]
family = "Cyclone V"
device = "5CSEBA6U23I7"

[hdl]
sources = ["src/*.vhd"]

[pins]
clk = "PIN_Y2"
led = ["PIN_V16", "PIN_W16", "PIN_V17", "PIN_W17"]
"#;

    /// Exactly what `load` does, minus the file read — so these tests exercise
    /// every validation rule a real `chipsmith.toml` goes through.
    fn parse_manifest(toml: &str) -> Result<Manifest, String> {
        Manifest::parse(toml, Path::new("chipsmith.toml")).map_err(|e| e.to_string())
    }

    #[test]
    fn parses_valid_manifest() {
        let m = parse_manifest(VALID_TOML).unwrap();
        assert_eq!(m.project.name, "blinky");
        assert_eq!(m.project.top, "blinky");
        assert_eq!(m.toolchain.backend(), "quartus-prime");
        assert_eq!(m.toolchain.version(), "23.1");
        assert_eq!(m.target.family, "Cyclone V");
        assert_eq!(m.target.device, "5CSEBA6U23I7");
        assert_eq!(m.hdl.sources, vec!["src/*.vhd"]);
    }

    fn with_clocks(clocks: &str) -> Result<Manifest, String> {
        parse_manifest(&format!("{VALID_TOML}\n[clocks]\n{clocks}\n"))
    }

    #[test]
    fn resolves_clock_frequency_to_period() {
        let m = with_clocks(r#"clk = "50 MHz""#).unwrap();
        let clocks = m.resolve_clocks().unwrap();
        assert_eq!(clocks.len(), 1);
        assert_eq!(clocks[0].port, "clk");
        assert!((clocks[0].period_ns - 20.0).abs() < 1e-9);
    }

    #[test]
    fn accepts_frequency_units_with_and_without_spaces() {
        for (text, expected_ns) in [
            ("50MHz", 20.0),
            ("50 mhz", 20.0),
            ("125 MHz", 8.0),
            ("1 GHz", 1.0),
            ("1000 kHz", 1000.0),
            ("1000000 Hz", 1000.0),
        ] {
            let m = with_clocks(&format!("clk = \"{text}\"")).unwrap();
            let clocks = m.resolve_clocks().unwrap();
            assert!(
                (clocks[0].period_ns - expected_ns).abs() < 1e-6,
                "{text} gave {} ns, want {expected_ns}",
                clocks[0].period_ns
            );
        }
    }

    #[test]
    fn rejects_clock_without_a_pin_assignment() {
        let err = with_clocks(r#"clock_50 = "50 MHz""#).unwrap_err();
        assert!(err.contains("clock_50"), "{err}");
        assert!(err.contains("[pins]"), "{err}");
    }

    #[test]
    fn rejects_unparseable_frequency() {
        for bad in ["fifty MHz", "50", "50 furlongs", "-50 MHz", "0 MHz"] {
            let result = with_clocks(&format!("clk = \"{bad}\""));
            assert!(result.is_err(), "'{bad}' should not parse");
        }
    }

    #[test]
    fn rejects_an_io_standard_for_a_signal_with_no_pin() {
        let err = parse_manifest(&format!(
            "{VALID_TOML}\n[io-standards]\nuart_tx = \"3.3-V LVTTL\"\n"
        ))
        .unwrap_err();
        assert!(err.contains("uart_tx"), "{err}");
        assert!(err.contains("[pins]"), "{err}");
    }

    #[test]
    fn io_standard_falls_back_from_signal_to_target_to_none() {
        let m = parse_manifest(VALID_TOML).unwrap();
        assert_eq!(m.io_standard_for("clk"), None);

        let with_default = parse_manifest(&VALID_TOML.replace(
            r#"device = "5CSEBA6U23I7""#,
            "device = \"5CSEBA6U23I7\"\nio_standard = \"3.3-V LVTTL\"",
        ))
        .unwrap();
        assert_eq!(with_default.io_standard_for("clk"), Some("3.3-V LVTTL"));
        assert_eq!(with_default.io_standard_for("led"), Some("3.3-V LVTTL"));

        let with_override = parse_manifest(&format!(
            "{}\n[io-standards]\nclk = \"1.5 V\"\n",
            VALID_TOML.replace(
                r#"device = "5CSEBA6U23I7""#,
                "device = \"5CSEBA6U23I7\"\nio_standard = \"3.3-V LVTTL\"",
            )
        ))
        .unwrap();
        assert_eq!(with_override.io_standard_for("clk"), Some("1.5 V"));
        assert_eq!(with_override.io_standard_for("led"), Some("3.3-V LVTTL"));
    }

    #[test]
    fn manifest_without_clocks_resolves_to_none() {
        let m = parse_manifest(VALID_TOML).unwrap();
        assert!(m.resolve_clocks().unwrap().is_empty());
    }

    #[test]
    fn default_hdl_standard_is_vhdl_2008() {
        let m = parse_manifest(VALID_TOML).unwrap();
        assert_eq!(m.hdl.standard, "VHDL_2008");
    }

    #[test]
    fn parses_single_pin() {
        let m = parse_manifest(VALID_TOML).unwrap();
        match m.pins.get("clk").unwrap() {
            PinMapping::Single(pin) => assert_eq!(pin, "PIN_Y2"),
            _ => panic!("expected Single pin mapping"),
        }
    }

    #[test]
    fn parses_bus_pins() {
        let m = parse_manifest(VALID_TOML).unwrap();
        match m.pins.get("led").unwrap() {
            PinMapping::Bus(pins) => {
                assert_eq!(pins.len(), 4);
                assert_eq!(pins[0], "PIN_V16");
                assert_eq!(pins[3], "PIN_W17");
            }
            _ => panic!("expected Bus pin mapping"),
        }
    }

    #[test]
    fn rejects_multiple_toolchain_entries() {
        let toml = r#"
[project]
name = "test"
top = "test"

[toolchain]
quartus-prime = "23.1"
vivado = "2024.1"

[target]
family = "Cyclone V"
device = "test"

[hdl]
sources = ["*.vhd"]
"#;
        let result = parse_manifest(toml);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("exactly one entry"));
    }

    #[test]
    fn rejects_missing_project_name() {
        let toml = r#"
[project]
top = "test"

[toolchain]
quartus-prime = "23.1"

[target]
family = "Cyclone V"
device = "test"

[hdl]
sources = ["*.vhd"]
"#;
        assert!(parse_manifest(toml).is_err());
    }
}
