use std::path::PathBuf;
use std::process::ExitCode;

use chipsmith_core::toolchain::BuildOutcome;
use facet::Facet;
use figue::{self as args, FigueBuiltins};

#[derive(Facet)]
struct Cli {
    #[facet(args::subcommand)]
    command: Commands,

    #[facet(flatten)]
    builtins: FigueBuiltins,
}

#[derive(Facet)]
#[repr(u8)]
enum Commands {
    /// Create a new chipsmith project
    Init {
        /// Project directory (default: current dir)
        #[facet(args::named, default = PathBuf::from("."))]
        project_dir: PathBuf,

        /// Project name (default: directory name)
        #[facet(args::named)]
        name: Option<String>,

        /// Toolchain backend
        #[facet(args::named, default = "quartus-prime".to_string())]
        backend: String,

        /// Toolchain version
        #[facet(args::named, default = chipsmith_core::DEFAULT_LATEST.to_string())]
        version: String,

        /// Target FPGA family
        #[facet(args::named, default = "Cyclone V".to_string())]
        family: String,

        /// Target FPGA device
        #[facet(args::named, default = "5CSEBA6U23I7".to_string())]
        device: String,
    },

    /// Download and install a toolchain
    Install {
        /// Version to install (e.g. 23.1, 22.1, 24.1, 13.0sp1)
        #[facet(args::positional, default = chipsmith_core::DEFAULT_LATEST.to_string())]
        version: String,

        /// Toolchain backend (quartus-prime or quartus-ii-13)
        #[facet(args::named, default = "quartus-prime".to_string())]
        backend: String,

        /// Path to a local installer (skips download)
        #[facet(args::named)]
        installer: Option<PathBuf>,
    },

    /// Build the FPGA project in the current (or given) directory
    Build {
        /// Project directory containing chipsmith.toml (default: current dir)
        #[facet(args::named, default = PathBuf::from("."))]
        project_dir: PathBuf,

        /// Exit non-zero if the design does not meet timing
        #[facet(args::named, default = false)]
        require_timing: bool,
    },

    /// Run a toolchain tool directly
    Run {
        /// Tool name (e.g. quartus_sh, quartus_map)
        #[facet(args::positional)]
        tool: String,

        /// Version to use (default: latest)
        #[facet(args::named, default = chipsmith_core::DEFAULT_LATEST.to_string())]
        version: String,

        /// Toolchain backend (quartus-prime or quartus-ii-13)
        #[facet(args::named, default = "quartus-prime".to_string())]
        backend: String,

        /// Arguments passed to the tool
        #[facet(args::positional)]
        args: Vec<String>,
    },

    /// Flash a .sof file to the FPGA via JTAG
    Flash {
        /// Project directory containing chipsmith.toml (default: current dir)
        #[facet(args::named, default = PathBuf::from("."))]
        project_dir: PathBuf,

        /// Path to .sof file (default: auto-detect from build output)
        #[facet(args::named)]
        sof: Option<PathBuf>,

        /// JTAG cable name (e.g. USB-Blaster)
        #[facet(args::named)]
        cable: Option<String>,
    },

    /// List connected JTAG cables and devices
    Cables {
        /// Toolchain backend (quartus-prime or quartus-ii-13)
        #[facet(args::named, default = "quartus-prime".to_string())]
        backend: String,

        /// Toolchain version
        #[facet(args::named, default = chipsmith_core::DEFAULT_LATEST.to_string())]
        version: String,
    },

    /// Show the install path for a toolchain version
    Which {
        /// Version (default: latest)
        #[facet(args::positional, default = chipsmith_core::DEFAULT_LATEST.to_string())]
        version: String,

        /// Toolchain backend (quartus-prime or quartus-ii-13)
        #[facet(args::named, default = "quartus-prime".to_string())]
        backend: String,
    },
}

/// A design that misses timing still produces a loadable Bitstream, so this
/// warns by default — but it says so loudly, because "Build complete" on a
/// design that missed setup by 2ns is exactly the trap this is here to close.
/// `--require-timing` turns the warning into a failure.
fn report_build(outcome: &BuildOutcome) {
    eprintln!("Build complete: {}", outcome.bitstream.display());

    match outcome.timing.as_ref().and_then(|t| t.worst()) {
        Some(worst) if outcome.meets_timing() => {
            eprintln!(
                "Timing met — worst slack {:.3} ns ({})",
                worst.slack_ns, worst.kind
            );
        }
        Some(worst) => {
            eprintln!(
                "WARNING: timing NOT met — worst slack {:.3} ns ({})",
                worst.slack_ns, worst.kind
            );
        }
        None => eprintln!("WARNING: timing analysis produced no summary"),
    }
}

#[tokio::main]
async fn main() -> ExitCode {
    let cli: Cli = figue::from_std_args().unwrap();

    let result = match cli.command {
        Commands::Init {
            project_dir,
            name,
            backend,
            version,
            family,
            device,
        } => chipsmith_core::init(
            &project_dir,
            chipsmith_core::InitOptions {
                name,
                backend,
                version,
                family,
                device,
            },
        ),

        Commands::Install {
            version,
            backend,
            installer,
        } => match installer {
            Some(path) => chipsmith_core::install_from_local(&backend, &path, &version).await,
            None => chipsmith_core::install(&backend, &version)
                .await
                .map(|_| ()),
        },

        Commands::Build {
            project_dir,
            require_timing,
        } => match chipsmith_core::build(&project_dir).await {
            Ok(outcome) => {
                report_build(&outcome);
                if require_timing && !outcome.meets_timing() {
                    return ExitCode::FAILURE;
                }
                Ok(())
            }
            Err(e) => Err(e),
        },

        Commands::Run {
            tool,
            version,
            backend,
            args,
        } => chipsmith_core::run_tool(&backend, &version, &tool, &args, None).await,

        Commands::Flash {
            project_dir,
            sof,
            cable,
        } => chipsmith_core::flash(&project_dir, sof.as_deref(), cable.as_deref()).await,

        Commands::Cables { backend, version } => chipsmith_core::cables(&backend, &version).await,

        Commands::Which { version, backend } => match chipsmith_core::which(&backend, &version) {
            Ok((dir, true)) => {
                println!("{}", dir.display());
                Ok(())
            }
            Ok((dir, false)) => {
                eprintln!("Not installed (would install to {})", dir.display());
                return ExitCode::FAILURE;
            }
            Err(e) => Err(e),
        },
    };

    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::FAILURE
        }
    }
}
