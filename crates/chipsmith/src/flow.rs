//! HDL to bitstream to chip. The device decides the flow.
//!
//! Cyclone II: no external program between the Verilog and the chip
//! (ADR-0001). GHDL translates VHDL to Verilog in front of the flow; the
//! Verilog front end (`chipsmith_verilog`), synthesis
//! (`chipsmith_cyclone2::synth`), packing, placement, routing, the
//! bitstream and its JTAG stream (`chipsmith_cyclone2`) and loading over the
//! cable (`chipsmith_jtag`) are chipsmith's own.
//!
//! 7-series (Zynq-7010): yosys `synth_xilinx`, openXC7's nextpnr
//! (himbaechel xilinx) for packing, site routing and FASM, prjxray's
//! `fasm2frames` and `xc7frames2bit`. Placement (`chipsmith_xc7::place`),
//! the interconnect routing (`chipsmith_xc7::route`) and loading over the
//! Zybo's FT2232 cable (`chipsmith_jtag::ftdi`, `chipsmith_jtag::xc7`) are
//! chipsmith's own.
//!
//! Each stage is one `Step` in a `Plan`, so replacing a tool with
//! chipsmith's own code means replacing one step.
//!
//! Planning is pure: `plan_cyclone2`, `plan_xc7` and `plan_flash` return
//! every file to write and every command to run, and `Plan::run` does it.
//! `plan_build` only adds reading the 7-series tools' data from the
//! environment.

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use std::time::Instant;

use anyhow::{bail, Context, Result};

use chipsmith_cyclone2::bitstream;
use chipsmith_cyclone2::persist::{self, EpcsOp, Loader};
use chipsmith_cyclone2::synth::cells;
use chipsmith_jtag::ftdi::Ftdi;
use chipsmith_jtag::usb_blaster::UsbBlaster;
use chipsmith_jtag::{svf, xc7};

use crate::device::{Clg400Pin, Device, Xc7Part, XC7Z010CLG400_1};
use crate::manifest::{
    Clock, De2Pin, Frequency, Manifest, PinAssignment, QuartusPin, Sources, Target,
};
use chipsmith_cyclone2::pin::{IoRegisters, UnusedPins};
use chipsmith_cyclone2::place::PinPlan;

