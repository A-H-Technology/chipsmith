//! The Backend registry, and the two commands that are driven by a Manifest
//! rather than by flags.
//!
//! Everything else a caller might want is the `Toolchain` interface itself:
//! `resolve_backend` hands you the adapter and you call it directly, rather
//! than through a forwarder that re-does the lookup and re-widens the error
//! space with an `UnknownBackend` you already ruled out.

pub use chipsmith_toolchain::{error, manifest, scaffold, timing, toolchain};

use std::path::Path;

use error::ChipsmithError;
use manifest::Manifest;
use toolchain::{BuildOutcome, Toolchain};

use chipsmith_quartus_common::build::BuildLayout;
use chipsmith_quartus_common::{QuartusProduct, QuartusToolchain};

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
