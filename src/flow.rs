//! HDL to bitstream to chip. Some stages are still external tools: GHDL
//! (VHDL to Verilog), yosys (synthesis) and cram (bitstream, JTAG stream).
//! Packing (`pack`), placement (`place`), routing (`route`) and loading
//! over the cable (`jtag`) are chipsmith's own. Each stage is one
//! `Step` in a `Plan`, so replacing a tool with chipsmith's own code means
//! replacing one step.
//!
//! Planning is pure: `plan_build` and `plan_flash` return every file to
//! write and every command to run, and `Plan::run` does it.

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use std::time::Instant;

use anyhow::{bail, Context, Result};

use crate::jtag::svf;
use crate::jtag::usb_blaster::UsbBlaster;

use crate::manifest::{Manifest, Sources};

/// Where the cyclone2-re yosys techmap lives: a directory holding `synth/`.
/// Set by the nix wrapper and the devshell.
pub const SYNTH_DIR_VAR: &str = "CHIPSMITH_CYCLONE2_SYNTH";

/// Every artifact path of a build, in one place.
#[derive(Debug, Clone)]
pub struct Layout {
    pub dir: PathBuf,
    name: String,
}

impl Layout {
    pub fn new(project_dir: &Path, manifest: &Manifest) -> Self {
        Layout {
            dir: project_dir.join("build"),
            name: manifest.name.clone(),
        }
    }

