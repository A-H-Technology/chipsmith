//! HDL to bitstream to chip. The device decides the flow.
//!
//! Cyclone II: some stages are still external tools: GHDL (VHDL to
//! Verilog), yosys (synthesis, except LUT mapping) and cram (bitstream,
//! JTAG stream). LUT mapping (`synth::lutmap`), packing (`pack`),
//! placement (`place`), routing (`route`) and loading over the cable
//! (`jtag`) are chipsmith's own.
//!
//! 7-series (Zynq-7010): yosys `synth_xilinx`, openXC7's nextpnr
//! (himbaechel xilinx) for packing, placement and site routing, prjxray's
//! `fasm2frames` and `xc7frames2bit`. The interconnect routing (`route::xc7`)
//! and loading over the Zybo's FT2232 cable (`jtag::ftdi`, `jtag::xc7`) are
//! chipsmith's own.
//!
//! Each stage is one `Step` in a `Plan`, so replacing a tool with
//! chipsmith's own code means replacing one step.
//!
//! Planning is pure: `plan_cyclone2`, `plan_xc7` and `plan_flash` return
//! every file to write and every command to run, and `Plan::run` does it.
//! `plan_build` only adds reading the tools' locations (and cyclone2-re's
//! synthesis script) from the environment.

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use std::time::Instant;

use anyhow::{bail, Context, Result};

use crate::jtag::ftdi::Ftdi;
use crate::jtag::usb_blaster::UsbBlaster;
use crate::jtag::{svf, xc7};

use crate::device::{Clg400Pin, Device, Xc7Part, XC7Z010CLG400_1};
use crate::manifest::{Clock, Frequency, Manifest, PinAssignment, QuartusPin, Sources, Target};

/// Where the cyclone2-re yosys techmap lives: a directory holding `synth/`.
/// Set by the nix wrapper and the devshell.
pub const SYNTH_DIR_VAR: &str = "CHIPSMITH_CYCLONE2_SYNTH";
/// A directory of nextpnr-himbaechel xilinx chip databases,
/// `chipdb-<die>.bin`. Set by the nix wrapper and the devshell.
pub const XC7_CHIPDB_VAR: &str = "CHIPSMITH_XC7_CHIPDB";
/// A prjxray-db checkout (holding `zynq7/`, ...), the one the chip
/// databases were built from. Set by the nix wrapper and the devshell.
pub const PRJXRAY_DB_VAR: &str = "CHIPSMITH_PRJXRAY_DB";

/// Every artifact path of a build, in one place.
#[derive(Debug, Clone)]
pub struct Layout {
    pub dir: PathBuf,
    name: String,
    device: Device,
}

impl Layout {
    pub fn new(project_dir: &Path, manifest: &Manifest) -> Self {
        Layout {
            dir: project_dir.join("build"),
            name: manifest.name.clone(),
            device: manifest.target.device(),
        }
    }

