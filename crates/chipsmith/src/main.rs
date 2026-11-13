use std::path::PathBuf;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};

use chipsmith::flow::{self, Flash, Layout, Persistence, Via};
use chipsmith::manifest::Manifest;
use chipsmith_cyclone2::pack;
use chipsmith_cyclone2::persist::{EpcsOp, Loader};
use chipsmith_route::chipdb::ChipDb;
use chipsmith_xc7::xray;

#[derive(Parser)]
#[command(version, about)]
struct Cli {
    /// Directory holding chipsmith.toml.
    #[arg(long, short = 'C', default_value = ".", global = true)]
    project_dir: PathBuf,
    #[command(subcommand)]
    command: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// HDL to bitstream: build/<name>.rbf (Cyclone II), .bit (7-series) or
    /// .core.rbf (Agilex 5).
    Build,
    /// Load a bitstream into the FPGA over JTAG (volatile; gone on power
    /// cycle), or with --persist write it into the board's configuration
    /// flash.
    Flash {
        /// An .rbf, .svf or .bit, loaded as it is. Without one, the
        /// project is built first (`chipsmith build`) and its bitstream
        /// loaded.
        bitstream: Option<PathBuf>,
        /// Write the configuration flash (DE2: the EPCS16), so the design
        /// loads at power-up. The DE2's RUN/PROG switch stays in RUN.
        #[arg(long)]
        persist: bool,
        /// Load from Linux on the board instead of over JTAG (a Zynq's PL,
        /// the DE25-Nano's core): ssh to HOST and write the bitstream to
        /// nixos-fpga's /dev/fpga-load.
        #[arg(long, value_name = "HOST")]
        ssh: Option<String>,
    },
    /// Save the DE2's whole EPCS16 to a file (read twice and compared).
    #[command(hide = true)]
    FlashDump { out: PathBuf },
    /// Write a `flash-dump` file back into the DE2's EPCS16, verbatim.
    #[command(hide = true)]
    FlashRestore { dump: PathBuf },
    /// Prebuild the routing graph from a chip database (the nix package does).
    #[command(hide = true)]
    RoutingGraph { chipdb: PathBuf, out: PathBuf },
    /// Pack a yosys netlist into what the placer reads (`build` does this).
    #[command(hide = true)]
    Pack { netlist: PathBuf, packed: PathBuf },
    /// Compare two packed netlists (ours against nextpnr's `--pack-only`).
    #[command(hide = true)]
    PackDiff { ours: PathBuf, theirs: PathBuf },
    /// Prebuild the 7-series routing graph from a prjxray-db family
    /// directory (e.g. `.../zynq7`); the nix package does.
    #[command(hide = true)]
    Xc7RoutingGraph { family: PathBuf, out: PathBuf },
    /// Map a yosys netlist's simple gates to 4-input LUTs (in place of abc).
    #[command(hide = true)]
    Lutmap { netlist: PathBuf, mapped: PathBuf },
    /// DE25-Nano phase 1 with another U-Boot SPL: BASE is a Quartus
    /// HPS-first `.hps.rbf` for the board's HPS settings, SPL the
    /// `u-boot-spl-dtb.bin` to put in it.
    Agilex5Phase1 {
        base: PathBuf,
        spl: PathBuf,
        /// Write the phase 1 `.hps.rbf` here (for a JTAG load).
        #[arg(long)]
        rbf: Option<PathBuf>,
        /// Write the QSPI image here, as the `.jic` quartus_pgm programs.
        #[arg(long)]
        jic: Option<PathBuf>,
    },
}

