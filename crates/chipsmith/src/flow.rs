//! HDL to bitstream to chip. The device decides the flow.
//!
//! Cyclone II: no external program between the Verilog and the chip
//! (ADR-0001). GHDL translates VHDL to Verilog in front of the flow; the
//! Verilog front end (`chipsmith_verilog`), synthesis
//! (`chipsmith_cyclone2::synth`), packing, placement, routing, the
//! bitstream and its JTAG stream (`chipsmith_cyclone2`) and loading over the
//! cable (`chipsmith_jtag`) are chipsmith's own.
//!
//! 7-series (Zynq-7010): no external program either. The Verilog front
//! end, synthesis (`chipsmith_xc7::synth`), packing, placement, routing
//! and the FASM (`chipsmith_xc7::pnr`), the frames and `.bit`
//! (`chipsmith_xc7::frames`, from prjxray-db's bits) and loading over the
//! Zybo's FT2232 cable (`chipsmith_jtag::ftdi`, `chipsmith_jtag::xc7`) are
//! chipsmith's own.
//!
//! Agilex 5 (the DE25-Nano): no external program either, but data from
//! the user's own Quartus Prime Pro install (its SDM firmware and the
//! routing graph agilex5-re dumps from it) and the board's HPS-first shell
//! from its one Quartus build. Synthesis, place and route and assembly are
//! `chipsmith_agilex5`'s; the core loads from the board's own Linux.
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

use anyhow::{bail, ensure, Context, Result};

use crate::cache::{self, say, Stamp};
use chipsmith_cyclone2::bitstream;
use chipsmith_cyclone2::persist::{self, EpcsOp, Loader};
use chipsmith_cyclone2::synth::cells;
use chipsmith_jtag::ftdi::Ftdi;
use chipsmith_jtag::usb_blaster::UsbBlaster;
use chipsmith_jtag::{svf, xc7};

use crate::device::{Clg400Pin, Device, Xc7Part, XC7Z010CLG400_1};
use crate::manifest::{
    A5Pin, Clock, De2Pin, Frequency, HpsFirst, Manifest, PinAssignment, QuartusPin, Sources, Target,
};
use chipsmith_cyclone2::pin::{IoRegisters, UnusedPins};
use chipsmith_cyclone2::place::PinPlan;

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
            // Phase 2 of an HPS-first boot, as quartus_pfg names it.
            Device::A5eb013bb23be4scs => "core.rbf",
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
    fn fasm(&self) -> PathBuf {
        self.dir.join(format!("{}.fasm", self.name))
    }
    fn a5_pins(&self) -> PathBuf {
        self.dir.join("pins.txt")
    }
    fn a5_design(&self) -> PathBuf {
        self.dir.join("design.txt")
    }
    fn sof(&self) -> PathBuf {
        self.dir.join(format!("{}.sof", self.name))
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
    /// Read Verilog with the 7-series cell library, elaborate `top` and
    /// synthesize it to 7-series primitives, in process
    /// (`chipsmith_xc7::synth`); writes the netlist as yosys JSON, its
    /// PS7's interrupts tied off (`tie_off_ps7`).
    Xc7Synth {
        verilog: Vec<PathBuf>,
        top: String,
        netlist: PathBuf,
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
    /// Pack, place and route a 7-series netlist and write its FASM, in
    /// process (`chipsmith_xc7::pnr`). `pins` maps port bits (`led[0]`) to
    /// package pins (`M14`); each clock's critical path is reported
    /// against `clocks` (by port bit) and fails the build if it misses.
    Xc7Pnr {
        netlist: PathBuf,
        fasm: PathBuf,
        pins: BTreeMap<String, String>,
        clocks: BTreeMap<String, Frequency>,
    },
    /// Write a 7-series design's FASM as a `.bit`, in process
    /// (`chipsmith_xc7::frames`, what prjxray's `fasm2frames` and
    /// `xc7frames2bit` make of it); `design` names it in the header.
    Xc7Bitstream {
        fasm: PathBuf,
        bit: PathBuf,
        design: String,
    },
    /// Play an SVF over the USB-Blaster, in process.
    PlaySvf(PathBuf),
    /// Load a 7-series `.bit` over a Digilent FT2232 cable, in process.
    LoadBit(PathBuf),
    /// Write a `.bit`'s payload as Linux's Zynq fpga-manager takes it, in
    /// process.
    ZynqBin { bit: PathBuf, bin: PathBuf },
    /// Load a 7-series `.bit` into a Zynq's PL through `/dev/fpga-load` on
    /// `host`, piped over ssh.
    LoadBitSsh { bit: PathBuf, host: String },
    /// Write a Zynq `.bit` as a QSPI boot image (chipsmith's loader and the
    /// bitstream) through the PS's DAP over the Digilent FT2232, verify it,
    /// and start it.
    ZynqQspi(PathBuf),
    /// Read Verilog with the Agilex 5 cell library, elaborate `top` and
    /// synthesize it to ALM-sized LUTs, carry chains, flops, MLAB RAM and
    /// DSP products, in process (`chipsmith_agilex5::synth`); writes the
    /// netlist as yosys JSON.
    A5Synth {
        verilog: Vec<PathBuf>,
        top: String,
        netlist: PathBuf,
    },
    /// Pack, place and route an Agilex 5 netlist and write its design file,
    /// in process (`chipsmith_agilex5::pnr`); `pins` is the pin file it
    /// reads (`<port> <ball> <volts>`, `<port> node <node>`).
    A5Pnr {
        netlist: PathBuf,
        top: String,
        pins: PathBuf,
        design: PathBuf,
    },
    /// Assemble the design file and the board's HPS shell into the `.sof`
    /// and the core.rbf for phase 1's HPS IO hash, in process
    /// (`chipsmith_agilex5::flow::assemble`).
    A5Assemble {
        design: PathBuf,
        hps: HpsFirst,
        sof: PathBuf,
        core: PathBuf,
    },
    /// Load an Agilex 5 core through `/dev/fpga-load` on `host`, piped over
    /// ssh, once its HPS IO hash is checked to be `hps_io_hash`: any other
    /// wedges the SDM.
    A5LoadSsh {
        core: PathBuf,
        host: String,
        hps_io_hash: [u8; 32],
    },
    /// Build the DE2's serial flash loader into this project directory.
    BuildLoader(PathBuf),
    /// Configure the serial flash loader (an SVF) into the DE2's FPGA over
    /// the USB-Blaster, then read or write the EPCS through it.
    Epcs { loader: PathBuf, op: EpcsOp },
}

/// The 7-series tools' data, resolved for one part.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Xc7Data {
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
            db: var(PRJXRAY_DB_VAR)?.join(part.family),
        };
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
            Step::Xc7Synth { .. } => "synthesis",
            Step::Pack { .. } => "pack",
            Step::PlaceRoute { .. } => "place and route",
            Step::Bitstream { .. } => "bitstream",
            Step::Svf { .. } => "JTAG stream",
            Step::Xc7Pnr { .. } => "pack, place and route",
            Step::Xc7Bitstream { .. } => "bitstream",
            Step::PlaySvf(_) => "JTAG: program over USB-Blaster",
            Step::LoadBit(_) => "JTAG: program over Digilent FT2232",
            Step::LoadBitSsh { .. } => "ssh: load through the board's fpga-manager",
            Step::ZynqQspi(_) => "QSPI: write and verify a boot image through the ARM's debug port",
            Step::ZynqBin { .. } => ".bit to .bin for Linux's fpga-manager",
            Step::A5Synth { .. } => "synthesis",
            Step::A5Pnr { .. } => "pack, place and route",
            Step::A5Assemble { .. } => "bitstream",
            Step::A5LoadSsh { .. } => "ssh: load the core through the board's fpga-manager",
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
            // The steps read prjxray-db through the environment when they
            // run; a missing one fails here, before any step.
            Xc7Data::from_env(XC7Z010CLG400_1)?;
            plan_xc7(manifest, pins, clocks, layout)
        }
        Target::A5eb013bb23be4scs { pins, hps } => {
            agilex5_data()?;
            plan_agilex5(manifest, pins, hps, layout)
        }
    })
}