    /// What the device is configured with: a raw bitstream for Cyclone II,
    /// a `.bit` (header plus configuration packets) for 7-series.
    pub fn bitstream(&self) -> PathBuf {
        let extension = match self.device {
            Device::Ep2c35f672c6 => "rbf",
            Device::Xc7z010clg400_1 => "bit",
        };
        self.dir.join(format!("{}.{extension}", self.name))
    }
    fn translated(&self) -> PathBuf {
        self.dir.join("vhdl.v")
    }
    fn synth_script(&self) -> PathBuf {
        self.dir.join("synth.ys")
    }
    /// Cyclone II: synthesis after LUT mapping.
    fn cells_script(&self) -> PathBuf {
        self.dir.join("cells.ys")
    }
    /// Cyclone II: the netlist as simple gates and registers, before LUT
    /// mapping.
    fn gates(&self) -> PathBuf {
        self.dir.join("gates.json")
    }
    fn luts(&self) -> PathBuf {
        self.dir.join("luts.json")
    }
    fn netlist(&self) -> PathBuf {
        self.dir.join("netlist.json")
    }
    fn packed(&self) -> PathBuf {
        self.dir.join("packed.json")
    }
    fn placed(&self) -> PathBuf {
        self.dir.join("placed.json")
    }
    fn routed(&self) -> PathBuf {
        self.dir.join("routed.json")
    }
    fn nextpnr_routed(&self) -> PathBuf {
        self.dir.join("nextpnr.json")
    }
    fn preplaced(&self) -> PathBuf {
        self.dir.join("preplaced.txt")
    }
    fn prerouted(&self) -> PathBuf {
        self.dir.join("prerouted.txt")
    }
    fn xdc(&self) -> PathBuf {
        self.dir.join("pins.xdc")
    }
    fn fasm(&self) -> PathBuf {
        self.dir.join(format!("{}.fasm", self.name))
    }
    fn timing_report(&self) -> PathBuf {
        self.dir.join("timing.json")
    }
    fn frames(&self) -> PathBuf {
        self.dir.join(format!("{}.frames", self.name))
    }
    fn log(&self, tool: &str) -> PathBuf {
        self.dir.join(format!("{tool}.log"))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Step {
    /// An external program.
    Tool(Tool),
    /// Map a yosys netlist's simple gates to 4-input LUTs, in process
    /// (`synth::lutmap::map_file`).
    Lutmap { gates: PathBuf, luts: PathBuf },
    /// Pack a yosys netlist into LEs, pins and clock controls (nextpnr's
    /// JSON), in process.
    Pack { netlist: PathBuf, packed: PathBuf },
    /// Place a packed design (nextpnr JSON) and route it, in process.
    /// `pins` maps top-level port bits (`LEDR[0]`) to pins (`PIN_AE23`).
    /// The routed design's estimated slack is reported against `clock`.
    PlaceRoute {
        packed: PathBuf,
        placed: PathBuf,
        routed: PathBuf,
        pins: BTreeMap<String, String>,
        clock: Option<Frequency>,
    },
    /// Report each clock's achieved frequency from nextpnr's `--report`;
    /// fails if one misses its constraint.
    NextpnrTiming(PathBuf),
    /// Route a 7-series design nextpnr placed and routed (`nextpnr`) again
    /// with chipsmith's router, in process: its routes as nextpnr's
    /// `-o prerouted` lines, and nextpnr's placement as `-o preplaced` lines.
    Xc7Route {
        nextpnr: PathBuf,
        prerouted: PathBuf,
        preplaced: PathBuf,
    },
    /// Play an SVF over the USB-Blaster, in process.
    PlaySvf(PathBuf),
    /// Load a 7-series `.bit` over a Digilent FT2232 cable, in process.
    LoadBit(PathBuf),
}

/// The 7-series tools' data, resolved for one part.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Xc7Data {
    /// nextpnr's chip database for the part's die.
    pub chipdb: PathBuf,
    /// prjxray-db's directory for the part's family, e.g. `.../zynq7`.
    pub db: PathBuf,
}

impl Xc7Data {
    pub fn from_env(part: Xc7Part) -> Result<Self> {
        let var = |name: &str| {
            std::env::var_os(name).map(PathBuf::from).with_context(|| {
                format!("{name} is not set; run chipsmith from its nix package or `nix develop`")
            })
        };
        let data = Xc7Data {
            chipdb: var(XC7_CHIPDB_VAR)?.join(format!("chipdb-{}.bin", part.die)),
            db: var(PRJXRAY_DB_VAR)?.join(part.family),
        };
        if !data.chipdb.is_file() {
            bail!(
                "no chip database for {} at {}",
                part.die,
                data.chipdb.display()
            );
        }
        let part_yaml = data.db.join(part.part).join("part.yaml");
        if !part_yaml.is_file() {
            bail!("prjxray-db has no {}", part_yaml.display());
        }
        Ok(data)
    }
}

impl Step {
    pub fn label(&self) -> &'static str {
        match self {
            Step::Tool(tool) => tool.label,
            Step::Lutmap { .. } => "LUT mapping",
            Step::Pack { .. } => "pack",
            Step::PlaceRoute { .. } => "place and route",
            Step::NextpnrTiming(_) => "nextpnr: timing",
            Step::Xc7Route { .. } => "route (chipsmith)",
            Step::PlaySvf(_) => "JTAG: program over USB-Blaster",
            Step::LoadBit(_) => "JTAG: program over Digilent FT2232",
        }
    }
}

impl From<Tool> for Step {
    fn from(tool: Tool) -> Self {
        Step::Tool(tool)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Tool {
    pub label: &'static str,
    pub program: OsString,
    pub args: Vec<OsString>,
    pub cwd: Option<PathBuf>,
    /// Where stdout goes when the step's output *is* its product.
    pub stdout: Option<PathBuf>,
    /// The tool's own log, pointed at when the step fails.
    pub log: Option<PathBuf>,
}

impl Tool {
    fn new(label: &'static str, program: &str) -> Self {
        Tool {
            label,
            program: program.into(),
            args: Vec::new(),
            cwd: None,
            stdout: None,
            log: None,
        }
    }

    fn arg(mut self, arg: impl Into<OsString>) -> Self {
        self.args.push(arg.into());
        self
    }

    fn args<I: IntoIterator<Item = S>, S: Into<OsString>>(mut self, args: I) -> Self {
        self.args.extend(args.into_iter().map(Into::into));
        self
    }
}

#[derive(Debug, Default)]
pub struct Plan {
    pub dir: Option<PathBuf>,
    pub files: Vec<(PathBuf, String)>,
    pub steps: Vec<Step>,
}

/// The build for the manifest's device, with the tools' data found through
/// the environment the nix package (or devshell) sets.
pub fn plan_build(manifest: &Manifest, layout: &Layout) -> Result<Plan> {
    Ok(match &manifest.target {
        Target::Ep2c35f672c6 { pins, clock } => plan_cyclone2(
            manifest,
            pins,
            clock.as_ref(),
            layout,
            &Cyclone2Synth::from_env()?,
        ),
        Target::Xc7z010clg400_1 { pins, clocks } => {
            let part = XC7Z010CLG400_1;
            plan_xc7(
                manifest,
                part,
                pins,
                clocks,
                layout,
                &Xc7Data::from_env(part)?,
            )
        }
    })
}

/// cyclone2-re's `synth/cyclone2.ys`, cut at its LUT mapping so
/// chipsmith's mapper runs in place of `abc -lut 4`. Cut at plan time
/// rather than copied into chipsmith, so cyclone2-re stays the one place
/// the techmap is written; a script whose cut point moved or doubled fails
/// here instead of synthesising something else.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cyclone2Synth {
    /// The directory holding `synth/`; the script names its maps relative
    /// to it.
    dir: PathBuf,
    /// Up to the LUT mapping: simple gates and registers.
    to_gates: String,
    /// After it: LUTs and registers to Cyclone II cells, I/O buffers, checks.
    to_cells: String,
}

impl Cyclone2Synth {
    /// The line chipsmith's LUT mapper replaces.
    const LUT_MAPPING: &str = "abc -lut 4";
    const SCRIPT: &str = "synth/cyclone2.ys";

