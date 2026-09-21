//! Launching Quartus tools and the Quartus installer.
//!
//! One spawn point per operation. The host differences — NixOS or not,
//! sandboxed or patched — are resolved into a `Command` before the spawn
//! rather than forking the control flow around it.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

use tokio::process::Command;

use chipsmith_toolchain::error::ChipsmithError;

use crate::nixos;
use crate::product::QuartusProduct;

pub const KNOWN_TOOLS: &[&str] = &[
    "quartus_sh",
    "quartus_map",
    "quartus_fit",
    "quartus_asm",
    "quartus_sta",
    "quartus_pgm",
    "quartus_cpf",
    "quartus",
    "jtagconfig",
];

pub fn tool_path(install_dir: &Path, tool: &str) -> Result<PathBuf, ChipsmithError> {
    if !KNOWN_TOOLS.contains(&tool) {
        return Err(ChipsmithError::UnknownTool {
            name: tool.to_string(),
            available: KNOWN_TOOLS.iter().map(|t| t.to_string()).collect(),
        });
    }
    Ok(install_dir.join("quartus").join("bin").join(tool))
}

/// Resolve the host compatibility layer, if this host needs one.
async fn host_compat(product: &QuartusProduct) -> Result<Option<nixos::NixCompat>, ChipsmithError> {
    if nixos::is_nixos() {
        Ok(Some(nixos::NixCompat::init(product.spawn).await?))
    } else {
        Ok(None)
    }
}

fn command_for(compat: Option<&nixos::NixCompat>, program: &Path) -> Command {
    let mut cmd = match compat {
        Some(compat) => compat.command_for(program),
        None => Command::new(program),
    };
    if let Some(compat) = compat {
        cmd.env("LD_LIBRARY_PATH", &compat.ld_library_path);
    }
    cmd
}

pub async fn run_tool(
    product: &QuartusProduct,
    install_dir: &Path,
    tool: &str,
    args: &[String],
    working_dir: Option<&Path>,
) -> Result<(), ChipsmithError> {
    let bin = tool_path(install_dir, tool)?;

    if !bin.exists() {
        return Err(ChipsmithError::NotInstalled {
            path: install_dir.to_path_buf(),
        });
    }

    let compat = host_compat(product).await?;
    let mut cmd = command_for(compat.as_ref(), &bin);
    cmd.args(args);
    if let Some(dir) = working_dir {
        cmd.current_dir(dir);
    }

    let status = cmd
        .spawn()
        .map_err(|source| ChipsmithError::Spawn {
            command: bin.display().to_string(),
            source,
        })?
        .wait()
        .await?;

    if !status.success() {
        return Err(ChipsmithError::ProcessFailed {
            command: format!("{} {}", tool, args.join(" ")),
            code: status.code(),
        });
    }

    Ok(())
}

/// The unattended-install arguments for a Product, ending with the target dir.
fn installer_args(product: &QuartusProduct, install_dir: &Path) -> Vec<OsString> {
    let mut args: Vec<OsString> = vec![
        "--mode".into(),
        "unattended".into(),
        "--unattendedmodeui".into(),
        "none".into(),
        "--installdir".into(),
        install_dir.into(),
    ];
    if product.installer.accepts_eula_flag {
        args.push("--accept_eula".into());
        args.push("1".into());
    }
    args
}

pub async fn install_quartus(
    product: &QuartusProduct,
    installer: &Path,
    install_dir: &Path,
) -> Result<(), ChipsmithError> {
    if !installer.exists() {
        return Err(ChipsmithError::InstallerNotFound {
            path: installer.to_path_buf(),
        });
    }

    let compat = host_compat(product).await?;

    // The installer is a vendor binary like any other, except that when the
    // sandbox isn't available we have to rewrite it in place before it can run.
    if let Some(compat) = &compat {
        if compat.bwrap_argv(installer).is_some() {
            eprintln!("Using bubblewrap for 32-bit installer...");
        } else {
            eprintln!("Patching installer for NixOS...");
            compat.patch_elf(installer).await?;
        }
    }

    let status = command_for(compat.as_ref(), installer)
        .args(installer_args(product, install_dir))
        .spawn()
        .map_err(|source| ChipsmithError::Spawn {
            command: installer.display().to_string(),
            source,
        })?
        .wait()
        .await?;

    if !status.success() {
        return Err(ChipsmithError::ProcessFailed {
            command: installer.display().to_string(),
            code: status.code(),
        });
    }

    if let Some(compat) = &compat {
        compat
            .patch_install(&[
                install_dir.join("quartus").join("bin"),
                install_dir.join("quartus").join("linux64"),
                install_dir.join("quartus").join("adm"),
            ])
            .await?;
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::product::{InstallerSpec, QuartusProduct, SpawnStrategy};

    fn product(accepts_eula_flag: bool) -> QuartusProduct {
        QuartusProduct {
            backend: "test",
            display_name: "Test",
            install_root: "test",
            versions: &[],
            latest: "1.0",
            installer: InstallerSpec { accepts_eula_flag },
            spawn: SpawnStrategy::PatchElf,
        }
    }

    fn args_as_strings(args: &[OsString]) -> Vec<String> {
        args.iter()
            .map(|a| a.to_string_lossy().into_owned())
            .collect()
    }

    #[test]
    fn tool_path_lives_under_quartus_bin() {
        let path = tool_path(Path::new("/opt/q"), "quartus_map").unwrap();
        assert_eq!(path, Path::new("/opt/q/quartus/bin/quartus_map"));
    }

    #[test]
    fn an_unknown_tool_is_rejected_with_the_list_of_known_ones() {
        let err = tool_path(Path::new("/opt/q"), "vivado").unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("vivado"), "{msg}");
        assert!(msg.contains("quartus_map"), "{msg}");
    }

    #[test]
    fn installer_runs_unattended_into_the_requested_directory() {
        let args = args_as_strings(&installer_args(&product(true), Path::new("/home/u/q")));
        assert_eq!(args[0], "--mode");
        assert_eq!(args[1], "unattended");
        let installdir = args.iter().position(|a| a == "--installdir").unwrap();
        assert_eq!(args[installdir + 1], "/home/u/q");
    }

    /// Quartus II 13.0sp1 rejects the flag outright rather than ignoring it.
    #[test]
    fn the_eula_flag_is_only_passed_to_products_that_accept_it() {
        let with = args_as_strings(&installer_args(&product(true), Path::new("/q")));
        assert!(with.windows(2).any(|w| w == ["--accept_eula", "1"]));

        let without = args_as_strings(&installer_args(&product(false), Path::new("/q")));
        assert!(!without.iter().any(|a| a == "--accept_eula"));
    }
}
