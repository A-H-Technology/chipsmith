//! Launching Quartus tools and the Quartus installer.
//!
//! One spawn point per operation, all of them through a `ProcessHost`. The
//! host differences — NixOS or not, sandboxed or patched — are resolved into a
//! `ProcessSpec` before the spawn rather than forking the control flow around
//! it.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

use chipsmith_toolchain::error::ChipsmithError;

use crate::nixos::NixCompat;
use crate::product::QuartusProduct;
use chipsmith_toolchain::process::{Capture, ProcessHost, ProcessSpec};

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

/// Resolve the Nix Compat layer, if this host needs one.
pub(crate) async fn host_compat(
    host: &dyn ProcessHost,
    product: &QuartusProduct,
) -> Result<Option<NixCompat>, ChipsmithError> {
    if host.is_nixos() {
        Ok(Some(NixCompat::init(host, product.spawn).await?))
    } else {
        Ok(None)
    }
}

/// Dress a spec for this host: sandboxed if the Spawn Strategy calls for it,
/// and with the resolved library path either way.
pub(crate) fn for_host(spec: ProcessSpec, compat: Option<&NixCompat>) -> ProcessSpec {
    let Some(compat) = compat else {
        return spec;
    };
    let spec = match compat.bwrap_argv(&spec.program) {
        Some(argv) => spec.wrapped_in(argv),
        None => spec,
    };
    spec.env("LD_LIBRARY_PATH", &compat.ld_library_path)
}

/// Run a spec and turn a non-zero exit into an error that says what happened.
async fn run_checked(
    host: &dyn ProcessHost,
    spec: ProcessSpec,
    command: String,
) -> Result<(), ChipsmithError> {
    let outcome = host.run(spec).await?;
    if outcome.success() {
        return Ok(());
    }
    Err(ChipsmithError::ProcessFailed {
        command,
        code: outcome.code,
        stderr_tail: outcome.stderr_tail,
    })
}

pub async fn run_tool(
    host: &dyn ProcessHost,
    product: &QuartusProduct,
    install_dir: &Path,
    tool: &str,
    args: &[String],
    working_dir: Option<&Path>,
) -> Result<(), ChipsmithError> {
    let bin = tool_path(install_dir, tool)?;

    if !host.path_exists(&bin) {
        return Err(ChipsmithError::NotInstalled {
            path: install_dir.to_path_buf(),
        });
    }

    let compat = host_compat(host, product).await?;

    let mut spec = ProcessSpec::new(&bin).args(args.iter().map(String::as_str));
    if let Some(dir) = working_dir {
        spec = spec.current_dir(dir);
    }
    // Tee rather than Piped: a synthesis run takes minutes and the user needs
    // to see it happening, but a failure has to be able to explain itself.
    let spec = for_host(spec.capture(Capture::Tee), compat.as_ref());

    run_checked(host, spec, format!("{} {}", tool, args.join(" "))).await
}

