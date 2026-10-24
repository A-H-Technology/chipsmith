use std::path::PathBuf;

use anyhow::Result;
use clap::{Parser, Subcommand};

use chipsmith::flow::{self, Flash, Layout, Persistence};
use chipsmith::manifest::Manifest;
use chipsmith::pack;
use chipsmith::persist::{EpcsOp, Loader};
use chipsmith::route::chipdb::ChipDb;
use chipsmith::route::xray;

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
    /// HDL to bitstream: build/<name>.rbf (Cyclone II) or .bit (7-series).
    Build,
    /// Load a bitstream into the FPGA over JTAG (volatile; gone on power
    /// cycle), or with --persist write it into the board's configuration
    /// flash.
    Flash {
        /// An .rbf, .svf or .bit; defaults to what `chipsmith build` produced.
        bitstream: Option<PathBuf>,
        /// Write the configuration flash (DE2: the EPCS16), so the design
        /// loads at power-up. The DE2's RUN/PROG switch stays in RUN.
        #[arg(long)]
        persist: bool,
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
    /// Route nextpnr's routed xc7 design (`--write`) again with chipsmith's
    /// router: its `-o prerouted` and `-o preplaced` lines.
    #[command(hide = true)]
    Xc7Route {
        routed: PathBuf,
        prerouted: PathBuf,
        preplaced: PathBuf,
    },
    /// Map a yosys netlist's simple gates to 4-input LUTs (in place of abc).
    #[command(hide = true)]
    Lutmap { netlist: PathBuf, mapped: PathBuf },
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
        Cmd::Xc7Route {
            routed,
            prerouted,
            preplaced,
        } => {
            let started = std::time::Instant::now();
            let db = xray::load(chipsmith::device::XC7Z010CLG400_1)?;
            eprintln!("routing graph in {:.1} s", started.elapsed().as_secs_f64());
            let s = chipsmith::route::xc7::route_file(&db, routed, prerouted, preplaced)?;
            eprintln!("{s:#?}");
            return Ok(());
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
            let s = chipsmith::synth::lutmap::map_file(netlist, mapped)?;
            eprintln!("{s:?}");
            return Ok(());
        }
        _ => {}
    }
    let project_dir = std::path::absolute(&cli.project_dir)?;
    let manifest = Manifest::load(&project_dir)?;
    let layout = Layout::new(&project_dir, &manifest);
    match cli.command {
        Cmd::Build => {
            eprintln!(
                "building {} for {}",
                manifest.name,
                manifest.target.device()
            );
            flow::plan_build(&manifest, &layout)?.run()?;
            println!("{}", layout.bitstream().display());
        }
        Cmd::RoutingGraph { .. }
        | Cmd::Xc7RoutingGraph { .. }
        | Cmd::Xc7Route { .. }
        | Cmd::Pack { .. }
        | Cmd::PackDiff { .. }
        | Cmd::Lutmap { .. } => {
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
        Cmd::Flash { bitstream, persist } => {
            let path = match bitstream {
                Some(path) => path,
                None => {
                    let built = layout.bitstream();
                    anyhow::ensure!(
                        built.exists(),
                        "{} doesn't exist yet; run `chipsmith build` first",
                        built.display()
                    );
                    built
                }
            };
            let persistence = if persist {
                Persistence::Persistent
            } else {
                Persistence::Volatile
            };
            let flash = Flash::new(path, persistence)?;
            flow::plan_flash(&flash, &Loader::from_env(&layout.dir), &layout.dir).run()?;
        }
    }
    Ok(())
}