/// A directory of nextpnr-himbaechel xilinx chip databases,
/// `chipdb-<die>.bin`. Set by the nix wrapper and the devshell.
pub const XC7_CHIPDB_VAR: &str = "CHIPSMITH_XC7_CHIPDB";
pub use chipsmith_xc7::xray::PRJXRAY_DB_VAR;

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
    /// The `.bit` as Linux's Zynq fpga-manager takes it (`xc7::zynq_bin`),
    /// for nixos-fpga's `hardware.fpga.bitstream`.
    pub fn zynq_bin(&self) -> PathBuf {
        self.dir.join(format!("{}.bin", self.name))
    }
    fn translated(&self) -> PathBuf {
        self.dir.join("vhdl.v")
    }
    /// 7-series: yosys' script.
    fn synth_script(&self) -> PathBuf {
        self.dir.join("synth.ys")
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
    /// 7-series: chipsmith's placement, as `-o preplaced` lines.
    fn xc7_placement(&self) -> PathBuf {
        self.dir.join("placement.txt")
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
    /// Read Verilog with the Cyclone II cell library, elaborate `top` and
    /// synthesize it to Cyclone II cells, in process (`chipsmith_verilog`,
    /// `chipsmith_cyclone2::synth`); writes the netlist as yosys JSON.
    /// `io_registers`: per port bit (`LEDR[0]`), the I/O-cell registers
    /// synthesis is asked to fill.
    Synth {
        verilog: Vec<PathBuf>,
        top: String,
        netlist: PathBuf,
        io_registers: BTreeMap<String, IoRegisters>,
    },
    /// Pack a yosys JSON netlist into LEs, pins and clock controls (nextpnr's
    /// JSON), in process.
    Pack { netlist: PathBuf, packed: PathBuf },
    /// Place a packed design (nextpnr JSON) and route it, in process.
    /// `pins` maps top-level port bits (`LEDR[0]`) to pins (`PIN_AE23`)
    /// and their I/O settings, which the placed design carries on to the
    /// bitstream with `unused_pins`. Each global clock's estimated slack is
    /// reported against its frequency: `clocks` for clocks from pins (by
    /// port bit), the PLL's for its outputs.
    PlaceRoute {
        packed: PathBuf,
        placed: PathBuf,
        routed: PathBuf,
        pins: BTreeMap<String, PinPlan>,
        unused_pins: UnusedPins,
        clocks: BTreeMap<String, Frequency>,
    },
    /// Assemble a routed design (nextpnr JSON) into an RBF, in process
    /// (`chipsmith_cyclone2::bitstream`).
    Bitstream { routed: PathBuf, rbf: PathBuf },
    /// Write the JTAG configuration of an RBF as an SVF, in process. A raw
    /// RBF shifted over JTAG configures a Cyclone II that never starts up;
    /// the SVF has the JTAG framing and checks CONF_DONE.
    Svf { rbf: PathBuf, svf: PathBuf },
    /// Report each clock's achieved frequency from nextpnr's `--report`;
    /// fails if one misses its constraint.
    NextpnrTiming(PathBuf),
    /// Place nextpnr's packed 7-series netlist (`--pack-only --write`) in
    /// process, as `-o preplaced` lines; an empty file if chipsmith can't
    /// place the design yet, so nextpnr does.
    Xc7Place { packed: PathBuf, placement: PathBuf },
    /// Route a 7-series design nextpnr routed (`nextpnr`) again with
    /// chipsmith's router, in process: its routes as nextpnr's
    /// `-o prerouted` lines, and the placement as `-o preplaced` lines.
    /// First checks nextpnr kept chipsmith's `placement`.
    Xc7Route {
        nextpnr: PathBuf,
        placement: PathBuf,
        prerouted: PathBuf,
        preplaced: PathBuf,
    },
    /// Play an SVF over the USB-Blaster, in process.
    PlaySvf(PathBuf),
    /// Load a 7-series `.bit` over a Digilent FT2232 cable, in process.
    LoadBit(PathBuf),
    /// Hold the fabric's interrupt lines into the Zynq's processor inactive,
    /// in the yosys netlist, in process (`tie_off_ps7`).
    Ps7TieOff(PathBuf),
    /// Write a `.bit`'s payload as Linux's Zynq fpga-manager takes it, in
    /// process.
    ZynqBin { bit: PathBuf, bin: PathBuf },
    /// Load a 7-series `.bit` into a Zynq's PL through `/dev/fpga-load` on
    /// `host`, piped over ssh.
    LoadBitSsh { bit: PathBuf, host: String },
    /// Build the DE2's serial flash loader into this project directory.
    BuildLoader(PathBuf),
    /// Configure the serial flash loader (an SVF) into the DE2's FPGA over
    /// the USB-Blaster, then read or write the EPCS through it.
    Epcs { loader: PathBuf, op: EpcsOp },
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
            Step::Synth { .. } => "synthesis",
            Step::Pack { .. } => "pack",
            Step::PlaceRoute { .. } => "place and route",
            Step::Bitstream { .. } => "bitstream",
            Step::Svf { .. } => "JTAG stream",
            Step::NextpnrTiming(_) => "nextpnr: timing",
            Step::Xc7Place { .. } => "place (chipsmith)",
            Step::Xc7Route { .. } => "route (chipsmith)",
            Step::PlaySvf(_) => "JTAG: program over USB-Blaster",
            Step::LoadBit(_) => "JTAG: program over Digilent FT2232",
            Step::LoadBitSsh { .. } => "ssh: load through the board's fpga-manager",
            Step::ZynqBin { .. } => ".bit to .bin for Linux's fpga-manager",
            Step::Ps7TieOff(_) => "PS7: the fabric's interrupts to the ARM cores held off",
            Step::BuildLoader(_) => "build the serial flash loader",
            Step::Epcs { op, .. } => match op {
                EpcsOp::Program(_) => "EPCS: write and verify through the serial flash loader",
                EpcsOp::Dump(_) => "EPCS: read through the serial flash loader",
                EpcsOp::Restore(_) => "EPCS: restore a dump through the serial flash loader",
            },
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
        Target::Ep2c35f672c6 {
            pins,
            clocks,
            unused_pins,
        } => plan_cyclone2(manifest, pins, clocks, *unused_pins, layout),
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
    pins: &[PinAssignment<QuartusPin, De2Pin>],
    clocks: &[Clock],
    unused_pins: UnusedPins,
    layout: &Layout,
) -> Plan {
    let mut steps: Vec<Step> = Vec::new();
    let verilog = verilog_sources(manifest, layout, &mut steps);
    steps.push(Step::Synth {
        verilog,
        top: manifest.top.clone(),
        netlist: layout.netlist(),
        io_registers: pins
            .iter()
            .filter(|a| a.io.registers.any())
            .map(|a| (a.port.to_string(), a.io.registers))
            .collect(),
    });
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
            .map(|a| {
                (
                    a.port.to_string(),
                    PinPlan {
                        pin: a.pin.as_str().to_string(),
                        settings: a.io.settings,
                    },
                )
            })
            .collect(),
        unused_pins,
        clocks: clocks
            .iter()
            .map(|c| (c.port.to_string(), c.frequency))
            .collect(),
    });
    steps.push(Step::Bitstream {
        routed: layout.routed(),
        rbf: layout.bitstream(),
    });

    Plan {
        dir: Some(layout.dir.clone()),
        files: Vec::new(),
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
    steps.push(Step::Ps7TieOff(layout.netlist()));
    // nextpnr's packer turns yosys' cells into bel types (and splits
    // carries, adds feed-through LUTs, places the pins from the XDC);
    // chipsmith places what it wrote. Every later nextpnr run packs the
    // netlist again and pins each cell to its bel (`-o preplaced`):
    // packing is deterministic, so the cells and their names are the same.
    steps.push(
        Tool {
            log: Some(layout.log("nextpnr-pack")),
            ..Tool::new("nextpnr: pack", "nextpnr-himbaechel")
        }
        .args(["--device", part.part])
        .arg("--chipdb")
        .arg(&data.chipdb)
        .arg("--json")
        .arg(layout.netlist())
        .arg("-o")
        .arg(prefixed("xdc=", &layout.xdc()))
        .arg("--pack-only")
        .arg("--write")
        .arg(layout.packed())
        .arg("-q")
        .arg("-l")
        .arg(layout.log("nextpnr-pack"))
        .into(),
    );
    steps.push(Step::Xc7Place {
        packed: layout.packed(),
        placement: layout.xc7_placement(),
    });
    // nextpnr routes chipsmith's placement once, for the site routing (the
    // site pins and LUT input pins each net uses) that chipsmith's router
    // starts from.
    steps.push(
        Tool {
            log: Some(layout.log("nextpnr")),
            ..Tool::new("nextpnr: route chipsmith's placement", "nextpnr-himbaechel")
        }
        .args(["--device", part.part])
        .arg("--chipdb")
        .arg(&data.chipdb)
        .arg("--json")
        .arg(layout.netlist())
        .arg("-o")
        .arg(prefixed("xdc=", &layout.xdc()))
        .arg("-o")
        .arg(prefixed("preplaced=", &layout.xc7_placement()))
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
    // and pins each cell to its bel (`-o preplaced`) once more.
    steps.push(Step::Xc7Route {
        nextpnr: layout.nextpnr_routed(),
        placement: layout.xc7_placement(),
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
    steps.push(Step::ZynqBin {
        bit: layout.bitstream(),
        bin: layout.zynq_bin(),
    });

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
    /// A Cyclone II raw bitstream, loaded through its JTAG sequence.
    Rbf(PathBuf),
    /// A JTAG sequence already, from chipsmith or any other tool.
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

/// Where `chipsmith flash` puts a design.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Persistence {
    /// Configuration RAM over JTAG: fast, gone at power-off.
    Volatile,
    /// The board's configuration flash, loaded from at power-up.
    Persistent,
}

/// How `chipsmith flash` reaches the board.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Via {
    /// A JTAG cable on this machine.
    Jtag,
    /// Linux on the board's own CPU, over ssh: nixos-fpga's `/dev/fpga-load`.
    Ssh(String),
}

/// A `chipsmith flash`, checked against what each board can persist and
/// what each way in can load.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Flash {
    Volatile(Loadable),
    /// A Cyclone II RBF into the DE2's EPCS16.
    Epcs(PathBuf),
    /// A 7-series `.bit` into a Zynq's PL from its own Linux.
    Ssh {
        bit: PathBuf,
        host: String,
    },
}

impl Flash {
    pub fn new(path: PathBuf, persistence: Persistence, via: Via) -> Result<Self> {
        let loadable = Loadable::from_path(path)?;
        if let Via::Ssh(host) = via {
            return match (persistence, loadable) {
                (Persistence::Volatile, Loadable::Bit(bit)) => Ok(Flash::Ssh { bit, host }),
                (Persistence::Persistent, _) => bail!(
                    "--persist --ssh: set hardware.fpga.bitstream in {host}'s NixOS config to the \
                     build's .bin (next to the .bit) instead; nixos-fpga loads it at every boot"
                ),
                (Persistence::Volatile, _) => {
                    bail!("--ssh loads a Zynq's PL from its own Linux, which takes a .bit")
                }
            };
        }
        Ok(match (persistence, loadable) {
            (Persistence::Volatile, l) => Flash::Volatile(l),
            (Persistence::Persistent, Loadable::Rbf(rbf)) => Flash::Epcs(rbf),
            (Persistence::Persistent, Loadable::Svf(svf)) => bail!(
                "{}: an SVF is a JTAG sequence, not a bitstream; persist the .rbf it came from",
                svf.display()
            ),
            (Persistence::Persistent, Loadable::Bit(bit)) => bail!(
                "{}: persistent flashing of the Zybo isn't supported yet (README, roadmap step 6)",
                bit.display()
            ),
        })
    }
}

/// The steps that bring the serial flash loader into `build_dir` and then
/// do `op` through it.
pub fn plan_epcs(loader: &Loader, build_dir: &Path, op: EpcsOp) -> Plan {
    let svf = build_dir.join("flash-loader.svf");
    let mut steps = Vec::new();
    if let Loader::Build(dir) = loader {
        steps.push(Step::BuildLoader(dir.clone()));
    }
    steps.push(Step::Svf {
        rbf: loader.rbf(),
        svf: svf.clone(),
    });
    steps.push(Step::Epcs { loader: svf, op });
    Plan {
        dir: Some(build_dir.to_path_buf()),
        files: Vec::new(),
        steps,
    }
}

pub fn plan_flash(flash: &Flash, loader: &Loader, build_dir: &Path) -> Plan {
    let loadable = match flash {
        Flash::Volatile(l) => l,
        Flash::Epcs(rbf) => return plan_epcs(loader, build_dir, EpcsOp::Program(rbf.clone())),
        Flash::Ssh { bit, host } => {
            return Plan {
                dir: None,
                files: Vec::new(),
                steps: vec![Step::LoadBitSsh {
                    bit: bit.clone(),
                    host: host.clone(),
                }],
            }
        }
    };
    let steps = match loadable {
        Loadable::Svf(svf) => vec![Step::PlaySvf(svf.clone())],
        Loadable::Rbf(rbf) => {
            let svf = rbf.with_extension("svf");
            vec![
                Step::Svf {
                    rbf: rbf.clone(),
                    svf: svf.clone(),
                },
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
                Step::Synth {
                    verilog,
                    top,
                    netlist,
                    io_registers,
                } => synth(verilog, top, netlist, io_registers)?,
                Step::Pack { netlist, packed } => pack(netlist, packed)?,
                Step::PlaceRoute {
                    packed,
                    placed,
                    routed,
                    pins,
                    unused_pins,
                    clocks,
                } => place_route(packed, placed, routed, pins, *unused_pins, clocks)?,
                Step::Bitstream { routed, rbf } => bitstream(routed, rbf)?,
                Step::Svf { rbf, svf } => bitstream::svf_file(rbf, svf)?,
                Step::NextpnrTiming(report) => nextpnr_timing(report)?,
                Step::Xc7Place { packed, placement } => xc7_place(packed, placement)?,
                Step::Xc7Route {
                    nextpnr,
                    placement,
                    prerouted,
                    preplaced,
                } => xc7_route(nextpnr, placement, prerouted, preplaced)?,
                Step::PlaySvf(svf) => play_svf(svf)?,
                Step::LoadBit(bit) => load_bit(bit)?,
                Step::LoadBitSsh { bit, host } => load_bit_ssh(bit, host)?,
                Step::ZynqBin { bit, bin } => zynq_bin(bit, bin)?,
                Step::Ps7TieOff(netlist) => {
                    let read = || -> Result<serde_json::Value> {
                        Ok(serde_json::from_slice(&std::fs::read(netlist)?)?)
                    };
                    let mut json =
                        read().with_context(|| format!("reading {}", netlist.display()))?;
                    tie_off_ps7(&mut json)?;
                    std::fs::write(netlist, serde_json::to_vec(&json)?)
                        .with_context(|| format!("writing {}", netlist.display()))?;
                }
                Step::BuildLoader(dir) => build_loader(dir)?,
                Step::Epcs { loader, op } => persist::run_epcs(loader, op)?,
            }
        }
        Ok(())
    }
}

fn synth(
    verilog: &[PathBuf],
    top: &str,
    netlist: &Path,
    io_registers: &BTreeMap<String, IoRegisters>,
) -> Result<()> {
    let started = Instant::now();
    let mut fe = chipsmith_verilog::Frontend::new();
    fe.read_library_source(cells::LIBRARY_NAME, cells::LIBRARY)?;
    for file in verilog {
        fe.read_file(file)?;
    }
    let design = fe.elaborate(top)?;
    eprintln!(
        "    elaborated {} modules in {:.1} s",
        design.modules.iter().filter(|m| !m.is_blackbox()).count(),
        started.elapsed().as_secs_f64()
    );

    let started = Instant::now();
    let out = chipsmith_cyclone2::synth::synthesize(design, io_registers)?;
    let s = out.stats;
    eprintln!(
        "    {} gates ({} AIG ANDs) to {} LUTs, depth {}; {} registers ({} in I/O cells), {} carry chains, {} M4Ks, {} multipliers in {:.1} s",
        s.lutmap.gates,
        s.lutmap.and_nodes,
        s.lutmap.luts,
        s.lutmap.depth,
        s.registers,
        s.io_registers,
        s.chains,
        s.m4ks,
        s.mults,
        started.elapsed().as_secs_f64()
    );
    // Quartus warns the same (10240, 13004): the LE has neither, so both
    // are LUT loops whose power-up value the hardware doesn't define.
    if s.latches > 0 {
        eprintln!(
            "    warning: {} latches, each a LUT feeding itself; their power-up value is undefined",
            s.latches
        );
    }
    if s.emulated > 0 {
        eprintln!(
            "    warning: {} registers with an asynchronous load or set emulated with a latch; \
             their power-up value is undefined",
            s.emulated
        );
    }
    let file = std::fs::File::create(netlist)
        .with_context(|| format!("creating {}", netlist.display()))?;
    chipsmith_netlist::write_json(&out.design, file)
        .with_context(|| format!("writing {}", netlist.display()))
}

fn pack(netlist: &Path, packed: &Path) -> Result<()> {
    let started = Instant::now();
    let s = chipsmith_cyclone2::pack::pack_file(netlist, packed)?;
    eprintln!(
        "    {} LUTs, {} registers ({} on the sync load path), {} carry chains, {} M4Ks, {} clocks in {:.1} s; \
         removed {} stuck registers, {} constant and {} one-input LUTs",
        s.luts,
        s.ffs,
        s.sdata,
        s.chains,
        s.m4ks,
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
    pins: &BTreeMap<String, PinPlan>,
    unused_pins: UnusedPins,
    clocks: &BTreeMap<String, Frequency>,
) -> Result<()> {
    let started = Instant::now();
    let graph = chipsmith_cyclone2::route::load_graph()?;
    eprintln!(
        "    routing graph in {:.1} s",
        started.elapsed().as_secs_f64()
    );

    let started = Instant::now();
    let p = chipsmith_cyclone2::place::place_file(&graph, packed, pins, unused_pins, placed)?;
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
    let r = chipsmith_cyclone2::route::route_file(&graph, placed, routed)?;
    eprintln!(
        "    routed {} nets, {} pips, {} iterations in {:.1} s; critical path {:.2} ns (estimate)",
        r.nets,
        r.pips,
        r.iterations,
        started.elapsed().as_secs_f64(),
        r.critical / 1000.0
    );
    for line in clock_report(&r, clocks) {
        eprintln!("    {line}");
    }
    Ok(())
}

/// One line per global clock with paths of its own (Fmax, and slack when
/// its frequency is known), one per pair of clocks with paths between them.
fn clock_report(
    r: &chipsmith_cyclone2::route::Stats,
    clocks: &BTreeMap<String, Frequency>,
) -> Vec<String> {
    use chipsmith_cyclone2::route::ClockSource;
    let hz = |i: usize| -> Option<f64> {
        match &r.clocks[i].source {
            ClockSource::Pin(port) => clocks.get(port).map(|f| f.hz() as f64),
            ClockSource::Pll { hz, .. } => Some(*hz),
            ClockSource::Core => None,
        }
    };
    let what = |i: usize| -> String {
        let c = &r.clocks[i];
        match (&c.source, hz(i)) {
            (ClockSource::Pin(port), Some(f)) => format!("clock {port} ({:.3} MHz)", f / 1e6),
            (ClockSource::Pin(port), None) => format!("clock {port} (no frequency in [clocks])"),
            (ClockSource::Pll { cell, output, hz }, _) => {
                format!("clock {} ({cell} c{output}, {:.3} MHz)", c.name, hz / 1e6)
            }
            (ClockSource::Core, _) => format!("clock {} (from logic)", c.name),
        }
    };
    let mut out = Vec::new();
    for p in &r.pairs {
        let ns = f64::from(p.critical) / 1000.0;
        if p.from == p.to {
            let slack = hz(p.from).map(|f| 1e9 / f - ns);
            out.push(format!(
                "{}: critical path {ns:.2} ns, Fmax {:.1} MHz{} (estimate)",
                what(p.from),
                1e3 / ns,
                match slack {
                    Some(s) if s < 0.0 => format!(", slack {s:.2} ns; FAILS timing"),
                    Some(s) => format!(", slack {s:.2} ns"),
                    None => String::new(),
                }
            ));
        } else {
            out.push(format!(
                "{} -> {}: {ns:.2} ns, between clocks (not checked)",
                what(p.from),
                what(p.to)
            ));
        }
    }
    out
}

fn bitstream(routed: &Path, rbf: &Path) -> Result<()> {
    let started = Instant::now();
    let db = bitstream::load_db()?;
    eprintln!(
        "    device database in {:.1} s",
        started.elapsed().as_secs_f64()
    );
    let started = Instant::now();
    let s = bitstream::pack_file(&db, routed, rbf)?;
    eprintln!(
        "    {} LEs, {} pins, {} nets: model bits {:?} + {} replicated-frame I/O bits in {:.1} s",
        s.les,
        s.pins,
        s.nets,
        s.bits,
        s.rep_bits,
        started.elapsed().as_secs_f64()
    );
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

fn xc7_place(packed: &Path, placement: &Path) -> Result<()> {
    let started = Instant::now();
    let graph = chipsmith_xc7::xray::load(XC7Z010CLG400_1)?;
    eprintln!(
        "    routing graph in {:.1} s",
        started.elapsed().as_secs_f64()
    );
    let delays = chipsmith_xc7::delay::Delays::load(
        &Path::new(
            &std::env::var_os(PRJXRAY_DB_VAR)
                .with_context(|| format!("{PRJXRAY_DB_VAR} is not set"))?,
        )
        .join(XC7Z010CLG400_1.family),
    )?;
    match chipsmith_xc7::place::place_file(&graph, Some(&delays), packed, placement)? {
        chipsmith_xc7::place::Outcome::Placed(s) => eprintln!(
            "    placed {} cells ({} LUT/FF pairs, {} carry chains, {} RAM32Ms), \
             wirelength {:.0} (from {:.0}), estimated critical path {:.2} ns, \
             {} moves on {} threads in {:.1} s",
            s.cells,
            s.bles,
            s.carry_chains,
            s.rams,
            s.wirelength,
            s.initial_wirelength,
            s.critical / 1000.0,
            s.moves,
            s.threads,
            s.seconds
        ),
        chipsmith_xc7::place::Outcome::Unsupported(u) => {
            eprintln!("    not placed by chipsmith ({}); nextpnr places it", u.0)
        }
    }
    Ok(())
}

fn xc7_route(nextpnr: &Path, placement: &Path, prerouted: &Path, preplaced: &Path) -> Result<()> {
    let started = Instant::now();
    let graph = chipsmith_xc7::xray::load(XC7Z010CLG400_1)?;
    eprintln!(
        "    routing graph in {:.1} s",
        started.elapsed().as_secs_f64()
    );
    let kept = chipsmith_xc7::route::check_placement(&graph, nextpnr, placement)?;
    if kept > 0 {
        eprintln!("    nextpnr kept chipsmith's bel for all {kept} cells");
    }
    let r = chipsmith_xc7::route::route_file(&graph, nextpnr, prerouted, preplaced)?;
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

/// The PS7's IRQF2P[19:16] are the ARM cores' FIQ and IRQ, straight from
/// the fabric and active high; an interconnect input that nothing drives
/// reads as 1. So a design without a PS7 asserts FIQ on both cores, and the
/// first time the PL->PS level shifters are on with Linux running (Linux
/// loading it, or a JTAG load under U-Boot) both cores spin in the FIQ
/// handler for good. This gives such a design a PS7 with IRQF2P tied to 0,
/// or ties off the IRQF2P of the design's own PS7 if it leaves it open.
fn tie_off_ps7(netlist: &mut serde_json::Value) -> Result<()> {
    let zeros = || serde_json::json!(vec!["0"; 20]);
    let top = netlist["modules"]
        .as_object_mut()
        .context("netlist has no modules")?
        .values_mut()
        .find(|m| {
            m["attributes"]["top"]
                .as_str()
                .is_some_and(|t| t.ends_with('1'))
        })
        .context("netlist has no top module")?;
    let cells = top["cells"]
        .as_object_mut()
        .context("top module has no cells")?;
    if let Some(ps7) = cells.values_mut().find(|c| c["type"] == "PS7") {
        let connections = ps7["connections"]
            .as_object_mut()
            .context("PS7 without connections")?;
        connections.entry("IRQF2P").or_insert_with(zeros);
        ps7["port_directions"]["IRQF2P"] = "input".into();
    } else {
        cells.insert(
            "chipsmith_ps7".into(),
            serde_json::json!({
                "hide_name": 0,
                "type": "PS7",
                "parameters": {},
                "attributes": {},
                "port_directions": { "IRQF2P": "input" },
                "connections": { "IRQF2P": zeros() },
            }),
        );
    }
    Ok(())
}

fn zynq_bin(bit: &Path, bin: &Path) -> Result<()> {
    let file = std::fs::read(bit).with_context(|| format!("reading {}", bit.display()))?;
    std::fs::write(
        bin,
        chipsmith_xc7::zynq::zynq_bin(xc7::bit_payload(&file)?)?,
    )
    .with_context(|| format!("writing {}", bin.display()))
}

fn load_bit_ssh(path: &Path, host: &str) -> Result<()> {
    let file = std::fs::read(path).with_context(|| format!("reading {}", path.display()))?;
    let bin = chipsmith_xc7::zynq::zynq_bin(xc7::bit_payload(&file)?)?;
    let started = Instant::now();
    // The load runs when /dev/fpga-load is closed and its error is close()'s,
    // which cat reports. The manager's state confirms the PL came up.
    let mut child = Command::new("ssh")
        .arg(host)
        .arg("cat > /dev/fpga-load && cat /sys/class/fpga_manager/fpga0/state")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .context("running ssh")?;
    {
        use std::io::Write;
        let mut stdin = child.stdin.take().context("ssh's stdin")?;
        stdin
            .write_all(&bin)
            .with_context(|| format!("sending the bitstream to {host}"))?;
    }
    let out = child.wait_with_output().context("waiting for ssh")?;
    if !out.status.success() {
        bail!(
            "{host}: loading through /dev/fpga-load failed ({})",
            out.status
        );
    }
    let state = String::from_utf8_lossy(&out.stdout);
    let state = state.trim();
    if state != "operating" {
        bail!("{host}: fpga-manager is {state:?} after the load, not \"operating\"");
    }
    eprintln!(
        "    loaded {} KiB, manager operating, in {:.1} s",
        bin.len() / 1024,
        started.elapsed().as_secs_f64()
    );
    Ok(())
}

/// Write the loader's sources into `dir` and build it.
fn build_loader(dir: &Path) -> Result<()> {
    std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    std::fs::write(dir.join("chipsmith.toml"), persist::LOADER_MANIFEST)?;
    std::fs::write(dir.join("top.v"), persist::LOADER_TOP)?;
    let manifest = Manifest::load(dir)?;
    let layout = Layout::new(dir, &manifest);
    plan_build(&manifest, &layout)?.run()
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

    /// The DE2 manifest's plan, with `sources` in place of the file's.
    fn cyclone2(sources: Sources) -> Plan {
        let m = manifest_for(DE2, sources);
        let Target::Ep2c35f672c6 {
            pins,
            clocks,
            unused_pins,
        } = &m.target
        else {
            unreachable!()
        };
        plan_cyclone2(&m, pins, clocks, *unused_pins, &layout(&m))
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
    fn verilog_goes_straight_to_chipsmiths_synthesis() {
        let plan = cyclone2(Sources::Verilog(vec!["/p/src/blinky.v".into()]));
        assert_eq!(
            labels(&plan),
            ["synthesis", "pack", "place and route", "bitstream"]
        );
        assert!(plan.files.is_empty());
        assert_eq!(
            plan.steps[0],
            Step::Synth {
                verilog: vec!["/p/src/blinky.v".into()],
                top: "blinky".into(),
                netlist: "/p/build/netlist.json".into(),
                io_registers: BTreeMap::new(),
            }
        );
        assert_eq!(
            plan.steps[1],
            Step::Pack {
                netlist: "/p/build/netlist.json".into(),
                packed: "/p/build/packed.json".into()
            }
        );
        assert_eq!(
            plan.steps[3],
            Step::Bitstream {
                routed: "/p/build/routed.json".into(),
                rbf: "/p/build/blinky.rbf".into()
            }
        );
    }

    #[test]
    fn vhdl_is_translated_to_verilog_and_synthesis_reads_the_translation() {
        let plan = cyclone2(Sources::Vhdl(vec!["/p/src/blinky.vhd".into()]));
        assert_eq!(
            labels(&plan),
            [
                "ghdl: VHDL to Verilog",
                "synthesis",
                "pack",
                "place and route",
                "bitstream"
            ]
        );
        assert_eq!(
            tool(&plan.steps[0]).stdout.as_deref(),
            Some(Path::new("/p/build/vhdl.v"))
        );
        let Step::Synth { verilog, .. } = &plan.steps[1] else {
            panic!("{:?} is not synthesis", plan.steps[1]);
        };
        assert_eq!(verilog, &[PathBuf::from("/p/build/vhdl.v")]);
    }

    /// ADR-0001: no external program between the Verilog and the chip.
    #[test]
    fn the_de2_build_runs_no_tool_but_ghdl() {
        for sources in [
            Sources::Verilog(vec!["/p/src/blinky.v".into()]),
            Sources::Vhdl(vec!["/p/src/blinky.vhd".into()]),
        ] {
            let plan = cyclone2(sources);
            let tools: Vec<&OsString> = plan
                .steps
                .iter()
                .filter_map(|s| match s {
                    Step::Tool(t) => Some(&t.program),
                    _ => None,
                })
                .filter(|p| *p != "ghdl")
                .collect();
            assert!(tools.is_empty(), "{tools:?}");
        }
    }

    #[test]
    fn pins_and_the_clock_reach_the_placer() {
        let plan = cyclone2(Sources::Verilog(vec![]));
        let Step::PlaceRoute { pins, clocks, .. } = &plan.steps[2] else {
            panic!("{:?} is not place and route", plan.steps[2]);
        };
        assert_eq!(clocks.get("CLOCK_50").map(|f| f.hz()), Some(50_000_000));
        let pins: Vec<(&str, &str)> = pins
            .iter()
            .map(|(a, b)| (a.as_str(), b.pin.as_str()))
            .collect();
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
    fn pin_settings_reach_the_placer_and_register_requests_synthesis() {
        let text = DE2.replace(
            "LEDR = [\"PIN_AE23\", \"PIN_AF23\"]",
            "LEDR = { pins = [\"PIN_AE23\", \"PIN_AF23\"], drive = \"8 mA\", registers = [\"output\"] }",
        );
        let text = text.replace("[target]\n", "[target]\nunused_pins = \"bus-hold\"\n");
        let m = manifest_for(&text, Sources::Verilog(vec![]));
        let Target::Ep2c35f672c6 {
            pins,
            clocks,
            unused_pins,
        } = &m.target
        else {
            unreachable!()
        };
        let plan = plan_cyclone2(&m, pins, clocks, *unused_pins, &layout(&m));
        let Step::Synth { io_registers, .. } = &plan.steps[0] else {
            panic!("{:?} is not synthesis", plan.steps[0]);
        };
        assert_eq!(
            io_registers.keys().collect::<Vec<_>>(),
            ["LEDR[0]", "LEDR[1]"]
        );
        let Step::PlaceRoute {
            pins, unused_pins, ..
        } = &plan.steps[2]
        else {
            panic!("{:?} is not place and route", plan.steps[2]);
        };
        assert_eq!(*unused_pins, UnusedPins::BusHold);
        assert_eq!(
            pins["LEDR[1]"].settings.params(),
            [("CURRENT_STRENGTH_NEW", "8MA".to_string())]
        );
        assert!(pins["CLOCK_50"].settings.params().is_empty());
    }

    #[test]
    fn the_zynq_goes_through_openxc7_and_prjxray_to_a_bit() {
        let plan = xc7(Sources::Verilog(vec!["/p/src/blinky.v".into()]));
        assert_eq!(
            labels(&plan),
            [
                "yosys: synthesis (synth_xilinx)",
                "PS7: the fabric's interrupts to the ARM cores held off",
                "nextpnr: pack",
                "place (chipsmith)",
                "nextpnr: route chipsmith's placement",
                "route (chipsmith)",
                "nextpnr: FASM of chipsmith's routing",
                "nextpnr: timing",
                "prjxray: FASM to frames",
                "prjxray: frames to .bit",
                ".bit to .bin for Linux's fpga-manager"
            ]
        );
        assert_eq!(
            plan.files[0].1,
            "read_verilog \"/p/src/blinky.v\"\nsynth_xilinx -flatten -abc9 -arch xc7 -top blinky\nwrite_json \"/p/build/netlist.json\"\n"
        );
        let pack = tool(&plan.steps[2]);
        assert_eq!(pack.program, "nextpnr-himbaechel");
        assert!(pack
            .args
            .windows(2)
            .any(|w| w == ["--device", "xc7z010clg400-1"]));
        assert!(pack
            .args
            .windows(2)
            .any(|w| w == ["--chipdb", "/db/chipdb-xc7z010.bin"]));
        assert!(pack.args.iter().any(|a| a == "--pack-only"));
        assert!(pack
            .args
            .windows(2)
            .any(|w| w == ["--write", "/p/build/packed.json"]));
        assert_eq!(
            plan.steps[3],
            Step::Xc7Place {
                packed: "/p/build/packed.json".into(),
                placement: "/p/build/placement.txt".into(),
            }
        );
        // nextpnr routes chipsmith's placement, with nothing left to place.
        assert!(tool(&plan.steps[4])
            .args
            .windows(2)
            .any(|w| w == ["-o", "preplaced=/p/build/placement.txt"]));
        assert_eq!(
            plan.steps[5],
            Step::Xc7Route {
                nextpnr: "/p/build/nextpnr.json".into(),
                placement: "/p/build/placement.txt".into(),
                prerouted: "/p/build/prerouted.txt".into(),
                preplaced: "/p/build/preplaced.txt".into(),
            }
        );
        let fasm = tool(&plan.steps[6]);
        assert!(fasm
            .args
            .windows(2)
            .any(|w| w == ["--report", "/p/build/timing.json"]));
        assert_eq!(
            plan.steps[7],
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
            tool(&plan.steps[8]).args.last().unwrap(),
            "/p/build/blinky.frames"
        );
        let bit = tool(&plan.steps[9]);
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
        assert_eq!(
            plan.steps[1],
            Step::Ps7TieOff("/p/build/netlist.json".into())
        );
        assert_eq!(
            plan.steps[10],
            Step::ZynqBin {
                bit: "/p/build/blinky.bit".into(),
                bin: "/p/build/blinky.bin".into()
            }
        );
    }

    #[test]
    fn a_design_without_a_ps7_gets_one_with_its_interrupts_tied_off() {
        let mut netlist = serde_json::json!({"modules": {
            "lib": {"attributes": {}, "cells": {}},
            "top": {"attributes": {"top": "00000000000000000000000000000001"}, "cells": {}},
        }});
        tie_off_ps7(&mut netlist).unwrap();
        let ps7 = &netlist["modules"]["top"]["cells"]["chipsmith_ps7"];
        assert_eq!(ps7["type"], "PS7");
        assert_eq!(
            ps7["connections"]["IRQF2P"],
            serde_json::json!(vec!["0"; 20])
        );
        assert_eq!(netlist["modules"]["lib"]["cells"], serde_json::json!({}));
    }

    #[test]
    fn a_designs_own_ps7_keeps_its_interrupts_or_gets_them_tied_off() {
        let ps7 = |connections| {
            serde_json::json!({"modules": {"top": {
                "attributes": {"top": "1"},
                "cells": {"mine": {"type": "PS7", "port_directions": {}, "connections": connections}},
            }}})
        };
        let mut open = ps7(serde_json::json!({}));
        tie_off_ps7(&mut open).unwrap();
        let cells = &open["modules"]["top"]["cells"];
        assert_eq!(cells.as_object().unwrap().len(), 1);
        assert_eq!(
            cells["mine"]["connections"]["IRQF2P"],
            serde_json::json!(vec!["0"; 20])
        );

        let driven = serde_json::json!({"IRQF2P": vec![5; 20]});
        let mut used = ps7(driven.clone());
        tie_off_ps7(&mut used).unwrap();
        assert_eq!(
            used["modules"]["top"]["cells"]["mine"]["connections"],
            driven
        );
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

    fn volatile(path: &str) -> Flash {
        Flash::new(path.into(), Persistence::Volatile, Via::Jtag).unwrap()
    }

    #[test]
    fn a_bit_over_ssh_goes_to_the_boards_fpga_manager() {
        let flash = Flash::new(
            "/p/build/blinky.bit".into(),
            Persistence::Volatile,
            Via::Ssh("venus".into()),
        )
        .unwrap();
        let plan = plan_flash(
            &flash,
            &Loader::Prebuilt("/l.rbf".into()),
            Path::new("/p/build"),
        );
        assert_eq!(
            plan.steps,
            [Step::LoadBitSsh {
                bit: "/p/build/blinky.bit".into(),
                host: "venus".into()
            }]
        );
    }

    #[test]
    fn ssh_takes_a_volatile_bit_only() {
        let ssh = || Via::Ssh("venus".into());
        assert!(Flash::new("/x.rbf".into(), Persistence::Volatile, ssh()).is_err());
        assert!(Flash::new("/x.svf".into(), Persistence::Volatile, ssh()).is_err());
        assert!(Flash::new("/x.bit".into(), Persistence::Persistent, ssh()).is_err());
    }

    #[test]
    fn a_bit_is_loaded_over_the_ft2232() {
        let loader = Loader::Prebuilt("/l.rbf".into());
        let plan = plan_flash(
            &volatile("/p/build/blinky.bit"),
            &loader,
            Path::new("/p/build"),
        );
        assert_eq!(plan.steps, [Step::LoadBit("/p/build/blinky.bit".into())]);
    }

    #[test]
    fn a_persistent_rbf_goes_through_the_serial_flash_loader() {
        let flash = Flash::new(
            "/p/build/blinky.rbf".into(),
            Persistence::Persistent,
            Via::Jtag,
        )
        .unwrap();
        assert_eq!(flash, Flash::Epcs("/p/build/blinky.rbf".into()));
        let build = Path::new("/p/build");
        let plan = plan_flash(
            &flash,
            &Loader::Build("/p/build/flash-loader".into()),
            build,
        );
        assert_eq!(
            labels(&plan),
            [
                "build the serial flash loader",
                "JTAG stream",
                "EPCS: write and verify through the serial flash loader"
            ]
        );
        assert_eq!(
            plan.steps[1],
            Step::Svf {
                rbf: "/p/build/flash-loader/build/loader.rbf".into(),
                svf: "/p/build/flash-loader.svf".into()
            }
        );
        assert_eq!(
            plan.steps[2],
            Step::Epcs {
                loader: "/p/build/flash-loader.svf".into(),
                op: EpcsOp::Program("/p/build/blinky.rbf".into())
            }
        );
        // The nix package's prebuilt loader needs no build step.
        let plan = plan_flash(&flash, &Loader::Prebuilt("/nix/l.rbf".into()), build);
        assert_eq!(plan.steps.len(), 2);
    }

    #[test]
    fn only_an_rbf_can_be_persisted() {
        assert!(Flash::new("/x.svf".into(), Persistence::Persistent, Via::Jtag).is_err());
        assert!(Flash::new("/x.bit".into(), Persistence::Persistent, Via::Jtag).is_err());
    }
}