    pub fn parse(dir: PathBuf, script: &str) -> Result<Self> {
        let lines: Vec<&str> = script.lines().collect();
        let cuts: Vec<usize> = (0..lines.len())
            .filter(|&i| lines[i].trim() == Self::LUT_MAPPING)
            .collect();
        let [cut] = cuts[..] else {
            bail!(
                "{} has {} `{}` lines, expected exactly one to replace with chipsmith's LUT mapper",
                dir.join(Self::SCRIPT).display(),
                cuts.len(),
                Self::LUT_MAPPING
            );
        };
        let join = |lines: &[&str]| lines.iter().map(|l| format!("{l}\n")).collect();
        Ok(Cyclone2Synth {
            to_gates: join(&lines[..cut]),
            to_cells: join(&lines[cut + 1..]),
            dir,
        })
    }

    pub fn from_env() -> Result<Self> {
        let dir = std::env::var_os(SYNTH_DIR_VAR)
            .map(PathBuf::from)
            .with_context(|| {
                format!(
                    "{SYNTH_DIR_VAR} is not set; run chipsmith from its nix package or `nix develop`"
                )
            })?;
        let path = dir.join(Self::SCRIPT);
        let script = std::fs::read_to_string(&path)
            .with_context(|| format!("{SYNTH_DIR_VAR}: reading {}", path.display()))?;
        Self::parse(dir, &script)
    }
}

/// VHDL becomes Verilog through GHDL; Verilog is used as it is. Returns the
/// Verilog files synthesis reads.
fn verilog_sources(manifest: &Manifest, layout: &Layout, steps: &mut Vec<Step>) -> Vec<PathBuf> {
    match &manifest.sources {
        Sources::Verilog(files) => files.clone(),
        Sources::Vhdl(files) => {
            steps.push(
                Tool {
                    cwd: Some(layout.dir.clone()),
                    stdout: Some(layout.translated()),
                    ..Tool::new("ghdl: VHDL to Verilog", "ghdl")
                }
                .args(["--synth", "--std=08", "--out=verilog"])
                .args(files)
                .args(["-e", &manifest.top])
                .into(),
            );
            vec![layout.translated()]
        }
    }
}

pub fn plan_cyclone2(
    manifest: &Manifest,
    pins: &[PinAssignment<QuartusPin>],
    clock: Option<&Clock>,
    layout: &Layout,
    synth: &Cyclone2Synth,
) -> Plan {
    let mut steps: Vec<Step> = Vec::new();
    let verilog = verilog_sources(manifest, layout, &mut steps);

    let yosys = |label, script: PathBuf, log: &str| -> Step {
        Tool {
            cwd: Some(synth.dir.clone()),
            log: Some(layout.log(log)),
            ..Tool::new(label, "yosys")
        }
        .arg("-q")
        .arg("-l")
        .arg(layout.log(log))
        .arg("-s")
        .arg(script)
        .into()
    };
    steps.push(yosys(
        "yosys: synthesis to gates",
        layout.synth_script(),
        "yosys",
    ));
    steps.push(Step::Lutmap {
        gates: layout.gates(),
        luts: layout.luts(),
    });
    steps.push(yosys(
        "yosys: LUTs to Cyclone II cells",
        layout.cells_script(),
        "yosys-cells",
    ));
    steps.push(Step::Pack {
        netlist: layout.netlist(),
        packed: layout.packed(),
    });
    steps.push(Step::PlaceRoute {
        packed: layout.packed(),
        placed: layout.placed(),
        routed: layout.routed(),
        pins: pins
            .iter()
            .map(|a| (a.port.to_string(), a.pin.as_str().to_string()))
            .collect(),
        clock: clock.map(|c| c.frequency),
    });
    steps.push(
        Tool::new("cram: bitstream", "cram")
            // `-`: the base image comes from the device database.
            .args(["pack", "-"])
            .arg(layout.routed())
            .arg(layout.bitstream())
            .into(),
    );

    Plan {
        dir: Some(layout.dir.clone()),
        files: vec![
            (
                layout.synth_script(),
                gates_script(&verilog, &manifest.top, synth, &layout.gates()),
            ),
            (
                layout.cells_script(),
                cells_script(synth, &layout.luts(), &layout.netlist()),
            ),
        ],
        steps,
    }
}

/// Every pin is LVCMOS33: the Zybo's PL banks (34, 35) are both powered at
/// 3.3 V, and the manifest has no I/O standards yet. Clocks reach nextpnr as
/// XDC `create_clock`s; without one it times against its default 12 MHz.
pub fn plan_xc7(
    manifest: &Manifest,
    part: Xc7Part,
    pins: &[PinAssignment<Clg400Pin>],
    clocks: &[Clock],
    layout: &Layout,
    data: &Xc7Data,
) -> Plan {
    let mut steps: Vec<Step> = Vec::new();
    let verilog = verilog_sources(manifest, layout, &mut steps);

    steps.push(
        Tool {
            log: Some(layout.log("yosys")),
            ..Tool::new("yosys: synthesis (synth_xilinx)", "yosys")
        }
        .arg("-q")
        .arg("-l")
        .arg(layout.log("yosys"))
        .arg("-s")
        .arg(layout.synth_script())
        .into(),
    );
    steps.push(
        Tool {
            log: Some(layout.log("nextpnr")),
            ..Tool::new("nextpnr: place and route", "nextpnr-himbaechel")
        }
        .args(["--device", part.part])
        .arg("--chipdb")
        .arg(&data.chipdb)
        .arg("--json")
        .arg(layout.netlist())
        .arg("-o")
        .arg(prefixed("xdc=", &layout.xdc()))
        .arg("--write")
        .arg(layout.nextpnr_routed())
        .arg("-q")
        .arg("-l")
        .arg(layout.log("nextpnr"))
        .into(),
    );
    // nextpnr's interconnect routing is replaced by chipsmith's; nextpnr
    // keeps the site routing, and its FASM writer writes both. nextpnr
    // can't load the placed design it wrote (it names bels one way when
    // writing and another when reading), so it packs the netlist again
    // and pins each cell to its bel (`-o preplaced`); packing is
    // deterministic, so the cells are the same.
    steps.push(Step::Xc7Route {
        nextpnr: layout.nextpnr_routed(),
        prerouted: layout.prerouted(),
        preplaced: layout.preplaced(),
    });
    steps.push(
        Tool {
            log: Some(layout.log("nextpnr-fasm")),
            ..Tool::new("nextpnr: FASM of chipsmith's routing", "nextpnr-himbaechel")
        }
        .args(["--device", part.part])
        .arg("--chipdb")
        .arg(&data.chipdb)
        .arg("--json")
        .arg(layout.netlist())
        .arg("-o")
        .arg(prefixed("xdc=", &layout.xdc()))
        .arg("-o")
        .arg(prefixed("preplaced=", &layout.preplaced()))
        .arg("-o")
        .arg(prefixed("prerouted=", &layout.prerouted()))
        .arg("-o")
        .arg(prefixed("fasm=", &layout.fasm()))
        .arg("--write")
        .arg(layout.routed())
        // Timing of chipsmith's routing, not of the routing it replaced.
        .arg("--report")
        .arg(layout.timing_report())
        // `Step::NextpnrTiming` fails the build instead, after printing
        // every clock's Fmax.
        .arg("--timing-allow-fail")
        .arg("-q")
        .arg("-l")
        .arg(layout.log("nextpnr-fasm"))
        .into(),
    );
    steps.push(Step::NextpnrTiming(layout.timing_report()));
    steps.push(
        Tool::new("prjxray: FASM to frames", "fasm2frames")
            .args(["--part", part.part])
            .arg("--db-root")
            .arg(&data.db)
            .arg(layout.fasm())
            .arg(layout.frames())
            .into(),
    );
    steps.push(
        // The .bit header names the frames file as given: relative to the
        // build directory, it doesn't depend on where the project lives.
        Tool {
            cwd: Some(layout.dir.clone()),
            ..Tool::new("prjxray: frames to .bit", "xc7frames2bit")
        }
        .arg("--part_file")
        .arg(data.db.join(part.part).join("part.yaml"))
        .args(["--part_name", part.part])
        .arg("--frm_file")
        .arg(file_name(&layout.frames()))
        .arg("--output_file")
        .arg(layout.bitstream())
        .into(),
    );

    Plan {
        dir: Some(layout.dir.clone()),
        files: vec![
            (
                layout.synth_script(),
                xc7_synth_script(&verilog, &manifest.top, &layout.netlist()),
            ),
            (layout.xdc(), xdc(pins, clocks)),
        ],
        steps,
    }
}

fn file_name(path: &Path) -> &std::ffi::OsStr {
    path.file_name().expect("layout paths name files")
}

fn prefixed(prefix: &str, path: &Path) -> OsString {
    let mut s = OsString::from(prefix);
    s.push(path);
    s
}

fn xc7_synth_script(verilog: &[PathBuf], top: &str, netlist: &Path) -> String {
    let mut script = String::new();
    for file in verilog {
        script += &format!("read_verilog {}\n", quoted(file));
    }
    // openXC7's flow (demo-projects openXC7.mk): one flat netlist, abc9 for
    // LUT mapping with the 7-series delays.
    script += &format!("synth_xilinx -flatten -abc9 -arch xc7 -top {top}\n");
    script += &format!("write_json {}\n", quoted(netlist));
    script
}

fn xdc(pins: &[PinAssignment<Clg400Pin>], clocks: &[Clock]) -> String {
    let pins = pins.iter().map(|a| {
        format!(
            "set_property PACKAGE_PIN {pin} [get_ports {{{port}}}]\n\
             set_property IOSTANDARD LVCMOS33 [get_ports {{{port}}}]\n",
            pin = a.pin,
            port = a.port
        )
    });
    // nextpnr's XDC reader ignores `-name`; the clock is named after its net.
    let clocks = clocks.iter().map(|c| {
        format!(
            "create_clock -period {:.3} [get_ports {{{}}}]\n",
            c.frequency.period_ns(),
            c.port
        )
    });
    pins.chain(clocks).collect()
}

/// What `chipsmith flash` can load.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Loadable {
    /// A Cyclone II raw bitstream; cram turns it into its JTAG sequence.
    Rbf(PathBuf),
    /// A JTAG sequence already, from cram or any other tool.
    Svf(PathBuf),
    /// A 7-series `.bit`, loaded over a Digilent FT2232 cable (the Zybo's
    /// on-board one).
    Bit(PathBuf),
}