/// agilex5-re's database, routing graph and the Quartus files the image
/// writer reads (the nix package and devshell set all three).
/// CHIPSMITH_AGILEX5_QUARTUS can also name a whole Quartus Prime Pro 26.1.1
/// install.
pub fn agilex5_data() -> Result<chipsmith_agilex5::flow::Data> {
    let var = |name: &str| {
        std::env::var_os(name).map(PathBuf::from).with_context(|| {
            format!("{name} is not set; run chipsmith from its nix package or `nix develop`")
        })
    };
    Ok(chipsmith_agilex5::flow::Data {
        db: var("CHIPSMITH_AGILEX5_DB")?,
        graph: var("CHIPSMITH_AGILEX5_GRAPH")?,
        quartus: var("CHIPSMITH_AGILEX5_QUARTUS")?,
    })
}

/// The pin file place and route reads.
fn agilex5_pins(pins: &[PinAssignment<A5Pin>]) -> String {
    pins.iter()
        .map(|a| match &a.pin {
            A5Pin::Ball { ball, vccio } => format!("{} {ball} {}\n", a.port, vccio.volts()),
            A5Pin::Hps(h) => format!("{} node {}\n", a.port, h.node()),
        })
        .collect()
}

pub fn plan_agilex5(
    manifest: &Manifest,
    pins: &[PinAssignment<A5Pin>],
    hps: &HpsFirst,
    layout: &Layout,
) -> Plan {
    let mut steps: Vec<Step> = Vec::new();
    let verilog = verilog_sources(manifest, layout, &mut steps);
    steps.push(Step::A5Synth {
        verilog,
        top: manifest.top.clone(),
        netlist: layout.netlist(),
    });
    steps.push(Step::A5Pnr {
        netlist: layout.netlist(),
        top: manifest.top.clone(),
        pins: layout.a5_pins(),
        design: layout.a5_design(),
    });
    steps.push(Step::A5Assemble {
        design: layout.a5_design(),
        hps: hps.clone(),
        sof: layout.sof(),
        core: layout.bitstream(),
    });
    Plan {
        dir: Some(layout.dir.clone()),
        files: vec![(layout.a5_pins(), agilex5_pins(pins))],
        steps,
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
/// 3.3 V, and the manifest has no I/O standards yet.
pub fn plan_xc7(
    manifest: &Manifest,
    pins: &[PinAssignment<Clg400Pin>],
    clocks: &[Clock],
    layout: &Layout,
) -> Plan {
    let mut steps: Vec<Step> = Vec::new();
    let verilog = verilog_sources(manifest, layout, &mut steps);
    steps.push(Step::Xc7Synth {
        verilog,
        top: manifest.top.clone(),
        netlist: layout.netlist(),
    });
    steps.push(Step::Xc7Pnr {
        netlist: layout.netlist(),
        fasm: layout.fasm(),
        pins: pins
            .iter()
            .map(|a| (a.port.to_string(), a.pin.to_string()))
            .collect(),
        clocks: clocks
            .iter()
            .map(|c| (c.port.to_string(), c.frequency))
            .collect(),
    });
    steps.push(Step::Xc7Bitstream {
        fasm: layout.fasm(),
        bit: layout.bitstream(),
        design: manifest.name.clone(),
    });
    steps.push(Step::ZynqBin {
        bit: layout.bitstream(),
        bin: layout.zynq_bin(),
    });

    Plan {
        dir: Some(layout.dir.clone()),
        files: Vec::new(),
        steps,
    }
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
    /// A 7-series `.bit` into a Zynq's QSPI flash, as a boot image.
    ZynqQspi(PathBuf),
    /// A 7-series `.bit` into a Zynq's PL from its own Linux.
    Ssh {
        bit: PathBuf,
        host: String,
    },
    /// An Agilex 5 core into the fabric from the HPS's own Linux, for the
    /// phase 1 with this HPS IO hash.
    Agilex5Ssh {
        core: PathBuf,
        host: String,
        hps_io_hash: [u8; 32],
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
            (Persistence::Persistent, Loadable::Bit(bit)) => Flash::ZynqQspi(bit),
        })
    }
}

