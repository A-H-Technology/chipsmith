//! End to end on the DE2, step by step: a project's HDL through
//! `chipsmith_verilog` and `chipsmith_cyclone2::synth`, then the packer,
//! placer, router and bitstream assembler, to an .rbf: the pipeline
//! `chipsmith build` runs, with each stage's numbers on one line. With
//! `--yosys` the netlist comes from the yosys flow the build ran before
//! chipsmith's synthesis replaced it (cyclone2-re's `cyclone2.ys` with
//! chipsmith's LUT mapper; the same back end after it), for comparing the
//! two; with `--yosys-rtl` yosys elaborates (`proc; flatten`) and chipsmith
//! synthesizes.
//!
//! `de2_synth [--yosys | --yosys-rtl] <project dir> <scratch dir>`: the
//! scratch dir gets the netlists, `routed.json`, the .rbf and, with a
//! `[clocks]` entries, `top.sdc` (for `bench/quartus-timing.sh`). Needs
//! the devshell for the comparisons (yosys, `CHIPSMITH_CYCLONE2_SYNTH`) and
//! for VHDL (GHDL).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Instant;

use anyhow::{bail, ensure, Context, Result};
use chipsmith::manifest::{Manifest, Sources, Target};
use chipsmith_cyclone2::synth::cells;
use chipsmith_cyclone2::{bitstream, pack, place, route, synth};

#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    /// chipsmith's front end and synthesis.
    Chipsmith,
    /// yosys elaborates, chipsmith synthesizes.
    YosysRtl,
    /// The yosys flow the build runs today.
    Yosys,
}