impl Loadable {
    pub fn from_path(path: PathBuf) -> Result<Self> {
        match path.extension().and_then(|e| e.to_str()) {
            Some("rbf") => Ok(Loadable::Rbf(path)),
            Some("svf") => Ok(Loadable::Svf(path)),
            Some("bit") => Ok(Loadable::Bit(path)),
            _ => bail!("{}: can load .rbf, .svf or .bit files", path.display()),
        }
    }
}

pub fn plan_flash(loadable: &Loadable) -> Plan {
    let steps = match loadable {
        Loadable::Svf(svf) => vec![Step::PlaySvf(svf.clone())],
        Loadable::Rbf(rbf) => {
            let svf = rbf.with_extension("svf");
            vec![
                // A raw .rbf shifted over JTAG configures a Cyclone II that
                // never starts up; cram writes the JTAG sequence and checks
                // CONF_DONE.
                Tool::new("cram: JTAG stream", "cram")
                    .arg("svf")
                    .arg(rbf)
                    .arg(&svf)
                    .into(),
                Step::PlaySvf(svf),
            ]
        }
        Loadable::Bit(bit) => vec![Step::LoadBit(bit.clone())],
    };
    Plan {
        dir: None,
        files: Vec::new(),
        steps,
    }
}

fn gates_script(verilog: &[PathBuf], top: &str, synth: &Cyclone2Synth, gates: &Path) -> String {
    let mut script = String::new();
    for file in verilog {
        script += &format!("read_verilog {}\n", quoted(file));
    }
    // cyclone2.ys synthesises the module called `top`.
    if top != "top" {
        script += &format!("rename {top} top\n");
    }
    script += &synth.to_gates;
    script += &format!("write_json {}\n", quoted(gates));
    script
}