impl Flash {
    /// A `chipsmith flash` of a DE25-Nano project: only its core, only from
    /// the board's Linux.
    pub fn agilex5(
        path: PathBuf,
        persistence: Persistence,
        via: Via,
        hps: &HpsFirst,
    ) -> Result<Self> {
        match (persistence, via) {
            (Persistence::Volatile, Via::Ssh(host)) => Ok(Flash::Agilex5Ssh {
                core: path,
                host,
                hps_io_hash: hps.hps_io_hash,
            }),
            (Persistence::Persistent, _) => bail!(
                "--persist: writing the DE25-Nano's QSPI isn't supported yet; load the core with --ssh HOST"
            ),
            (Persistence::Volatile, Via::Jtag) => bail!(
                "the DE25-Nano's core loads from its HPS's Linux after phase 1: --ssh HOST"
            ),
        }
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
        Flash::ZynqQspi(bit) => {
            return Plan {
                dir: None,
                files: Vec::new(),
                steps: vec![Step::ZynqQspi(bit.clone())],
            }
        }
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
        Flash::Agilex5Ssh {
            core,
            host,
            hps_io_hash,
        } => {
            return Plan {
                dir: None,
                files: Vec::new(),
                steps: vec![Step::A5LoadSsh {
                    core: core.clone(),
                    host: host.clone(),
                    hps_io_hash: *hps_io_hash,
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

impl Plan {
    pub fn run(&self) -> Result<()> {
        if let Some(dir) = &self.dir {
            std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
        }
        for (path, contents) in &self.files {
            std::fs::write(path, contents)
                .with_context(|| format!("writing {}", path.display()))?;
        }
        // The routing graph (0.5 s from its cache) and the bitstream's
        // device database (3 s, most of it xz) load on threads of their
        // own while the stages in front of them run; a small design used
        // to spend most of its build waiting for them.
        let needs = |f: fn(&Step) -> bool| self.steps.iter().any(f);
        let mut graph = needs(|s| matches!(s, Step::PlaceRoute { .. }))
            .then(|| Prefetch::spawn(chipsmith_cyclone2::route::load_graph));
        let mut device_db = needs(|s| matches!(s, Step::Bitstream { .. }))
            .then(|| Prefetch::spawn(bitstream::load_db));
        for step in &self.steps {
            eprintln!("  {}", step.label());
            match step {
                Step::Tool(tool) => run_tool(tool)?,
                Step::Synth {
                    verilog,
                    top,
                    netlist,
                    io_registers,
                } => stamp(step, &[netlist]).run(|| synth(verilog, top, netlist, io_registers))?,
                Step::Xc7Synth {
                    verilog,
                    top,
                    netlist,
                } => stamp(step, &[netlist]).run(|| xc7_synth(verilog, top, netlist))?,
                Step::Pack { netlist, packed } => stamp(step, &[packed]).run(|| {
                    pack(netlist, packed)?;
                    Ok(vec![netlist.clone()])
                })?,
                Step::PlaceRoute {
                    packed,
                    placed,
                    routed,
                    pins,
                    unused_pins,
                    clocks,
                } => stamp(step, &[routed, placed]).run(|| {
                    place_route(
                        Prefetch::take(&mut graph, chipsmith_cyclone2::route::load_graph),
                        packed,
                        placed,
                        routed,
                        pins,
                        *unused_pins,
                        clocks,
                    )?;
                    Ok(vec![packed.clone()])
                })?,
                Step::Bitstream { routed, rbf } => stamp(step, &[rbf]).run(|| {
                    bitstream(
                        Prefetch::take(&mut device_db, bitstream::load_db),
                        routed,
                        rbf,
                    )?;
                    Ok(vec![routed.clone()])
                })?,
                Step::Svf { rbf, svf } => bitstream::svf_file(rbf, svf)?,
                Step::Xc7Pnr {
                    netlist,
                    fasm,
                    pins,
                    clocks,
                } => stamp(step, &[fasm]).run(|| {
                    xc7_pnr(netlist, fasm, pins, clocks)?;
                    Ok(vec![netlist.clone()])
                })?,
                Step::Xc7Bitstream { fasm, bit, design } => stamp(step, &[bit]).run(|| {
                    xc7_bitstream(fasm, bit, design)?;
                    Ok(vec![fasm.clone()])
                })?,
                Step::PlaySvf(svf) => play_svf(svf)?,
                Step::LoadBit(bit) => load_bit(bit)?,
                Step::LoadBitSsh { bit, host } => load_bit_ssh(bit, host)?,
                Step::ZynqQspi(bit) => zynq_qspi(bit)?,
                Step::ZynqBin { bit, bin } => zynq_bin(bit, bin)?,
                Step::A5Synth {
                    verilog,
                    top,
                    netlist,
                } => stamp(step, &[netlist]).run(|| {
                    let sources: Vec<&Path> = verilog.iter().map(PathBuf::as_path).collect();
                    chipsmith_agilex5::synth::run(
                        top,
                        &sources,
                        netlist,
                        &chipsmith_agilex5::synth::Options::default(),
                    )
                })?,
                Step::A5Pnr {
                    netlist,
                    top,
                    pins,
                    design,
                } => stamp(step, &[design]).run(|| {
                    a5_pnr(netlist, top, pins, design)?;
                    Ok(vec![netlist.clone(), pins.clone()])
                })?,
                Step::A5Assemble {
                    design,
                    hps,
                    sof,
                    core,
                } => stamp(step, &[core, sof]).run(|| {
                    a5_assemble(design, hps, sof, core)?;
                    Ok(vec![design.clone(), hps.shell.clone()])
                })?,
                Step::A5LoadSsh {
                    core,
                    host,
                    hps_io_hash,
                } => a5_load_ssh(core, host, hps_io_hash)?,
                Step::BuildLoader(dir) => build_loader(dir)?,
                Step::Epcs { loader, op } => persist::run_epcs(loader, op)?,
            }
        }
        Ok(())
    }
}

/// An in-process step's stamp (ADR-0011), keyed on the step as planned.
fn stamp(step: &Step, outputs: &[&Path]) -> Stamp {
    Stamp::new(cache::key(&format!("{step:?}")), outputs)
}

/// Returns every file the front end read, for the step's stamp.
fn synth(
    verilog: &[PathBuf],
    top: &str,
    netlist: &Path,
    io_registers: &BTreeMap<String, IoRegisters>,
) -> Result<Vec<PathBuf>> {
    let started = Instant::now();
    let mut fe = chipsmith_verilog::Frontend::new();
    fe.read_library_source(cells::LIBRARY_NAME, cells::LIBRARY)?;
    for file in verilog {
        fe.read_file(file)?;
    }
    let design = fe.elaborate(top)?;
    say!(
        "elaborated {} modules in {:.1} s",
        design.modules.iter().filter(|m| !m.is_blackbox()).count(),
        started.elapsed().as_secs_f64()
    );

    let started = Instant::now();
    let out = chipsmith_cyclone2::synth::synthesize(design, io_registers)?;
    let s = out.stats;
    say!(
        "{} gates ({} AIG ANDs) to {} LUTs, depth {}; {} registers ({} in I/O cells), {} carry chains, {} M4Ks, {} multipliers in {:.1} s",
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
        say!(
            "warning: {} latches, each a LUT feeding itself; their power-up value is undefined",
            s.latches
        );
    }
    if s.emulated > 0 {
        say!(
            "warning: {} registers with an asynchronous load or set emulated with a latch; \
             their power-up value is undefined",
            s.emulated
        );
    }
    let file = std::fs::File::create(netlist)
        .with_context(|| format!("creating {}", netlist.display()))?;
    chipsmith_netlist::write_json(&out.design, file)
        .with_context(|| format!("writing {}", netlist.display()))?;
    Ok(fe.files_read())
}

/// Returns every file the front end read, for the step's stamp.
fn xc7_synth(verilog: &[PathBuf], top: &str, netlist: &Path) -> Result<Vec<PathBuf>> {
    use chipsmith_xc7::synth::{self as xsynth, cells as xcells};
    let started = Instant::now();
    let mut fe = chipsmith_verilog::Frontend::new();
    fe.read_library_source(xcells::LIBRARY_NAME, xcells::LIBRARY)?;
    for file in verilog {
        fe.read_file(file)?;
    }
    let design = fe.elaborate(top)?;
    say!(
        "elaborated {} modules in {:.1} s",
        design.modules.iter().filter(|m| !m.is_blackbox()).count(),
        started.elapsed().as_secs_f64()
    );
    let started = Instant::now();
    let out = xsynth::synthesize(design)?;
    let s = out.stats;
    say!(
        "{} gates ({} AIG ANDs) to {} LUTs, depth {}; {} registers ({} copies), {} CARRY4s, {} RAM32Ms, {} block RAM (RAMB18 halves), {} DSP48E1s, {} BUFGs in {:.1} s",
        s.lutmap.gates,
        s.lutmap.and_nodes,
        s.lutmap.luts,
        s.lutmap.depth,
        s.registers,
        s.replicas,
        s.carry4s,
        s.ram32ms,
        s.bram_halves,
        s.dsps,
        s.bufgs,
        started.elapsed().as_secs_f64()
    );
    for m in &out.design.modules {
        for c in &m.cells {
            let chipsmith_netlist::CellKind::Other(g) = &c.kind else {
                continue;
            };
            let Some(prim) = chipsmith_xc7::cmt::Prim::from_cell(&g.ty) else {
                continue;
            };
            let used: Vec<bool> = (0..prim.outputs())
                .map(|k| g.port(&format!("CLKOUT{k}")).is_some())
                .collect();
            let config = chipsmith_xc7::cmt::Config::from_params(prim, &g.params, &used)?;
            let period = match g.params.get("CLKIN1_PERIOD") {
                Some(chipsmith_netlist::ParamValue::Str(p)) => {
                    (p.parse::<f64>().unwrap_or(0.0) * 1000.0).round() as u32
                }
                _ => 0,
            };
            say!(
                "{} {}: {}",
                prim.cell(),
                c.name,
                config.summary(period.max(1))
            );
        }
    }
    let mut json = Vec::new();
    chipsmith_netlist::write_json(&out.design, &mut json)?;
    let mut json: serde_json::Value = serde_json::from_slice(&json)?;
    tie_off_ps7(&mut json)?;
    say!("PS7: the fabric's interrupts to the ARM cores held off");
    std::fs::write(netlist, serde_json::to_vec(&json)?)
        .with_context(|| format!("writing {}", netlist.display()))?;
    Ok(fe.files_read())
}

fn xc7_pnr(
    netlist: &Path,
    fasm: &Path,
    pins: &BTreeMap<String, String>,
    clocks: &BTreeMap<String, Frequency>,
) -> Result<()> {
    let part = XC7Z010CLG400_1;
    let started = Instant::now();
    let graph = chipsmith_xc7::xray::load(part)?;
    say!("routing graph in {:.1} s", started.elapsed().as_secs_f64());
    let data = Xc7Data::from_env(part)?;
    let delays = chipsmith_xc7::delay::Delays::load(&data.db)?;
    let r = chipsmith_xc7::pnr::run(&graph, &delays, &data.db, netlist, pins, fasm)?;
    say!(
        "packed {} LUTs ({} LUT6 sites), {} registers, {} CARRY4s, {} RAM32Ms into {} slices; {} RAMB18E1s, {} RAMB36E1s, {} DSP48E1s",
        r.luts,
        r.lut_sites,
        r.registers,
        r.carry4s,
        r.ram32ms,
        r.slices,
        r.ramb18s,
        r.ramb36s,
        r.dsps
    );
    say!(
        "placed {} cells ({} BLEs, {} carry chains, {} RAMs; {} start), wirelength {:.0} (from {:.0}), estimated critical path {:.2} ns, {} moves on {} threads in {:.1} s",
        r.place.cells,
        r.place.bles,
        r.place.carry_chains,
        r.place.rams,
        if r.place.electro { "electrostatic" } else { "greedy" },
        r.place.wirelength,
        r.place.initial_wirelength,
        r.place.critical / 1000.0,
        r.place.moves,
        r.place.threads,
        r.place_seconds
    );
    say!(
        "routed {} nets, {} pips, {} iterations in {:.1} s",
        r.nets,
        r.pips,
        r.iterations,
        r.route_seconds
    );
    // A PLL's outputs follow from the input period it was solved for, so
    // that has to be the pin's.
    for i in &r.cmt_inputs {
        if let Some(f) = clocks.get(&i.clock) {
            let want = 1e6 / f.mhz();
            ensure!(
                (want - f64::from(i.period_ps)).abs() < 0.5,
                "{}: INCLK0_INPUT_FREQUENCY {} ps, but clock {} is {} MHz ({want:.0} ps)",
                i.cell,
                i.period_ps,
                i.clock,
                f.mhz()
            );
        }
    }
    let mut failed = Vec::new();
    for c in &r.clocks {
        let ns = c.critical / 1000.0;
        let fmax = if ns > 0.0 {
            1e3 / ns as f64
        } else {
            f64::INFINITY
        };
        let period_ns = clocks
            .get(&c.name)
            .map(|f| f.period_ns())
            .or(c.hz.map(|hz| 1e9 / hz));
        match period_ns {
            Some(period) => {
                let slack = period - ns as f64;
                say!(
                    "clock {}: {ns:.2} ns, Fmax {fmax:.1} MHz at {:.3} MHz, slack {slack:.3} ns (estimate)",
                    c.name,
                    1e3 / period
                );
                if slack < 0.0 {
                    failed.push(c.name.clone());
                }
            }
            None => say!(
                "clock {}: {ns:.2} ns, Fmax {fmax:.1} MHz (estimate)",
                c.name
            ),
        }
    }
    if !failed.is_empty() {
        bail!("clocks missing their frequency: {}", failed.join(", "));
    }
    Ok(())
}

fn xc7_bitstream(fasm: &Path, bit: &Path, design: &str) -> Result<()> {
    let started = Instant::now();
    let part = XC7Z010CLG400_1;
    let data = Xc7Data::from_env(part)?;
    let text =
        std::fs::read_to_string(fasm).with_context(|| format!("reading {}", fasm.display()))?;
    let frames = chipsmith_xc7::frames::Assembler::new(&data.db, part)?.frames(&text)?;
    let bytes = chipsmith_xc7::frames::bit(&data.db, part.part, design, &frames)?;
    std::fs::write(bit, &bytes).with_context(|| format!("writing {}", bit.display()))?;
    say!(
        "{} frames, {} bytes in {:.1} s",
        frames.len(),
        bytes.len(),
        started.elapsed().as_secs_f64()
    );
    Ok(())
}

fn pack(netlist: &Path, packed: &Path) -> Result<()> {
    let started = Instant::now();
    let s = chipsmith_cyclone2::pack::pack_file(netlist, packed)?;
    say!(
        "{} LUTs, {} registers ({} on the sync load path), {} carry chains, {} M4Ks, {} clocks in {:.1} s; \
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

/// Something loading on a thread of its own until a step needs it.
struct Prefetch<T>(std::thread::JoinHandle<Result<T>>);

impl<T: Send + 'static> Prefetch<T> {
    fn spawn(load: fn() -> Result<T>) -> Self {
        Prefetch(std::thread::spawn(load))
    }

    /// What the thread loaded (waiting for it), or a load on the spot if
    /// it was already taken: a plan has one step of each kind, but nothing
    /// enforces that.
    fn take(slot: &mut Option<Self>, load: fn() -> Result<T>) -> Timed<T> {
        let started = Instant::now();
        let value = match slot.take() {
            Some(Prefetch(handle)) => match handle.join() {
                Ok(r) => r,
                Err(panic) => std::panic::resume_unwind(panic),
            },
            None => load(),
        };
        Timed {
            value,
            waited: started.elapsed().as_secs_f64(),
        }
    }
}

/// A prefetched value and how long the step had to wait for it.
struct Timed<T> {
    value: Result<T>,
    waited: f64,
}

fn place_route(
    graph: Timed<chipsmith_route::chipdb::ChipDb>,
    packed: &Path,
    placed: &Path,
    routed: &Path,
    pins: &BTreeMap<String, PinPlan>,
    unused_pins: UnusedPins,
    clocks: &BTreeMap<String, Frequency>,
) -> Result<()> {
    let waited = graph.waited;
    let graph = graph.value?;
    say!("routing graph in {waited:.1} s");

    let started = Instant::now();
    let p = chipsmith_cyclone2::place::place_file(&graph, packed, pins, unused_pins, placed)?;
    say!(
        "placed {} LEs ({} carry chains), wirelength {} -> {} in {:.1} s; critical path {:.2} ns (estimate)",
        p.les,
        p.chains,
        p.initial_cost,
        p.cost,
        started.elapsed().as_secs_f64(),
        p.critical / 1000.0
    );

    let started = Instant::now();
    let r = chipsmith_cyclone2::route::route_file(&graph, placed, routed)?;
    say!(
        "routed {} nets, {} pips, {} iterations in {:.1} s; critical path {:.2} ns (estimate)",
        r.nets,
        r.pips,
        r.iterations,
        started.elapsed().as_secs_f64(),
        r.critical / 1000.0
    );
    for line in clock_report(&r, clocks) {
        say!("{line}");
    }
    Ok(())
}

/// One line per global clock with paths of its own (Fmax, and slack when
/// its frequency is known), one per pair of clocks with paths between them.
fn clock_report(
    r: &chipsmith_cyclone2::route::Stats,
    clocks: &BTreeMap<String, Frequency>,
) -> Vec<String> {
    use chipsmith_cyclone2::route::{ClockSource, Edge};
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
    let edge = |e: Edge| match e {
        Edge::Rising => "rising",
        Edge::Falling => "falling",
    };
    let mut out = Vec::new();
    for p in &r.pairs {
        let ns = f64::from(p.critical) / 1000.0;
        if p.from == p.to {
            // From one edge to the other the path has half a period (a
            // pin's clock or a PLL output at 50% duty cycle).
            let (label, periods) = match (p.from_edge, p.to_edge) {
                (Edge::Rising, Edge::Rising) => (String::new(), 1.0),
                (Edge::Falling, Edge::Falling) => (", falling edge".to_string(), 1.0),
                (a, b) => (
                    format!(", {} to {} edge, half a period", edge(a), edge(b)),
                    0.5,
                ),
            };
            let slack = hz(p.from).map(|f| periods * 1e9 / f - ns);
            out.push(format!(
                "{}{label}: critical path {ns:.2} ns, Fmax {:.1} MHz{} (estimate)",
                what(p.from),
                periods * 1e3 / ns,
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

fn bitstream(db: Timed<bitstream::phys::Db>, routed: &Path, rbf: &Path) -> Result<()> {
    let waited = db.waited;
    let db = db.value?;
    say!("device database in {waited:.1} s");
    let started = Instant::now();
    let s = bitstream::pack_file(&db, routed, rbf)?;
    say!(
        "{} LEs, {} pins, {} nets: model bits {:?} + {} replicated-frame I/O bits in {:.1} s",
        s.les,
        s.pins,
        s.nets,
        s.bits,
        s.rep_bits,
        started.elapsed().as_secs_f64()
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

/// `flash --persist` on a Zybo. The flash is read first, and left alone
/// if it already holds the image (a read is about a tenth of a write).
/// With the boot mode on QSPI the PS is reset, so the BootROM boots the
/// image as at power-on; otherwise the PL is configured from the flash
/// directly, so the design runs now either way.
fn zynq_qspi(path: &Path) -> Result<()> {
    use chipsmith_jtag::adiv5::{Dap, MemAp};
    use chipsmith_jtag::zynq_ps::{self, BootMode};
    use chipsmith_jtag::zynq_qspi::{self, Qspi};
    use std::time::Duration;

    let file = std::fs::read(path).with_context(|| format!("reading {}", path.display()))?;
    let bin = chipsmith_xc7::zynq::zynq_bin(xc7::bit_payload(&file)?)?;
    let image = chipsmith_xc7::zynq::qspi_boot_image(&bin)?;
    let bin_at = (image.len() - bin.len()) as u32;

    let mut cable = Ftdi::open()?;
    let chain = xc7::scan(&mut cable)?;
    // The Zynq's ARM debug port (IR 4 bits) beside the PL's TAP.
    let target = chain
        .iter()
        .position(|t| t.ir_len == 4)
        .context("no ARM DAP on the JTAG chain; is this a Zynq?")?;
    let mut dap = Dap::new(&mut cable, &chain, target)?;
    let mut ahb = MemAp::new(&mut dap, 0)?;
    let started = Instant::now();
    Qspi::new(&mut ahb)?.into_linear()?;
    let current = zynq_qspi::read_linear(&mut ahb, 0, image.len(), |_| ())?;
    if current == image {
        eprintln!(
            "    the flash already holds this image ({} bytes, checked in {:.1} s)",
            image.len(),
            started.elapsed().as_secs_f64()
        );
    } else {
        Qspi::new(&mut ahb)?.write(0, &image, |_| ())?;
        Qspi::new(&mut ahb)?.into_linear()?;
        let back = zynq_qspi::read_linear(&mut ahb, 0, image.len(), |_| ())?;
        if let Some(k) = back.iter().zip(&image).position(|(a, b)| a != b) {
            bail!(
                "QSPI read back differs at {k:#x}: {:#04x}, wrote {:#04x}",
                back[k],
                image[k]
            );
        }
        eprintln!(
            "    wrote and verified {} bytes of boot image in {:.1} s",
            image.len(),
            started.elapsed().as_secs_f64()
        );
    }
    match zynq_ps::boot_mode(&mut ahb)? {
        BootMode::Qspi => {
            zynq_ps::soft_reset(&mut ahb)?;
            let sts =
                zynq_ps::wait_configured(&mut cable, &chain, target, Duration::from_secs(15))?;
            eprintln!("    reset: the BootROM booted it from QSPI (devcfg INT_STS {sts:#010x})");
        }
        mode => {
            zynq_ps::pcap_from_flash(&mut ahb, bin_at, bin.len() as u32)?;
            eprintln!(
                "    configured from flash now; the boot mode is {mode:?}, so it boots at \
                 power-on only with the boot-mode jumper (the Zybo's JP5) on QSPI"
            );
        }
    }
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

fn a5_pnr(netlist: &Path, top: &str, pins: &Path, design: &Path) -> Result<()> {
    let data = agilex5_data()?;
    let s = |p: &Path| {
        p.to_str()
            .map(str::to_owned)
            .with_context(|| format!("{} is not UTF-8", p.display()))
    };
    chipsmith_agilex5::pnr::run(
        &s(&data.chip())?,
        &s(&data.graph)?,
        &s(&data.delays())?,
        &s(netlist)?,
        top,
        &s(pins)?,
        &s(design)?,
        &chipsmith_agilex5::pnr::Options::default(),
    )
}

/// Place and route's design file with the board's HPS shell after it.
fn a5_assemble(design: &Path, hps: &HpsFirst, sof: &Path, core: &Path) -> Result<()> {
    let read =
        |p: &Path| std::fs::read_to_string(p).with_context(|| format!("reading {}", p.display()));
    let text = read(design)? + &read(&hps.shell)?;
    let rbf = chipsmith_agilex5::flow::assemble(&agilex5_data()?, &text, hps.hps_io_hash, sof)?;
    std::fs::write(core, rbf).with_context(|| format!("writing {}", core.display()))
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|b| format!("{b:02X}")).collect()
}

/// The SDM wedges on a core for another phase 1 (agilex5-re README), so the
/// hash is checked here, before anything reaches the board.
fn a5_load_ssh(core: &Path, host: &str, hps_io_hash: &[u8; 32]) -> Result<()> {
    let file = std::fs::read(core).with_context(|| format!("reading {}", core.display()))?;
    ensure!(
        chipsmith_agilex5::flow::is_rbf(&file),
        "{}: not an Agilex 5 .rbf",
        core.display()
    );
    let carried = chipsmith_agilex5::flow::hps_io_hash(&file)
        .with_context(|| format!("{}: no IO section, so no HPS IO hash", core.display()))?;
    ensure!(
        &carried == hps_io_hash,
        "{}: built for the phase 1 with HPS IO hash {}, but the manifest's is {}; the SDM would wedge",
        core.display(),
        hex(&carried),
        hex(hps_io_hash)
    );
    let started = Instant::now();
    // As load_bit_ssh: the load runs when /dev/fpga-load is closed.
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
            .write_all(&file)
            .with_context(|| format!("sending the core to {host}"))?;
    }
    let out = child.wait_with_output().context("waiting for ssh")?;
    ensure!(
        out.status.success(),
        "{host}: loading through /dev/fpga-load failed ({})",
        out.status
    );
    let state = String::from_utf8_lossy(&out.stdout);
    let state = state.trim();
    ensure!(
        state == "operating",
        "{host}: fpga-manager is {state:?} after the load, not \"operating\""
    );
    eprintln!(
        "    loaded {} KiB, manager operating, in {:.1} s",
        file.len() / 1024,
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
        plan_xc7(&m, pins, clocks, &layout(&m))
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
    fn the_zynq_build_runs_no_tool() {
        let plan = xc7(Sources::Verilog(vec!["/p/src/blinky.v".into()]));
        assert!(!plan.steps.iter().any(|s| matches!(s, Step::Tool(_))));
        assert_eq!(
            labels(&plan),
            [
                "synthesis",
                "pack, place and route",
                "bitstream",
                ".bit to .bin for Linux's fpga-manager"
            ]
        );
        assert_eq!(
            plan.steps[0],
            Step::Xc7Synth {
                verilog: vec!["/p/src/blinky.v".into()],
                top: "blinky".into(),
                netlist: "/p/build/netlist.json".into(),
            }
        );
        let Step::Xc7Pnr {
            netlist,
            fasm,
            pins,
            clocks,
        } = &plan.steps[1]
        else {
            panic!("{:?} is not place and route", plan.steps[1]);
        };
        assert_eq!(netlist, Path::new("/p/build/netlist.json"));
        assert_eq!(fasm, Path::new("/p/build/blinky.fasm"));
        assert_eq!(
            pins.iter()
                .map(|(a, b)| (a.as_str(), b.as_str()))
                .collect::<Vec<_>>(),
            [("clk", "L16"), ("led[0]", "M14"), ("led[1]", "M15")]
        );
        assert_eq!(clocks.get("clk").map(|f| f.hz()), Some(125_000_000));
        assert_eq!(
            plan.steps[2],
            Step::Xc7Bitstream {
                fasm: "/p/build/blinky.fasm".into(),
                bit: "/p/build/blinky.bit".into(),
                design: "blinky".into(),
            }
        );
        assert_eq!(
            plan.steps[3],
            Step::ZynqBin {
                bit: "/p/build/blinky.bit".into(),
                bin: "/p/build/blinky.bin".into()
            }
        );
        assert!(plan.files.is_empty());
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
    fn a_persistent_bit_becomes_a_qspi_boot_image() {
        let flash = Flash::new(
            "/p/build/blinky.bit".into(),
            Persistence::Persistent,
            Via::Jtag,
        )
        .unwrap();
        assert_eq!(flash, Flash::ZynqQspi("/p/build/blinky.bit".into()));
        let plan = plan_flash(
            &flash,
            &Loader::Prebuilt("/l.rbf".into()),
            Path::new("/p/build"),
        );
        assert_eq!(plan.steps, [Step::ZynqQspi("/p/build/blinky.bit".into())]);
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
    fn an_svf_cant_be_persisted() {
        assert!(Flash::new("/x.svf".into(), Persistence::Persistent, Via::Jtag).is_err());
    }

    const DE25: &str = r#"
        [project]
        name = "irq"
        top = "irq"
        sources = ["x.v"]

        [target]
        device = "A5EB013BB23BE4SCS"
        hps_shell = "x.v"
        hps_io_hash = "D921CDF7CADFEECA11572B24600BFD337FFD119E47601CA5075AA911EEC3D07A"

        [pins]
        CLOCK1_50 = { pin = "V16", vccio = "3.3 V" }
        irq0 = "f2h_irq0[0]"
        irq3 = "f2h_irq0[3]"
    "#;

    fn de25() -> (Manifest, Plan) {
        let m = manifest_for(DE25, Sources::Verilog(vec!["/p/irq.v".into()]));
        let Target::A5eb013bb23be4scs { pins, hps } = &m.target else {
            unreachable!()
        };
        let plan = plan_agilex5(&m, pins, hps, &layout(&m));
        (m, plan)
    }

    #[test]
    fn the_de25_build_writes_place_and_routes_pin_file_and_a_core() {
        let (_, plan) = de25();
        assert_eq!(
            labels(&plan),
            ["synthesis", "pack, place and route", "bitstream"]
        );
        assert_eq!(
            plan.files,
            [(
                PathBuf::from("/p/build/pins.txt"),
                "CLOCK1_50 V16 3.3\nirq0 node RPII:X52Y51Z0I44\nirq3 node RPII:X52Y51Z0I24\n"
                    .to_string()
            )]
        );
        let Step::A5Assemble { core, .. } = &plan.steps[2] else {
            panic!("{:?}", plan.steps[2]);
        };
        assert_eq!(core, Path::new("/p/build/irq.core.rbf"));
    }

    #[test]
    fn the_de25s_core_loads_only_from_its_linux() {
        let (m, _) = de25();
        let Target::A5eb013bb23be4scs { hps, .. } = &m.target else {
            unreachable!()
        };
        let core = PathBuf::from("/p/build/irq.core.rbf");
        let ssh = Via::Ssh("mercury".into());
        assert_eq!(
            Flash::agilex5(core.clone(), Persistence::Volatile, ssh.clone(), hps).unwrap(),
            Flash::Agilex5Ssh {
                core: core.clone(),
                host: "mercury".into(),
                hps_io_hash: hps.hps_io_hash
            }
        );
        assert!(Flash::agilex5(core.clone(), Persistence::Volatile, Via::Jtag, hps).is_err());
        assert!(Flash::agilex5(core, Persistence::Persistent, ssh, hps).is_err());
    }

    /// A core's sections as far as the hash check reads them: the CMF
    /// section's header at +0x400, then an IO section with its HPS IO hash.
    fn core_with(hps_io_hash: [u8; 32]) -> Vec<u8> {
        let mut f = vec![0u8; 0x2000];
        f[0x400..0x404].copy_from_slice(&0x9756_6593u32.to_le_bytes());
        f[0x408..0x40c].copy_from_slice(&0x1000u32.to_le_bytes());
        f[0x1000..0x1004].copy_from_slice(&0x9756_6593u32.to_le_bytes());
        f[0x1008..0x100c].copy_from_slice(&0x1000u32.to_le_bytes());
        f[0x100c..0x1010].copy_from_slice(b"IO\0\0");
        f[0x1374..0x1394].copy_from_slice(&hps_io_hash);
        f
    }

    #[test]
    fn a_core_for_another_phase_1_never_reaches_the_board() {
        let dir = std::env::temp_dir().join(format!("chipsmith-a5load-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let core = dir.join("x.core.rbf");
        std::fs::write(&core, core_with([1; 32])).unwrap();
        assert_eq!(
            chipsmith_agilex5::flow::hps_io_hash(&core_with([1; 32])),
            Some([1; 32])
        );
        // No such host: the check has to fail before ssh is run.
        let e = a5_load_ssh(&core, "no-such-host.invalid", &[2; 32]).unwrap_err();
        assert!(e.to_string().contains("the SDM would wedge"), "{e}");
        std::fs::write(&core, vec![0xff; 0x2000]).unwrap();
        let e = a5_load_ssh(&core, "no-such-host.invalid", &[2; 32]).unwrap_err();
        assert!(e.to_string().contains("not an Agilex 5 .rbf"), "{e}");
    }
}
