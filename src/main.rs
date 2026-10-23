use std::path::PathBuf;

use anyhow::Result;
use clap::{Parser, Subcommand};

use chipsmith::flow::{self, Layout, Loadable};
use chipsmith::manifest::Manifest;
use chipsmith::pack;
use chipsmith::route::chipdb::ChipDb;

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
    /// Load a bitstream into the FPGA over JTAG (volatile; gone on power cycle).
    Flash {
        /// An .rbf, .svf or .bit; defaults to what `chipsmith build` produced.
        bitstream: Option<PathBuf>,
    },
    /// Prebuild the routing graph from a chip database (the nix package does).
    #[command(hide = true)]
    RoutingGraph { chipdb: PathBuf, out: PathBuf },
    /// Pack a yosys netlist into what the placer reads (`build` does this).
    #[command(hide = true)]
    Pack { netlist: PathBuf, packed: PathBuf },
    /// Compare two packed netlists (ours against nextpnr's `--pack-only`).
    #[command(hide = true)]
    PackDiff { ours: PathBuf, theirs: PathBuf },
    /// Map a yosys netlist's simple gates to 4-input LUTs (in place of abc).
    #[command(hide = true)]
    Lutmap { netlist: PathBuf, mapped: PathBuf },
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    match &cli.command {
        Cmd::RoutingGraph { chipdb, out } => return ChipDb::load(chipdb)?.write_cache(out),
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
            let mut json: serde_json::Value =
                serde_json::from_str(&std::fs::read_to_string(netlist)?)?;
            let s = chipsmith::synth::lutmap::map(&mut json)?;
            std::fs::write(mapped, serde_json::to_string(&json)?)?;
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
        Cmd::RoutingGraph { .. } | Cmd::Pack { .. } | Cmd::PackDiff { .. } | Cmd::Lutmap { .. } => {
            unreachable!("handled before the manifest")
        }
        Cmd::Flash { bitstream } => {
            let loadable = Loadable::from_path(bitstream.unwrap_or_else(|| layout.bitstream()))?;
            flow::plan_flash(&loadable).run()?;
        }
    }
    Ok(())
}