fn cells_script(synth: &Cyclone2Synth, luts: &Path, netlist: &Path) -> String {
    // The cell library first, with its parameters: the mapped netlist holds
    // `top` only (see `Step::Lutmap`).
    let mut script = String::from("read_verilog -lib synth/cells_sim.v\n");
    script += &format!("read_json {}\n", quoted(luts));
    script += "hierarchy -top top\n";
    script += &synth.to_cells;
    script += &format!("write_json {}\n", quoted(netlist));
    script
}

fn quoted(path: &Path) -> String {
    format!("\"{}\"", path.display())
}

impl Plan {
    pub fn run(&self) -> Result<()> {
        if let Some(dir) = &self.dir {
            std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
        }
        for (path, contents) in &self.files {
            std::fs::write(path, contents)
                .with_context(|| format!("writing {}", path.display()))?;
        }
        for step in &self.steps {
            eprintln!("  {}", step.label());
            match step {
                Step::Tool(tool) => run_tool(tool)?,
                Step::Lutmap { gates, luts } => lutmap(gates, luts)?,
                Step::Pack { netlist, packed } => pack(netlist, packed)?,
                Step::PlaceRoute {
                    packed,
                    placed,
                    routed,
                    pins,
                    clock,
                } => place_route(packed, placed, routed, pins, *clock)?,
                Step::NextpnrTiming(report) => nextpnr_timing(report)?,
                Step::Xc7Route {
                    nextpnr,
                    prerouted,
                    preplaced,
                } => xc7_route(nextpnr, prerouted, preplaced)?,
                Step::PlaySvf(svf) => play_svf(svf)?,
                Step::LoadBit(bit) => load_bit(bit)?,
            }
        }
        Ok(())
    }
}

fn lutmap(gates: &Path, luts: &Path) -> Result<()> {
    let started = Instant::now();
    let s = crate::synth::lutmap::map_file(gates, luts)?;
    eprintln!(
        "    {} gates ({} AIG ANDs) to {} LUTs, depth {}, in {:.1} s",
        s.gates,
        s.and_nodes,
        s.luts,
        s.depth,
        started.elapsed().as_secs_f64()
    );
    Ok(())
}

fn pack(netlist: &Path, packed: &Path) -> Result<()> {
    let started = Instant::now();
    let s = crate::pack::pack_file(netlist, packed)?;
    eprintln!(
        "    {} LUTs, {} registers ({} on the sync load path), {} carry chains, {} clocks in {:.1} s; \
         removed {} stuck registers, {} constant and {} one-input LUTs",
        s.luts,
        s.ffs,
        s.sdata,
        s.chains,
        s.clocks,
        started.elapsed().as_secs_f64(),
        s.stuck_ffs,
        s.const_luts,
        s.removed_luts
    );
    Ok(())
}

fn place_route(
    packed: &Path,
    placed: &Path,
    routed: &Path,
    pins: &BTreeMap<String, String>,
    clock: Option<Frequency>,
) -> Result<()> {
    let started = Instant::now();
    let graph = crate::route::chipdb::ChipDb::load_for_device()?;
    eprintln!(
        "    routing graph in {:.1} s",
        started.elapsed().as_secs_f64()
    );

    let started = Instant::now();
    let p = crate::place::place_file(&graph, packed, pins, placed)?;
    eprintln!(
        "    placed {} LEs ({} carry chains), wirelength {} -> {} in {:.1} s; critical path {:.2} ns (estimate)",
        p.les,
        p.chains,
        p.initial_cost,
        p.cost,
        started.elapsed().as_secs_f64(),
        p.critical / 1000.0
    );

    let started = Instant::now();
    let r = crate::route::route_file(&graph, placed, routed)?;
    eprintln!(
        "    routed {} nets, {} pips, {} iterations in {:.1} s; critical path {:.2} ns (estimate)",
        r.nets,
        r.pips,
        r.iterations,
        started.elapsed().as_secs_f64(),
        r.critical / 1000.0
    );
    if let Some(f) = clock {
        let slack = f.period_ns() - f64::from(r.critical) / 1000.0;
        eprintln!(
            "    {f}: slack {slack:.2} ns (estimate, Fmax {:.1} MHz){}",
            1e6 / f64::from(r.critical),
            if slack < 0.0 { "; FAILS timing" } else { "" }
        );
    }
    Ok(())
}

