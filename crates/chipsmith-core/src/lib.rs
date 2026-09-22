//! The Backend registry, and the two commands that are driven by a Manifest
//! rather than by flags.
//!
//! Everything else a caller might want is the `Toolchain` interface itself:
//! `resolve_backend` hands you the adapter and you call it directly, rather
//! than through a forwarder that re-does the lookup and re-widens the error
//! space with an `UnknownBackend` you already ruled out.

pub use chipsmith_sim as sim;
pub use chipsmith_toolchain::{error, manifest, process, scaffold, timing, toolchain};

use std::path::Path;

use error::ChipsmithError;
use manifest::{Manifest, SimulatorChoice};
use toolchain::{BuildOutcome, Toolchain};

use chipsmith_quartus_common::build::BuildLayout;
use chipsmith_quartus_common::{QuartusProduct, QuartusSimulator, QuartusToolchain};
use chipsmith_sim::{ghdl::Ghdl, SimPlan, Simulator, TestReport};

/// Every Backend chipsmith knows. Adding one means adding a Product here.
pub static PRODUCTS: &[&QuartusProduct] = &[
    &chipsmith_quartus_prime::PRODUCT,
    &chipsmith_quartus_ii_13::PRODUCT,
];

/// The Backend used when a command doesn't name one.
pub const DEFAULT_BACKEND: &str = "quartus-prime";

pub fn resolve_backend(name: &str) -> Result<Box<dyn Toolchain>, ChipsmithError> {
    PRODUCTS
        .iter()
        .find(|product| product.backend == name)
        .map(|product| Box::new(QuartusToolchain::new(product)) as Box<dyn Toolchain>)
        .ok_or_else(|| ChipsmithError::UnknownBackend {
            name: name.to_string(),
            available: PRODUCTS.iter().map(|p| p.backend.to_string()).collect(),
        })
}

/// Load a Project's Manifest and the Toolchain it names.
pub fn open_project(project_dir: &Path) -> Result<(Manifest, Box<dyn Toolchain>), ChipsmithError> {
    let manifest = Manifest::load(project_dir)?;
    let backend = resolve_backend(manifest.toolchain.backend())?;
    Ok((manifest, backend))
}

/// Build the Project described by the Manifest.
pub async fn build(project_dir: &Path) -> Result<BuildOutcome, ChipsmithError> {
    let (manifest, backend) = open_project(project_dir)?;
    backend.build(project_dir, &manifest).await
}

/// The Simulator a Manifest selects. Cannot fail on the name: `[sim]` is
/// parsed into a `SimulatorChoice`, so an unknown one never gets this far.
///
/// The Quartus Simulator is a component of the Project's own Toolchain rather
/// than a thing of its own, which is why the Manifest picks it by saying
/// "quartus" and never by naming ModelSim or Questa: which one you get is a
/// fact about the Version you build with.
fn resolve_simulator(
    manifest: &Manifest,
    choice: SimulatorChoice,
) -> Result<Box<dyn Simulator>, ChipsmithError> {
    Ok(match choice {
        SimulatorChoice::Ghdl => Box::new(Ghdl::new()),
        SimulatorChoice::Quartus => {
            let product = PRODUCTS
                .iter()
                .find(|product| product.backend == manifest.toolchain.backend())
                .ok_or_else(|| ChipsmithError::UnknownBackend {
                    name: manifest.toolchain.backend().to_string(),
                    available: PRODUCTS.iter().map(|p| p.backend.to_string()).collect(),
                })?;
            Box::new(QuartusSimulator::new(product, manifest.toolchain.version()))
        }
    })
}

/// Run the Project's Testbenches.
///
/// `only` narrows the run to one Testbench and `simulator` overrides the
/// Manifest's choice — the override exists so a machine with no Quartus can
/// still run a Project's tests under GHDL.
pub async fn test(
    project_dir: &Path,
    only: Option<&str>,
    simulator: Option<SimulatorChoice>,
) -> Result<TestReport, ChipsmithError> {
    // Absolute from here down: the Simulator runs with the work library as its
    // working directory, so a relative source path would resolve against the
    // wrong place.
    let project_dir = &project_dir
        .canonicalize()
        .map_err(ChipsmithError::file("open", project_dir))?;

    let manifest = Manifest::load(project_dir)?;
    let sim = manifest
        .sim
        .as_ref()
        .ok_or_else(|| ChipsmithError::NoTestbenches {
            path: project_dir.join("chipsmith.toml"),
        })?;

    let plan = SimPlan::new(project_dir, &manifest, sim, only)?;
    resolve_simulator(&manifest, simulator.unwrap_or(sim.simulator))?
        .test(&plan)
        .await
}

/// Flash a Bitstream to the FPGA. Defaults to the one the last build produced.
pub async fn flash(
    project_dir: &Path,
    bitstream: Option<&Path>,
    cable: Option<&str>,
) -> Result<(), ChipsmithError> {
    let (manifest, backend) = open_project(project_dir)?;

    let default = BuildLayout::new(project_dir, &manifest).bitstream();
    backend
        .flash(
            manifest.toolchain.version(),
            bitstream.unwrap_or(&default),
            cable,
        )
        .await
}
