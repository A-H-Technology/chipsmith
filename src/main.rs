use std::path::PathBuf;

use anyhow::Result;
use clap::{Parser, Subcommand};

use chipsmith::flow::{self, Layout};
use chipsmith::manifest::Manifest;

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
    /// HDL to bitstream: build/<name>.rbf.
    Build,
    /// Load a bitstream into the FPGA over JTAG (volatile; gone on power cycle).
    Flash {
        /// Defaults to the one `chipsmith build` produced.
        bitstream: Option<PathBuf>,
    },
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let project_dir = std::path::absolute(&cli.project_dir)?;
    let manifest = Manifest::load(&project_dir)?;
    let layout = Layout::new(&project_dir, &manifest);
    match cli.command {
        Cmd::Build => {
            eprintln!("building {} for {}", manifest.name, manifest.device);
            flow::plan_build(&manifest, &layout, &flow::synth_dir()?).run()?;
            println!("{}", layout.bitstream().display());
        }
        Cmd::Flash { bitstream } => {
            let bitstream = bitstream.unwrap_or_else(|| layout.bitstream());
            let jtag_stream = bitstream.with_extension("svf");
            flow::plan_flash(&bitstream, &jtag_stream).run()?;
        }
    }
    Ok(())
}