/// One clock's line of nextpnr's `--report` JSON (`"fmax"`).
#[derive(Debug, PartialEq, serde::Deserialize)]
struct NextpnrFmax {
    /// MHz.
    achieved: f64,
    /// MHz.
    constraint: f64,
}

impl NextpnrFmax {
    fn slack_ns(&self) -> f64 {
        1e3 / self.constraint - 1e3 / self.achieved
    }
}

fn nextpnr_fmax(report: &str) -> Result<BTreeMap<String, NextpnrFmax>> {
    #[derive(serde::Deserialize)]
    struct Report {
        fmax: BTreeMap<String, NextpnrFmax>,
    }
    Ok(serde_json::from_str::<Report>(report)?.fmax)
}

fn nextpnr_timing(report: &Path) -> Result<()> {
    let text =
        std::fs::read_to_string(report).with_context(|| format!("reading {}", report.display()))?;
    let fmax = nextpnr_fmax(&text).with_context(|| format!("parsing {}", report.display()))?;
    for (clock, f) in &fmax {
        eprintln!(
            "    clock {clock}: Fmax {:.1} MHz at {} MHz, slack {:.3} ns",
            f.achieved,
            f.constraint,
            f.slack_ns()
        );
    }
    let failing: Vec<&str> = fmax
        .iter()
        .filter(|(_, f)| f.slack_ns() < 0.0)
        .map(|(clock, _)| clock.as_str())
        .collect();
    if !failing.is_empty() {
        bail!("timing fails for clock {}", failing.join(", "));
    }
    Ok(())
}

fn xc7_route(nextpnr: &Path, prerouted: &Path, preplaced: &Path) -> Result<()> {
    let started = Instant::now();
    let graph = crate::route::xray::load(XC7Z010CLG400_1)?;
    eprintln!(
        "    routing graph in {:.1} s",
        started.elapsed().as_secs_f64()
    );
    let r = crate::route::xc7::route_file(&graph, nextpnr, prerouted, preplaced)?;
    eprintln!(
        "    routed {} segments of {} nets, {} pips (nextpnr: {}), {} iterations in {:.1} s",
        r.segments, r.nets, r.pips, r.nextpnr_pips, r.iterations, r.route_seconds
    );
    Ok(())
}

fn play_svf(path: &Path) -> Result<()> {
    let text =
        std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    let commands = svf::parse(&text)?;
    let mut cable = UsbBlaster::open()?;
    let started = Instant::now();
    svf::Player::new(&mut cable).play(&commands)?;
    eprintln!("    played in {:.1} s", started.elapsed().as_secs_f64());
    Ok(())
}

fn load_bit(path: &Path) -> Result<()> {
    let file = std::fs::read(path).with_context(|| format!("reading {}", path.display()))?;
    let payload = xc7::bit_payload(&file)?;
    let mut cable = Ftdi::open()?;
    let chain = xc7::scan(&mut cable)?;
    // On a Zynq the PL shares the chain with the ARM DAP (IR 4 bits).
    let target = chain
        .iter()
        .position(|t| t.ir_len == 6)
        .context("no 7-series TAP on the JTAG chain")?;
    let started = Instant::now();
    xc7::load(&mut cable, &chain, target, payload)?;
    eprintln!(
        "    loaded {:#010x}, DONE high, in {:.1} s",
        chain[target].idcode,
        started.elapsed().as_secs_f64()
    );
    Ok(())
}