/// The `quartus_pgm` arguments for loading a Bitstream over JTAG. Pure, so
/// the operation grammar — `P;<file>` — gets a test instead of a live cable.
pub fn flash_args(bitstream: &Path, cable: Option<&str>) -> Vec<String> {
    let mut args = vec![
        "-m".to_string(),
        "jtag".to_string(),
        "-o".to_string(),
        format!("P;{}", bitstream.display()),
    ];
    if let Some(cable) = cable {
        args.push("-c".to_string());
        args.push(cable.to_string());
    }
    args
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
    host: &dyn ProcessHost,
    product: &QuartusProduct,
    installer: &Path,
    install_dir: &Path,
) -> Result<(), ChipsmithError> {
    if !host.path_exists(installer) {
        return Err(ChipsmithError::InstallerNotFound {
            path: installer.to_path_buf(),
        });
    }

    let compat = host_compat(host, product).await?;

    // The installer is a vendor binary like any other, except that when the
    // sandbox isn't available we have to rewrite it in place before it can run.
    if let Some(compat) = &compat {
        if compat.bwrap_argv(installer).is_some() {
            eprintln!("Using bubblewrap for 32-bit installer...");
        } else {
            eprintln!("Patching installer for NixOS...");
            compat.patch_elf(host, installer).await?;
        }
    }

    let spec = for_host(
        ProcessSpec::new(installer)
            .args(installer_args(product, install_dir))
            .capture(Capture::Tee),
        compat.as_ref(),
    );
    run_checked(host, spec, installer.display().to_string()).await?;

    if let Some(compat) = &compat {
        compat
            .patch_install(
                host,
                &[
                    install_dir.join("quartus").join("bin"),
                    install_dir.join("quartus").join("linux64"),
                    install_dir.join("quartus").join("adm"),
                    // the bundled simulator is a vendor binary like any other
                    product.simulator.bin_dir(install_dir),
                    install_dir
                        .join(product.simulator.subdir)
                        .join("linuxaloem"),
                    install_dir
                        .join(product.simulator.subdir)
                        .join("linux_x86_64"),
                ],
            )
            .await?;
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::product::{BundledSimulator, InstallerSpec, SpawnStrategy};
    use chipsmith_toolchain::process::fake::RecordingHost;

    fn product(accepts_eula_flag: bool, spawn: SpawnStrategy) -> QuartusProduct {
        QuartusProduct {
            backend: "test",
            display_name: "Test",
            install_root: "test",
            versions: &[],
            latest: "1.0",
            installer: InstallerSpec { accepts_eula_flag },
            spawn,
            simulator: BundledSimulator {
                subdir: "modelsim_ase",
                display_name: "Test Simulator",
                needs_license: false,
            },
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
    fn flashing_programs_the_bitstream_over_jtag() {
        let args = flash_args(Path::new("/proj/build/output_files/blinky.sof"), None);
        assert_eq!(
            args,
            vec!["-m", "jtag", "-o", "P;/proj/build/output_files/blinky.sof"]
        );
    }

    #[test]
    fn a_named_cable_is_passed_through() {
        let args = flash_args(Path::new("/b.sof"), Some("USB-Blaster"));
        assert!(args.windows(2).any(|w| w == ["-c", "USB-Blaster"]));
    }

    #[test]
    fn installer_runs_unattended_into_the_requested_directory() {
        let args = args_as_strings(&installer_args(
            &product(true, SpawnStrategy::PatchElf),
            Path::new("/home/u/q"),
        ));
        assert_eq!(args[0], "--mode");
        assert_eq!(args[1], "unattended");
        let installdir = args.iter().position(|a| a == "--installdir").unwrap();
        assert_eq!(args[installdir + 1], "/home/u/q");
    }

    /// Quartus II 13.0sp1 rejects the flag outright rather than ignoring it.
    #[test]
    fn the_eula_flag_is_only_passed_to_products_that_accept_it() {
        let with = args_as_strings(&installer_args(
            &product(true, SpawnStrategy::PatchElf),
            Path::new("/q"),
        ));
        assert!(with.windows(2).any(|w| w == ["--accept_eula", "1"]));

        let without = args_as_strings(&installer_args(
            &product(false, SpawnStrategy::PatchElf),
            Path::new("/q"),
        ));
        assert!(!without.iter().any(|a| a == "--accept_eula"));
    }

    #[tokio::test]
    async fn a_tool_that_is_not_installed_says_so_before_spawning_anything() {
        let host = RecordingHost::new().only_these_exist(vec![]);
        let err = run_tool(
            &host,
            &product(true, SpawnStrategy::PatchElf),
            Path::new("/opt/q"),
            "quartus_map",
            &[],
            None,
        )
        .await
        .unwrap_err();

        assert!(matches!(err, ChipsmithError::NotInstalled { .. }), "{err}");
        assert!(host.runs.lock().unwrap().is_empty());
    }

    /// The branch that never executes on the author's NixOS machine.
    #[tokio::test]
    async fn off_nixos_a_tool_runs_directly_with_no_compat_layer() {
        let host = RecordingHost::new();
        run_tool(
            &host,
            &product(true, SpawnStrategy::PatchElf),
            Path::new("/opt/q"),
            "quartus_map",
            &["blinky".to_string()],
            Some(Path::new("/proj/build")),
        )
        .await
        .unwrap();

        let runs = host.runs.lock().unwrap();
        assert_eq!(runs.len(), 1, "no nix eval should have happened");
        assert_eq!(
            runs[0].command_line(),
            "/opt/q/quartus/bin/quartus_map blinky"
        );
        assert_eq!(
            runs[0].working_dir.as_deref(),
            Some(Path::new("/proj/build"))
        );
        assert!(runs[0].env.is_empty());
    }

    /// The branch that never executes on CI.
    #[tokio::test]
    async fn on_nixos_a_32_bit_product_runs_its_tools_inside_the_sandbox() {
        let host = RecordingHost::new()
            .on_nixos()
            .will_reply(RecordingHost::succeeding_with(NIX_EVAL_JSON));

        run_tool(
            &host,
            &product(false, SpawnStrategy::Bubblewrap32),
            Path::new("/opt/q"),
            "quartus_map",
            &["blinky".to_string()],
            None,
        )
        .await
        .unwrap();

        let commands = host.commands();
        assert!(commands[0].starts_with("nix eval"), "{:?}", commands);
        let tool_run = &commands[1];
        assert!(tool_run.starts_with("/nix/bwrap/bin/bwrap"), "{tool_run}");
        assert!(tool_run.contains("--tmpfs /lib"), "{tool_run}");
        assert!(
            tool_run.ends_with("/opt/q/quartus/bin/quartus_map blinky"),
            "{tool_run}"
        );
    }

    #[tokio::test]
    async fn on_nixos_a_64_bit_product_gets_a_library_path_but_no_sandbox() {
        let host = RecordingHost::new()
            .on_nixos()
            .will_reply(RecordingHost::succeeding_with(NIX_EVAL_JSON));

        run_tool(
            &host,
            &product(true, SpawnStrategy::PatchElf),
            Path::new("/opt/q"),
            "quartus_map",
            &[],
            None,
        )
        .await
        .unwrap();

        let runs = host.runs.lock().unwrap();
        assert_eq!(runs[1].program, Path::new("/opt/q/quartus/bin/quartus_map"));
        assert_eq!(runs[1].env[0].0, "LD_LIBRARY_PATH");
    }

    /// A failed synthesis used to yield an exit code and nothing else, because
    /// the tool's output went straight to the terminal.
    #[tokio::test]
    async fn a_failing_tool_carries_out_what_it_said_before_it_died() {
        let host = RecordingHost::new().will_reply(RecordingHost::failing(
            1,
            "Error (10500): VHDL syntax error at blinky.vhd(14)",
        ));

        let err = run_tool(
            &host,
            &product(true, SpawnStrategy::PatchElf),
            Path::new("/opt/q"),
            "quartus_map",
            &["blinky".to_string()],
            None,
        )
        .await
        .unwrap_err();

        let msg = err.to_string();
        assert!(msg.contains("quartus_map blinky"), "{msg}");
        assert!(msg.contains("blinky.vhd(14)"), "{msg}");
    }

    #[tokio::test]
    async fn a_missing_installer_is_reported_before_anything_runs() {
        let host = RecordingHost::new().only_these_exist(vec![]);
        let err = install_quartus(
            &host,
            &product(true, SpawnStrategy::PatchElf),
            Path::new("/tmp/setup.run"),
            Path::new("/home/u/q"),
        )
        .await
        .unwrap_err();

        assert!(
            matches!(err, ChipsmithError::InstallerNotFound { .. }),
            "{err}"
        );
    }

    #[tokio::test]
    async fn off_nixos_the_installer_runs_unpatched() {
        let host = RecordingHost::new();
        install_quartus(
            &host,
            &product(true, SpawnStrategy::PatchElf),
            Path::new("/tmp/setup.run"),
            Path::new("/home/u/q"),
        )
        .await
        .unwrap();

        let commands = host.commands();
        assert_eq!(commands.len(), 1, "{commands:?}");
        assert!(commands[0].starts_with("/tmp/setup.run --mode unattended"));
        assert!(commands[0].ends_with("--installdir /home/u/q --accept_eula 1"));
    }

    /// Quartus Prime never sandboxes, so on NixOS its installer is patched in
    /// place and the whole install tree is patched afterwards.
    #[tokio::test]
    async fn on_nixos_a_64_bit_installer_is_patched_rather_than_sandboxed() {
        let host = RecordingHost::new()
            .on_nixos()
            .will_reply(RecordingHost::succeeding_with(NIX_EVAL_JSON));

        // patch_elf chmods the installer for real, so it has to be a real file
        let installer =
            std::env::temp_dir().join(format!("chipsmith-installer-{}.run", std::process::id()));
        std::fs::write(&installer, "#!/bin/sh\n").unwrap();

        install_quartus(
            &host,
            &product(true, SpawnStrategy::PatchElf),
            &installer,
            Path::new("/home/u/q"),
        )
        .await
        .unwrap();
        std::fs::remove_file(&installer).unwrap();

        let commands = host.commands();
        assert!(commands[0].starts_with("nix eval"));
        assert!(
            commands[1].starts_with("/nix/patchelf/bin/patchelf --set-interpreter"),
            "{:?}",
            commands[1]
        );
        assert!(
            commands[1].ends_with(installer.to_str().unwrap()),
            "{:?}",
            commands[1]
        );
        assert!(
            commands[2].contains("--mode unattended"),
            "{:?}",
            commands[2]
        );
    }

    const NIX_EVAL_JSON: &str = r#"{
        "glibc": "/nix/glibc",
        "glibc32": "/nix/glibc32",
        "patchelf": "/nix/patchelf",
        "bash": "/nix/bash",
        "bubblewrap": "/nix/bwrap"
    }"#;
}