    pub fn bitstream(&self) -> PathBuf {
        self.dir.join(format!("{}.rbf", self.name))
    }
    fn translated(&self) -> PathBuf {
        self.dir.join("vhdl.v")
    }
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
    fn log(&self, tool: &str) -> PathBuf {
        self.dir.join(format!("{tool}.log"))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Step {
    /// An external program.
    Tool(Tool),
    /// Pack a yosys netlist into LEs, pins and clock controls (nextpnr's
    /// JSON), in process.
    Pack { netlist: PathBuf, packed: PathBuf },
    /// Place a packed design (nextpnr JSON) and route it, in process.
    /// `pins` maps top-level port bits (`LEDR[0]`) to pins (`PIN_AE23`).
    PlaceRoute {
        packed: PathBuf,
        placed: PathBuf,
        routed: PathBuf,
        pins: BTreeMap<String, String>,
    },
    /// Play an SVF over the USB-Blaster, in process.
    PlaySvf(PathBuf),
}

impl Step {
    pub fn label(&self) -> &'static str {
        match self {
            Step::Tool(tool) => tool.label,
            Step::Pack { .. } => "pack",
            Step::PlaceRoute { .. } => "place and route",
            Step::PlaySvf(_) => "JTAG: program over USB-Blaster",
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

pub fn plan_build(manifest: &Manifest, layout: &Layout, synth_dir: &Path) -> Plan {
    let mut steps: Vec<Step> = Vec::new();

    let verilog: Vec<PathBuf> = match &manifest.sources {
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
    };

    steps.push(
        Tool {
            // cyclone2.ys names its maps relative to the directory holding synth/.
            cwd: Some(synth_dir.to_path_buf()),
            log: Some(layout.log("yosys")),
            ..Tool::new("yosys: synthesis", "yosys")
        }
        .arg("-q")
        .arg("-l")
        .arg(layout.log("yosys"))
        .arg("-s")
        .arg(layout.synth_script())
        .into(),
    );
    steps.push(Step::Pack {
        netlist: layout.netlist(),
        packed: layout.packed(),
    });
    steps.push(Step::PlaceRoute {
        packed: layout.packed(),
        placed: layout.placed(),
        routed: layout.routed(),
        pins: manifest
            .pins
            .iter()
            .map(|a| (a.port.to_string(), a.pin.as_str().to_string()))
            .collect(),
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
        files: vec![(
            layout.synth_script(),
            synth_script(&verilog, &manifest.top, &layout.netlist()),
        )],
        steps,
    }
}

/// What `chipsmith flash` can load.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Loadable {
    /// A Cyclone II raw bitstream; cram turns it into its JTAG sequence.
    Rbf(PathBuf),
    /// A JTAG sequence already, from cram or any other tool.
    Svf(PathBuf),
}

impl Loadable {
    pub fn from_path(path: PathBuf) -> Result<Self> {
        match path.extension().and_then(|e| e.to_str()) {
            Some("rbf") => Ok(Loadable::Rbf(path)),
            Some("svf") => Ok(Loadable::Svf(path)),
            _ => bail!("{}: can load .rbf or .svf files", path.display()),
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
    };
    Plan {
        dir: None,
        files: Vec::new(),
        steps,
    }
}

fn synth_script(verilog: &[PathBuf], top: &str, netlist: &Path) -> String {
    let mut script = String::new();
    for file in verilog {
        script += &format!("read_verilog {}\n", quoted(file));
    }
    // cyclone2.ys synthesises the module called `top`.
    if top != "top" {
        script += &format!("rename {top} top\n");
    }
    script += "script synth/cyclone2.ys\n";
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
                Step::Pack { netlist, packed } => pack(netlist, packed)?,
                Step::PlaceRoute {
                    packed,
                    placed,
                    routed,
                    pins,
                } => place_route(packed, placed, routed, pins)?,
                Step::PlaySvf(svf) => play_svf(svf)?,
            }
        }
        Ok(())
    }
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

pub fn synth_dir() -> Result<PathBuf> {
    let dir = std::env::var_os(SYNTH_DIR_VAR)
        .map(PathBuf::from)
        .with_context(|| {
            format!(
                "{SYNTH_DIR_VAR} is not set; run chipsmith from its nix package or `nix develop`"
            )
        })?;
    if !dir.join("synth/cyclone2.ys").is_file() {
        bail!("{SYNTH_DIR_VAR}={} has no synth/cyclone2.ys", dir.display());
    }
    Ok(dir)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn manifest(sources: Sources) -> Manifest {
        let dir = std::env::temp_dir().join(format!("chipsmith-flow-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("x.v"), "").unwrap();
        let text = r#"
            [project]
            name = "blinky"
            top = "blinky"
            sources = ["x.v"]
            [target]
            device = "EP2C35F672C6"
            [pins]
            CLOCK_50 = "PIN_N2"
            LEDR = ["PIN_AE23", "PIN_AF23"]
        "#;
        Manifest {
            sources,
            ..Manifest::parse(text, &dir).unwrap()
        }
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
    fn verilog_goes_straight_to_yosys() {
        let m = manifest(Sources::Verilog(vec!["/p/src/blinky.v".into()]));
        let layout = Layout {
            dir: "/p/build".into(),
            name: m.name.clone(),
        };
        let plan = plan_build(&m, &layout, Path::new("/synth"));
        assert_eq!(
            labels(&plan),
            [
                "yosys: synthesis",
                "pack",
                "place and route",
                "cram: bitstream"
            ]
        );
        let (_, script) = &plan.files[0];
        assert_eq!(
            script,
            "read_verilog \"/p/src/blinky.v\"\nrename blinky top\nscript synth/cyclone2.ys\nwrite_json \"/p/build/netlist.json\"\n"
        );
        // No nextpnr: chipsmith packs what yosys wrote.
        assert_eq!(
            plan.steps[1],
            Step::Pack {
                netlist: "/p/build/netlist.json".into(),
                packed: "/p/build/packed.json".into()
            }
        );
        assert_eq!(
            tool(&plan.steps[3]).args.last().unwrap(),
            "/p/build/blinky.rbf"
        );
    }

    #[test]
    fn vhdl_is_translated_to_verilog_and_yosys_reads_the_translation() {
        let m = manifest(Sources::Vhdl(vec!["/p/src/blinky.vhd".into()]));
        let layout = Layout {
            dir: "/p/build".into(),
            name: m.name.clone(),
        };
        let plan = plan_build(&m, &layout, Path::new("/synth"));
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
    fn pins_reach_the_placer_per_port_bit() {
        let m = manifest(Sources::Verilog(vec![]));
        let layout = Layout {
            dir: "/p/build".into(),
            name: m.name.clone(),
        };
        let plan = plan_build(&m, &layout, Path::new("/synth"));
        let Step::PlaceRoute { pins, .. } = &plan.steps[2] else {
            panic!("{:?} is not place and route", plan.steps[2]);
        };
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
}