fn run_tool(step: &Tool) -> Result<()> {
    let mut cmd = Command::new(&step.program);
    cmd.args(&step.args);
    if let Some(cwd) = &step.cwd {
        cmd.current_dir(cwd);
    }
    if let Some(out) = &step.stdout {
        let file =
            std::fs::File::create(out).with_context(|| format!("creating {}", out.display()))?;
        cmd.stdout(Stdio::from(file));
    }
    let program = step.program.to_string_lossy();
    let status = cmd
        .status()
        .with_context(|| format!("running {program} (is it on PATH? `nix develop` provides it)"))?;
    if !status.success() {
        match &step.log {
            Some(log) => bail!("{} failed ({status}); see {}", step.label, log.display()),
            None => bail!("{} failed ({status})", step.label),
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const DE2: &str = r#"
        [project]
        name = "blinky"
        top = "blinky"
        sources = ["x.v"]
        [target]
        device = "EP2C35F672C6"
        [pins]
        CLOCK_50 = "PIN_N2"
        LEDR = ["PIN_AE23", "PIN_AF23"]
        [clocks]
        CLOCK_50 = "50 MHz"
    "#;

    const ZYBO: &str = r#"
        [project]
        name = "blinky"
        top = "blinky"
        sources = ["x.v"]
        [target]
        device = "xc7z010clg400-1"
        [pins]
        clk = "L16"
        led = ["M14", "M15"]
        [clocks]
        clk = "125 MHz"
    "#;

    fn manifest_for(text: &str, sources: Sources) -> Manifest {
        let dir = std::env::temp_dir().join(format!("chipsmith-flow-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("x.v"), "").unwrap();
        Manifest {
            sources,
            ..Manifest::parse(text, &dir).unwrap()
        }
    }

    fn layout(m: &Manifest) -> Layout {
        Layout {
            dir: "/p/build".into(),
            name: m.name.clone(),
            device: m.target.device(),
        }
    }

    /// cyclone2.ys' shape: maps before and after the LUT mapping.
    const CYCLONE2_YS: &str =
        "read_verilog -lib synth/cells_sim.v\nsimplemap\nabc -lut 4\nopt -fast\n";

    /// The DE2 manifest's plan, with `sources` in place of the file's.
    fn cyclone2(sources: Sources) -> Plan {
        let m = manifest_for(DE2, sources);
        let Target::Ep2c35f672c6 { pins, clock } = &m.target else {
            unreachable!()
        };
        let synth = Cyclone2Synth::parse("/synth".into(), CYCLONE2_YS).unwrap();
        plan_cyclone2(&m, pins, clock.as_ref(), &layout(&m), &synth)
    }

    fn xc7(sources: Sources) -> Plan {
        let m = manifest_for(ZYBO, sources);
        let Target::Xc7z010clg400_1 { pins, clocks } = &m.target else {
            unreachable!()
        };
        let data = Xc7Data {
            chipdb: "/db/chipdb-xc7z010.bin".into(),
            db: "/xray/zynq7".into(),
        };
        plan_xc7(&m, XC7Z010CLG400_1, pins, clocks, &layout(&m), &data)
    }

    fn tool(step: &Step) -> &Tool {
        match step {
            Step::Tool(tool) => tool,
            other => panic!("{other:?} is not a tool"),
        }
    }

    fn labels(plan: &Plan) -> Vec<&str> {
        plan.steps.iter().map(Step::label).collect()
    }

    #[test]
    fn verilog_goes_straight_to_yosys_and_chipsmith_maps_the_luts() {
        let plan = cyclone2(Sources::Verilog(vec!["/p/src/blinky.v".into()]));
        assert_eq!(
            labels(&plan),
            [
                "yosys: synthesis to gates",
                "LUT mapping",
                "yosys: LUTs to Cyclone II cells",
                "pack",
                "place and route",
                "cram: bitstream"
            ]
        );
        assert_eq!(
            tool(&plan.steps[0]).cwd.as_deref(),
            Some(Path::new("/synth"))
        );
        assert_eq!(
            tool(&plan.steps[2]).cwd.as_deref(),
            Some(Path::new("/synth"))
        );
        let (path, script) = &plan.files[0];
        assert_eq!(path, Path::new("/p/build/synth.ys"));
        assert_eq!(
            script,
            "read_verilog \"/p/src/blinky.v\"\nrename blinky top\n\
             read_verilog -lib synth/cells_sim.v\nsimplemap\n\
             write_json \"/p/build/gates.json\"\n"
        );
        assert_eq!(
            plan.steps[1],
            Step::Lutmap {
                gates: "/p/build/gates.json".into(),
                luts: "/p/build/luts.json".into()
            }
        );
        // The second yosys reads the library with its parameters, then the
        // mapped `top`, and carries on after `abc -lut 4`.
        let (path, script) = &plan.files[1];
        assert_eq!(path, Path::new("/p/build/cells.ys"));
        assert_eq!(
            script,
            "read_verilog -lib synth/cells_sim.v\nread_json \"/p/build/luts.json\"\n\
             hierarchy -top top\nopt -fast\nwrite_json \"/p/build/netlist.json\"\n"
        );
        // No nextpnr: chipsmith packs what yosys wrote.
        assert_eq!(
            plan.steps[3],
            Step::Pack {
                netlist: "/p/build/netlist.json".into(),
                packed: "/p/build/packed.json".into()
            }
        );
        assert_eq!(
            tool(&plan.steps[5]).args.last().unwrap(),
            "/p/build/blinky.rbf"
        );
    }

    #[test]
    fn cyclone2_ys_is_cut_at_exactly_one_lut_mapping() {
        for script in [
            "simplemap\n",
            "abc -lut 4\nopt\nabc -lut 4\n",
            "abc -lut 6\n",
        ] {
            let e = Cyclone2Synth::parse("/synth".into(), script).unwrap_err();
            assert!(e.to_string().contains("expected exactly one"), "{e}");
        }
        let s = Cyclone2Synth::parse("/synth".into(), "a\n  abc -lut 4  \nb").unwrap();
        assert_eq!((s.to_gates.as_str(), s.to_cells.as_str()), ("a\n", "b\n"));
    }

    #[test]
    fn vhdl_is_translated_to_verilog_and_yosys_reads_the_translation() {
        let plan = cyclone2(Sources::Vhdl(vec!["/p/src/blinky.vhd".into()]));
        assert_eq!(plan.steps[0].label(), "ghdl: VHDL to Verilog");
        assert_eq!(
            tool(&plan.steps[0]).stdout.as_deref(),
            Some(Path::new("/p/build/vhdl.v"))
        );
        assert!(plan.files[0]
            .1
            .starts_with("read_verilog \"/p/build/vhdl.v\"\n"));
    }

    #[test]
    fn pins_and_the_clock_reach_the_placer() {
        let plan = cyclone2(Sources::Verilog(vec![]));
        let Step::PlaceRoute { pins, clock, .. } = &plan.steps[4] else {
            panic!("{:?} is not place and route", plan.steps[4]);
        };
        assert_eq!(clock.map(Frequency::hz), Some(50_000_000));
        let pins: Vec<(&str, &str)> = pins.iter().map(|(a, b)| (a.as_str(), b.as_str())).collect();
        assert_eq!(
            pins,
            [
                ("CLOCK_50", "PIN_N2"),
                ("LEDR[0]", "PIN_AE23"),
                ("LEDR[1]", "PIN_AF23")
            ]
        );
    }

    #[test]
    fn the_zynq_goes_through_openxc7_and_prjxray_to_a_bit() {
        let plan = xc7(Sources::Verilog(vec!["/p/src/blinky.v".into()]));
        assert_eq!(
            labels(&plan),
            [
                "yosys: synthesis (synth_xilinx)",
                "nextpnr: place and route",
                "route (chipsmith)",
                "nextpnr: FASM of chipsmith's routing",
                "nextpnr: timing",
                "prjxray: FASM to frames",
                "prjxray: frames to .bit"
            ]
        );
        assert_eq!(
            plan.files[0].1,
            "read_verilog \"/p/src/blinky.v\"\nsynth_xilinx -flatten -abc9 -arch xc7 -top blinky\nwrite_json \"/p/build/netlist.json\"\n"
        );
        let nextpnr = tool(&plan.steps[1]);
        assert_eq!(nextpnr.program, "nextpnr-himbaechel");
        assert!(nextpnr
            .args
            .windows(2)
            .any(|w| w == ["--device", "xc7z010clg400-1"]));
        assert!(nextpnr
            .args
            .windows(2)
            .any(|w| w == ["--chipdb", "/db/chipdb-xc7z010.bin"]));
        assert_eq!(
            plan.steps[2],
            Step::Xc7Route {
                nextpnr: "/p/build/nextpnr.json".into(),
                prerouted: "/p/build/prerouted.txt".into(),
                preplaced: "/p/build/preplaced.txt".into(),
            }
        );
        let fasm = tool(&plan.steps[3]);
        assert!(fasm
            .args
            .windows(2)
            .any(|w| w == ["--report", "/p/build/timing.json"]));
        assert_eq!(
            plan.steps[4],
            Step::NextpnrTiming("/p/build/timing.json".into())
        );
        assert!(fasm
            .args
            .windows(2)
            .any(|w| w == ["-o", "preplaced=/p/build/preplaced.txt"]));
        assert!(fasm
            .args
            .windows(2)
            .any(|w| w == ["-o", "prerouted=/p/build/prerouted.txt"]));
        assert_eq!(
            tool(&plan.steps[5]).args.last().unwrap(),
            "/p/build/blinky.frames"
        );
        let bit = tool(&plan.steps[6]);
        assert!(bit
            .args
            .windows(2)
            .any(|w| w == ["--part_file", "/xray/zynq7/xc7z010clg400-1/part.yaml"]));
        assert!(bit
            .args
            .windows(2)
            .any(|w| w == ["--frm_file", "blinky.frames"]));
        assert_eq!(bit.cwd.as_deref(), Some(Path::new("/p/build")));
        assert_eq!(bit.args.last().unwrap(), "/p/build/blinky.bit");
    }

    #[test]
    fn zynq_pins_and_clocks_become_an_xdc() {
        let plan = xc7(Sources::Verilog(vec![]));
        let (path, xdc) = &plan.files[1];
        assert_eq!(path, Path::new("/p/build/pins.xdc"));
        assert_eq!(
            xdc,
            "set_property PACKAGE_PIN L16 [get_ports {clk}]\n\
             set_property IOSTANDARD LVCMOS33 [get_ports {clk}]\n\
             set_property PACKAGE_PIN M14 [get_ports {led[0]}]\n\
             set_property IOSTANDARD LVCMOS33 [get_ports {led[0]}]\n\
             set_property PACKAGE_PIN M15 [get_ports {led[1]}]\n\
             set_property IOSTANDARD LVCMOS33 [get_ports {led[1]}]\n\
             create_clock -period 8.000 [get_ports {clk}]\n"
        );
    }

    #[test]
    fn nextpnr_reports_fmax_per_clock_net() {
        // As nextpnr-himbaechel (xilinx) writes it, trimmed.
        let report = r#"{"utilization": {}, "fmax": {"$abc$21083$aiger$o100": {"achieved": 391.5426940917969, "constraint": 125}}, "critical_paths": []}"#;
        let fmax = nextpnr_fmax(report).unwrap();
        assert_eq!(
            fmax["$abc$21083$aiger$o100"],
            NextpnrFmax {
                achieved: 391.5426940917969,
                constraint: 125.0
            }
        );
        assert!((fmax["$abc$21083$aiger$o100"].slack_ns() - 5.446).abs() < 1e-3);
    }

    #[test]
    fn a_bit_is_loaded_over_the_ft2232() {
        let plan = plan_flash(&Loadable::from_path("/p/build/blinky.bit".into()).unwrap());
        assert_eq!(plan.steps, [Step::LoadBit("/p/build/blinky.bit".into())]);
    }
}
