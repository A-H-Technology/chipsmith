pub use chipsmith_toolchain::{error, manifest, toolchain};

use std::path::{Path, PathBuf};

use error::ChipsmithError;
use manifest::Manifest;
use toolchain::Toolchain;

use chipsmith_quartus_common::build::BuildLayout;
use chipsmith_quartus_common::{QuartusProduct, QuartusToolchain};

/// Every Backend chipsmith knows. Adding one means adding a Product here.
pub static PRODUCTS: &[&QuartusProduct] = &[
    &chipsmith_quartus_prime::PRODUCT,
    &chipsmith_quartus_ii_13::PRODUCT,
];

pub const DEFAULT_LATEST: &str = chipsmith_quartus_prime::LATEST;

fn resolve_backend(name: &str) -> Result<Box<dyn Toolchain>, ChipsmithError> {
    PRODUCTS
        .iter()
        .find(|product| product.backend == name)
        .map(|product| Box::new(QuartusToolchain::new(product)) as Box<dyn Toolchain>)
        .ok_or_else(|| ChipsmithError::UnknownBackend {
            name: name.to_string(),
            available: PRODUCTS.iter().map(|p| p.backend.to_string()).collect(),
        })
}

/// Install a toolchain version. Downloads if needed.
pub async fn install(backend: &str, version: &str) -> Result<PathBuf, ChipsmithError> {
    resolve_backend(backend)?.ensure_installed(version).await
}

/// Install from a local installer file.
pub async fn install_from_local(
    backend: &str,
    installer: &Path,
    version: &str,
) -> Result<(), ChipsmithError> {
    resolve_backend(backend)?
        .install_from_local(installer, version)
        .await
}

/// Build the FPGA project described by the manifest.
pub async fn build(project_dir: &Path) -> Result<PathBuf, ChipsmithError> {
    let manifest = Manifest::load(project_dir)?;
    let backend = resolve_backend(manifest.toolchain.backend())?;
    backend.build(project_dir, &manifest).await
}

/// Run a tool from the given backend.
pub async fn run_tool(
    backend: &str,
    version: &str,
    tool: &str,
    args: &[String],
    working_dir: Option<&Path>,
) -> Result<(), ChipsmithError> {
    resolve_backend(backend)?
        .run_tool(version, tool, args, working_dir)
        .await
}

/// Flash a .sof file to the FPGA. Reads chipsmith.toml to resolve the backend.
/// If no sof path is given, uses the default build output.
pub async fn flash(
    project_dir: &Path,
    sof: Option<&Path>,
    cable: Option<&str>,
) -> Result<(), ChipsmithError> {
    let manifest = Manifest::load(project_dir)?;
    let backend = resolve_backend(manifest.toolchain.backend())?;

    let default_sof = BuildLayout::new(project_dir, &manifest).bitstream();
    let sof_path = sof.unwrap_or(&default_sof);

    backend.flash(sof_path, &manifest, cable).await
}

/// Scaffold a new chipsmith project.
pub fn init(project_dir: &Path, opts: InitOptions) -> Result<(), ChipsmithError> {
    let manifest_path = project_dir.join("chipsmith.toml");
    if manifest_path.exists() {
        return Err(ChipsmithError::ProjectAlreadyExists {
            path: manifest_path,
        });
    }

    let abs_dir = project_dir
        .canonicalize()
        .unwrap_or_else(|_| project_dir.to_path_buf());
    let name = opts.name.unwrap_or_else(|| {
        abs_dir
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("project")
            .to_string()
    });
    // VHDL identifiers can't contain hyphens or start with digits
    let name = name.replace('-', "_");

    let toml = format!(
        r#"[project]
name = "{name}"
top = "{name}"

[toolchain]
{backend} = "{version}"

[target]
family = "{family}"
device = "{device}"

[hdl]
sources = ["src/*.vhd"]

[pins]

# Port frequencies, e.g. clk = "50 MHz". Without these the timing report is meaningless.
[clocks]
"#,
        backend = opts.backend,
        version = opts.version,
        family = opts.family,
        device = opts.device,
    );

    std::fs::create_dir_all(project_dir)?;
    std::fs::write(&manifest_path, &toml)?;

    let src_dir = project_dir.join("src");
    std::fs::create_dir_all(&src_dir)?;

    let vhdl_path = src_dir.join(format!("{name}.vhd"));
    if !vhdl_path.exists() {
        let vhdl = format!(
            r#"library ieee;
use ieee.std_logic_1164.all;
use ieee.numeric_std.all;

entity {name} is
    port (
        clk : in std_logic
    );
end entity;

architecture rtl of {name} is
begin
end architecture;
"#,
        );
        std::fs::write(&vhdl_path, &vhdl)?;
    }

    // Quartus dumps ~16MB of databases and reports into build/ on every compile
    let gitignore_path = project_dir.join(".gitignore");
    if !gitignore_path.exists() {
        std::fs::write(&gitignore_path, "build/\n")?;
    }

    eprintln!("Created chipsmith.toml and src/{name}.vhd");
    Ok(())
}

pub struct InitOptions {
    pub name: Option<String>,
    pub backend: String,
    pub version: String,
    pub family: String,
    pub device: String,
}

/// List connected JTAG cables and devices.
pub async fn cables(backend: &str, version: &str) -> Result<(), ChipsmithError> {
    resolve_backend(backend)?
        .run_tool(version, "jtagconfig", &[], None)
        .await
}

/// Get the install directory for a backend + version.
pub fn which(backend: &str, version: &str) -> Result<(PathBuf, bool), ChipsmithError> {
    let b = resolve_backend(backend)?;
    let dir = b.install_dir(version)?;
    let installed = b.is_installed(version)?;
    Ok((dir, installed))
}
