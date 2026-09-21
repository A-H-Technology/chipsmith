//! Scaffolding a new Project.
//!
//! Lives next to `Manifest` deliberately: this module writes a
//! `chipsmith.toml` as a format string, and nothing but proximity and the
//! round-trip test below stops it drifting out of sync with the struct that
//! has to parse it back.

use std::path::Path;

use crate::error::ChipsmithError;
#[cfg(test)]
use crate::manifest::Manifest;

pub struct InitOptions {
    pub name: Option<String>,
    pub backend: String,
    pub version: String,
    pub family: String,
    pub device: String,
}

/// A Project name that is also a legal VHDL identifier.
fn vhdl_identifier(name: &str) -> String {
    let sanitised = name.replace('-', "_");
    // VHDL identifiers can't start with a digit, so a directory called `7seg`
    // would otherwise scaffold an entity that cannot compile.
    if sanitised.starts_with(|c: char| c.is_ascii_digit()) {
        format!("p_{sanitised}")
    } else {
        sanitised
    }
}

/// The Manifest text for a new Project.
pub fn manifest_template(name: &str, opts: &InitOptions) -> String {
    format!(
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
    )
}

fn entity_template(name: &str) -> String {
    format!(
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
    )
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
    let name = opts.name.clone().unwrap_or_else(|| {
        abs_dir
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("project")
            .to_string()
    });
    let name = vhdl_identifier(&name);

    std::fs::create_dir_all(project_dir).map_err(ChipsmithError::file("create", project_dir))?;
    std::fs::write(&manifest_path, manifest_template(&name, &opts))
        .map_err(ChipsmithError::file("write", &manifest_path))?;

    let src_dir = project_dir.join("src");
    std::fs::create_dir_all(&src_dir).map_err(ChipsmithError::file("create", &src_dir))?;

    let vhdl_path = src_dir.join(format!("{name}.vhd"));
    if !vhdl_path.exists() {
        std::fs::write(&vhdl_path, entity_template(&name))
            .map_err(ChipsmithError::file("write", &vhdl_path))?;
    }

    // Quartus dumps ~16MB of databases and reports into build/ on every compile
    let gitignore_path = project_dir.join(".gitignore");
    if !gitignore_path.exists() {
        std::fs::write(&gitignore_path, "build/\n")
            .map_err(ChipsmithError::file("write", &gitignore_path))?;
    }

    eprintln!("Created chipsmith.toml and src/{name}.vhd");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn options() -> InitOptions {
        InitOptions {
            name: None,
            backend: "quartus-prime".to_string(),
            version: "23.1".to_string(),
            family: "Cyclone V".to_string(),
            device: "5CSEBA6U23I7".to_string(),
        }
    }

    /// The template is a format string with no compile-time link to the struct
    /// that reads it back. Add a required field to Manifest and this is the
    /// only thing that notices.
    #[test]
    fn a_scaffolded_manifest_parses_back() {
        let text = manifest_template("blinky", &options());
        let manifest = Manifest::parse(&text, Path::new("chipsmith.toml")).unwrap();

        assert_eq!(manifest.project.name, "blinky");
        assert_eq!(manifest.project.top, "blinky");
        assert_eq!(manifest.toolchain.backend(), "quartus-prime");
        assert_eq!(manifest.toolchain.version(), "23.1");
        assert_eq!(manifest.target.family, "Cyclone V");
        assert!(manifest.clocks.is_empty());
    }

    #[test]
    fn the_scaffolded_entity_is_named_after_the_project() {
        let vhdl = entity_template("blinky");
        assert!(vhdl.contains("entity blinky is"));
        assert!(vhdl.contains("architecture rtl of blinky is"));
    }

    #[test]
    fn hyphens_become_underscores() {
        assert_eq!(vhdl_identifier("seven-seg"), "seven_seg");
    }

    /// A directory called `7seg` used to scaffold `entity 7seg is`, which VHDL
    /// rejects outright.
    #[test]
    fn a_name_starting_with_a_digit_gets_a_prefix() {
        assert_eq!(vhdl_identifier("7seg"), "p_7seg");
        assert!(entity_template(&vhdl_identifier("7seg")).contains("entity p_7seg is"));
    }
}