fn run(cmd: &mut Command) -> Result<()> {
    let out = cmd.output().with_context(|| format!("running {cmd:?}"))?;
    ensure!(
        out.status.success(),
        "{cmd:?} failed:\n{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    Ok(())
}

fn yosys(dir: &Path, cwd: &Path, name: &str, script: &str) -> Result<()> {
    let ys = dir.join(format!("{name}.ys"));
    std::fs::write(&ys, script)?;
    run(Command::new("yosys")
        .arg("-q")
        .arg("-l")
        .arg(dir.join(format!("{name}.log")))
        .arg("-s")
        .arg(&ys)
        .current_dir(cwd))
}

fn synth_dir() -> Result<PathBuf> {
    Ok(PathBuf::from(
        std::env::var_os("CHIPSMITH_CYCLONE2_SYNTH").context("run in `nix develop`")?,
    ))
}

/// The design as Verilog files (VHDL through GHDL).
fn verilog(m: &Manifest, dir: &Path) -> Result<Vec<PathBuf>> {
    Ok(match &m.sources {
        Sources::Verilog(files) => files.clone(),
        Sources::Vhdl(files) => {
            let out = dir.join("translated.v");
            let o = Command::new("ghdl")
                .args(["--synth", "--std=08", "--out=verilog"])
                .args(files)
                .args(["-e", &m.top])
                .current_dir(dir)
                .output()?;
            ensure!(
                o.status.success(),
                "ghdl: {}",
                String::from_utf8_lossy(&o.stderr)
            );
            std::fs::write(&out, o.stdout)?;
            vec![out]
        }
    })
}

fn quoted(files: &[PathBuf]) -> String {
    files
        .iter()
        .map(|f| format!("\"{}\"", f.display()))
        .collect::<Vec<_>>()
        .join(" ")
}

fn main() -> Result<()> {
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    let mode = match args.first().map(String::as_str) {
        Some("--yosys") => Mode::Yosys,
        Some("--yosys-rtl") => Mode::YosysRtl,
        _ => Mode::Chipsmith,
    };
    if mode != Mode::Chipsmith {
        args.remove(0);
    }
    let [project, scratch] = &args[..] else {
        bail!("usage: de2_synth [--yosys] <project dir> <scratch dir>");
    };
    let project = std::fs::canonicalize(project)?;
    let dir = PathBuf::from(scratch);
    std::fs::create_dir_all(&dir)?;
    let dir = std::fs::canonicalize(dir)?;
    let manifest = Manifest::load(&project)?;
    let Target::Ep2c35f672c6 {
        pins,
        clocks,
        unused_pins,
    } = &manifest.target
    else {
        bail!("not a DE2 project");
    };
    let files = verilog(&manifest, &dir)?;
    let rename = if manifest.top == "top" {
        String::new()
    } else {
        format!("rename {} top\n", manifest.top)
    };
    let netlist = dir.join("netlist.json");
    let t = Instant::now();
    // A netlist given as it is (placement and routing of a variant).
    if let Some(given) = std::env::var_os("DE2_NETLIST") {
        std::fs::copy(given, &netlist)?;
    } else if mode == Mode::Yosys {
        let script = std::fs::read_to_string(synth_dir()?.join("synth/cyclone2.ys"))?;
        let lines: Vec<&str> = script.lines().collect();
        let cut = lines
            .iter()
            .position(|l| l.trim() == "abc -lut 4")
            .context("cyclone2.ys without `abc -lut 4`")?;
        let (gates, luts) = (dir.join("gates.json"), dir.join("luts.json"));
        yosys(
            &dir,
            &synth_dir()?,
            "to_gates",
            &format!(
                "read_verilog {}\n{rename}{}\nwrite_json {}\n",
                quoted(&files),
                lines[..cut].join("\n"),
                gates.display()
            ),
        )?;
        chipsmith_synth::lutmap::map_file(&gates, &luts)?;
        yosys(
            &dir,
            &synth_dir()?,
            "to_cells",
            &format!(
                "read_verilog -lib synth/cells_sim.v\nread_json {}\nhierarchy -top top\n{}\nwrite_json {}\n",
                luts.display(),
                lines[cut + 1..].join("\n"),
                netlist.display()
            ),
        )?;
    } else {
        let design = if mode == Mode::YosysRtl {
            let rtl = dir.join("rtl.json");
            let library = dir.join("cells.v");
            std::fs::write(&library, cells::LIBRARY)?;
            yosys(
                &dir,
                &dir,
                "rtl",
                &format!(
                    "read_verilog -lib {}\nread_verilog {}\n{rename}hierarchy -check -top top\nproc\nflatten\nwrite_json {}\n",
                    library.display(),
                    quoted(&files),
                    rtl.display()
                ),
            )?;
            chipsmith_netlist::read_json(&std::fs::read(&rtl)?)?
        } else {
            let mut fe = chipsmith_verilog::Frontend::new();
            fe.read_library_source(cells::LIBRARY_NAME, cells::LIBRARY)?;
            for f in &files {
                fe.read_file(f)?;
            }
            fe.elaborate(&manifest.top)?
        };
        let registers = pins
            .iter()
            .filter(|a| a.io.registers.any())
            .map(|a| (a.port.to_string(), a.io.registers))
            .collect();
        let out = synth::synthesize(design, &registers)?;
        eprintln!("    {:?}", out.stats);
        chipsmith_netlist::write_json(
            &out.design,
            std::io::BufWriter::new(std::fs::File::create(&netlist)?),
        )?;
    }
    let synth_s = t.elapsed().as_secs_f64();

    let packed = dir.join("packed.json");
    let p = pack::pack_file(&netlist, &packed)?;
    let graph = route::load_graph()?;
    let pins: BTreeMap<String, place::PinPlan> = pins
        .iter()
        .map(|a| {
            let plan = place::PinPlan {
                pin: a.pin.as_str().to_string(),
                settings: a.io.settings,
            };
            (a.port.to_string(), plan)
        })
        .collect();
    let placed = dir.join("placed.json");
    let pl = place::place_file(&graph, &packed, &pins, *unused_pins, &placed)?;
    let routed = dir.join("routed.json");
    let r = route::route_file(&graph, &placed, &routed)?;
    let db = bitstream::load_db()?;
    let rbf = dir.join(format!("{}.rbf", manifest.name));
    bitstream::pack_file(&db, &routed, &rbf)?;
    // The manifest's clocks as Quartus' constraints, for bench/quartus-timing.sh.
    let sdc = dir.join("top.sdc");
    if clocks.is_empty() {
        if sdc.exists() {
            std::fs::remove_file(&sdc)?;
        }
    } else {
        let text: String = clocks
            .iter()
            .map(|c| {
                format!(
                    "create_clock -name {{{0}}} -period {1} [get_ports {{{0}}}]\n",
                    c.port,
                    c.frequency.period_ns()
                )
            })
            .collect();
        std::fs::write(&sdc, text)?;
    }
    let fmax = 1e6 / f64::from(r.critical);
    let slack = clocks
        .first()
        .map(|c| c.frequency.period_ns() - f64::from(r.critical) / 1000.0);
    println!(
        "{} ({}): synthesis {:.1} s; {} LUTs, {} registers, {} LEs, {} carry chains, {} M4Ks; critical path {:.2} ns, Fmax {:.1} MHz{}; {}",
        manifest.name,
        match mode {
            Mode::Yosys => "yosys",
            Mode::YosysRtl => "yosys RTL, chipsmith",
            Mode::Chipsmith => "chipsmith",
        },
        synth_s,
        p.luts,
        p.ffs,
        pl.les,
        p.chains,
        p.m4ks,
        f64::from(r.critical) / 1000.0,
        fmax,
        slack.map_or(String::new(), |s| format!(", slack {s:.2} ns")),
        rbf.display()
    );
    Ok(())
}