fn build(manifest: &Manifest, layout: &Layout) -> Result<()> {
    eprintln!(
        "building {} for {}",
        manifest.name,
        manifest.target.device()
    );
    flow::plan_build(manifest, layout)?.run()
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    match &cli.command {
        Cmd::RoutingGraph { chipdb, out } => return ChipDb::load(chipdb)?.write_cache(out),
        Cmd::Xc7RoutingGraph { family, out } => {
            let part = chipsmith::device::XC7Z010CLG400_1;
            let started = std::time::Instant::now();
            let (db, s) = xray::import(family, part.die, part.part)?;
            eprintln!("{s:?} in {:.1} s", started.elapsed().as_secs_f64());
            return db.write_cache(out);
        }
        Cmd::Pack { netlist, packed } => {
            let s = pack::pack_file(netlist, packed)?;
            eprintln!("{s:?}");
            return Ok(());
        }
        Cmd::PackDiff { ours, theirs } => {
            let d = pack::diff_files(ours, theirs)?;
            println!("{}", d.report);
            anyhow::ensure!(d.equal, "packed netlists differ");
            return Ok(());
        }
        Cmd::Lutmap { netlist, mapped } => {
            let s = chipsmith_synth::lutmap::map_file(netlist, mapped)?;
            eprintln!("{s:?}");
            return Ok(());
        }
        Cmd::Agilex5Phase1 {
            base,
            spl,
            rbf,
            jic,
        } => {
            anyhow::ensure!(
                rbf.is_some() || jic.is_some(),
                "nothing to write: give --rbf or --jic"
            );
            let read =
                |p: &PathBuf| std::fs::read(p).with_context(|| format!("reading {}", p.display()));
            // Only the key: no routing graph or Quartus files for this.
            let db = std::env::var_os("CHIPSMITH_AGILEX5_DB").map(PathBuf::from).context(
                "CHIPSMITH_AGILEX5_DB is not set; run chipsmith from its nix package or `nix develop`",
            )?;
            let key = read(&db.join("keys/rbf_keyed_hash.key"))?;
            let phase1 = chipsmith_agilex5::rbf::phase1::with_spl(&read(base)?, &read(spl)?, &key)?;
            if let Some(p) = rbf {
                std::fs::write(p, &phase1)?;
            }
            if let Some(p) = jic {
                std::fs::write(p, chipsmith_agilex5::jic::jic(&phase1)?)?;
            }
            return Ok(());
        }
        _ => {}
    }
    let project_dir = std::path::absolute(&cli.project_dir)?;
    // A given Zynq .bit loads over ssh as it is: nothing in it needs a
    // project. A DE25-Nano core does, for the HPS IO hash its load is checked
    // against.
    if let Cmd::Flash {
        bitstream: Some(path),
        persist,
        ssh: Some(host),
    } = &cli.command
    {
        if !project_dir.join(chipsmith::manifest::FILE_NAME).exists() {
            let is_core = std::fs::read(path)
                .map(|f| chipsmith_agilex5::flow::is_rbf(&f))
                .unwrap_or(false);
            anyhow::ensure!(
                !is_core,
                "{}: a DE25-Nano core loads only with its project (-C DIR), whose hps_io_hash it is checked against; another phase 1's core wedges the SDM",
                path.display()
            );
            let persistence = if *persist {
                Persistence::Persistent
            } else {
                Persistence::Volatile
            };
            let flash = Flash::new(path.clone(), persistence, Via::Ssh(host.clone()))?;
            return flow::plan_flash(&flash, &Loader::from_env(&project_dir), &project_dir).run();
        }
    }
    let manifest = Manifest::load(&project_dir)?;
    let layout = Layout::new(&project_dir, &manifest);
    match cli.command {
        Cmd::Build => {
            build(&manifest, &layout)?;
            println!("{}", layout.bitstream().display());
        }
        Cmd::RoutingGraph { .. }
        | Cmd::Xc7RoutingGraph { .. }
        | Cmd::Pack { .. }
        | Cmd::PackDiff { .. }
        | Cmd::Lutmap { .. }
        | Cmd::Agilex5Phase1 { .. } => {
            unreachable!("handled before the manifest")
        }
        Cmd::FlashDump { out } => {
            let out = std::path::absolute(out)?;
            flow::plan_epcs(
                &Loader::from_env(&layout.dir),
                &layout.dir,
                EpcsOp::Dump(out),
            )
            .run()?;
        }
        Cmd::FlashRestore { dump } => {
            let dump = std::path::absolute(dump)?;
            flow::plan_epcs(
                &Loader::from_env(&layout.dir),
                &layout.dir,
                EpcsOp::Restore(dump),
            )
            .run()?;
        }
        Cmd::Flash {
            bitstream,
            persist,
            ssh,
        } => {
            let path = match bitstream {
                Some(path) => path,
                // Built first, so what's loaded is what the sources say
                // now: an edit flashed without a build would load the
                // last one. Unchanged steps are skipped (ADR-0011).
                None => {
                    build(&manifest, &layout)?;
                    layout.bitstream()
                }
            };
            let persistence = if persist {
                Persistence::Persistent
            } else {
                Persistence::Volatile
            };
            let via = match ssh {
                Some(host) => Via::Ssh(host),
                None => Via::Jtag,
            };
            let flash = match &manifest.target {
                chipsmith::manifest::Target::A5eb013bb23be4scs { hps, .. } => {
                    Flash::agilex5(path, persistence, via, hps)?
                }
                _ => Flash::new(path, persistence, via)?,
            };
            flow::plan_flash(&flash, &Loader::from_env(&layout.dir), &layout.dir).run()?;
        }
    }
    Ok(())
}
