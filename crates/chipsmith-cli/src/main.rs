use std::path::PathBuf;
use std::process::ExitCode;

use chipsmith_core::error::ChipsmithError;
use chipsmith_core::scaffold::{self, InitOptions};
use chipsmith_core::toolchain::{BuildOutcome, Toolchain};
use chipsmith_core::{resolve_backend, DEFAULT_BACKEND};
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
        #[facet(args::named, default = DEFAULT_BACKEND.to_string())]
        backend: String,

        /// Toolchain version (default: the backend's latest)
        #[facet(args::named)]
        version: Option<String>,

        /// Target FPGA family
        #[facet(args::named, default = "Cyclone V".to_string())]
        family: String,

        /// Target FPGA device
        #[facet(args::named, default = "5CSEBA6U23I7".to_string())]
        device: String,
    },

    /// Download and install a toolchain
    Install {
        /// Version to install (default: the backend's latest)
        #[facet(args::positional)]
        version: Option<String>,

        /// Toolchain backend (quartus-prime or quartus-ii-13)
        #[facet(args::named, default = DEFAULT_BACKEND.to_string())]
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

        /// Version to use (default: the backend's latest)
        #[facet(args::named)]
        version: Option<String>,

        /// Toolchain backend (quartus-prime or quartus-ii-13)
        #[facet(args::named, default = DEFAULT_BACKEND.to_string())]
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

        /// Path to .sof file (default: the last build's output)
        #[facet(args::named)]
        sof: Option<PathBuf>,

        /// JTAG cable name (e.g. USB-Blaster)
        #[facet(args::named)]
        cable: Option<String>,
    },

    /// List connected JTAG cables and devices
    Cables {
        /// Toolchain backend (quartus-prime or quartus-ii-13)
        #[facet(args::named, default = DEFAULT_BACKEND.to_string())]
        backend: String,

        /// Toolchain version (default: the backend's latest)
        #[facet(args::named)]
        version: Option<String>,
    },

    /// Show the install path for a toolchain version
    Which {
        /// Version (default: the backend's latest)
        #[facet(args::positional)]
        version: Option<String>,

        /// Toolchain backend (quartus-prime or quartus-ii-13)
        #[facet(args::named, default = DEFAULT_BACKEND.to_string())]
        backend: String,
    },
}

/// Resolve a Backend and the Version to use with it. The default Version is
/// the Backend's own, which is why it cannot be an argument-parser default:
/// nothing knows it until the Backend is picked.
fn backend_and_version(
    backend: &str,
    version: Option<String>,
) -> Result<(Box<dyn Toolchain>, String), ChipsmithError> {
    let toolchain = resolve_backend(backend)?;
    let version = version.unwrap_or_else(|| toolchain.default_version().to_string());
    Ok((toolchain, version))
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
        } => backend_and_version(&backend, version).and_then(|(_, version)| {
            scaffold::init(
                &project_dir,
                InitOptions {
                    name,
                    backend,
                    version,
                    family,
                    device,
                },
            )
        }),

        Commands::Install {
            version,
            backend,
            installer,
        } => match backend_and_version(&backend, version) {
            Ok((toolchain, version)) => match installer {
                Some(path) => toolchain.install_from_local(&path, &version).await,
                None => toolchain.ensure_installed(&version).await.map(|_| ()),
            },
            Err(e) => Err(e),
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
        } => match backend_and_version(&backend, version) {
            Ok((toolchain, version)) => toolchain.run_tool(&version, &tool, &args, None).await,
            Err(e) => Err(e),
        },

        Commands::Flash {
            project_dir,
            sof,
            cable,
        } => chipsmith_core::flash(&project_dir, sof.as_deref(), cable.as_deref()).await,

        Commands::Cables { backend, version } => match backend_and_version(&backend, version) {
            Ok((toolchain, version)) => toolchain.run_tool(&version, "jtagconfig", &[], None).await,
            Err(e) => Err(e),
        },

        Commands::Which { version, backend } => match backend_and_version(&backend, version) {
            Ok((toolchain, version)) => match toolchain.install_dir(&version) {
                Ok(dir) => match toolchain.is_installed(&version) {
                    Ok(true) => {
                        println!("{}", dir.display());
                        Ok(())
                    }
                    Ok(false) => {
                        eprintln!("Not installed (would install to {})", dir.display());
                        return ExitCode::FAILURE;
                    }
                    Err(e) => Err(e),
                },
                Err(e) => Err(e),
            },
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
