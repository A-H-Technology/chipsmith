use std::path::{Path, PathBuf};

use chipsmith_toolchain::error::ChipsmithError;
use chipsmith_toolchain::manifest::Manifest;

use crate::{qsf, sdc};

pub struct BuildStep {
    pub label: &'static str,
    pub tool: &'static str,
    pub args: Vec<String>,
}

pub struct BuildPlan {
    pub build_dir: PathBuf,
    pub steps: Vec<BuildStep>,
    pub output_sof: PathBuf,
}

/// Prepare the build directory (QSF/QPF files, step list) without running anything.
/// Each backend calls this, then runs the steps through its own runner.
pub fn prepare_build(project_dir: &Path, manifest: &Manifest) -> Result<BuildPlan, ChipsmithError> {
    let sources = manifest.resolve_sources(project_dir)?;

    let build_dir = project_dir.join("build");
    std::fs::create_dir_all(&build_dir)?;

    let qsf_content = qsf::generate_qsf(manifest, &sources, project_dir)?;
    std::fs::write(
        build_dir.join(format!("{}.qsf", manifest.project.name)),
        &qsf_content,
    )?;

    let qpf_content = qsf::generate_qpf(manifest);
    std::fs::write(
        build_dir.join(format!("{}.qpf", manifest.project.name)),
        &qpf_content,
    )?;

    let clocks = manifest
        .resolve_clocks()
        .map_err(|message| ChipsmithError::ManifestParse {
            path: project_dir.join("chipsmith.toml"),
            message,
        })?;
    if !clocks.is_empty() {
        std::fs::write(
            build_dir.join(format!("{}.sdc", manifest.project.name)),
            sdc::generate_sdc(&clocks),
        )?;
    }

    let name = &manifest.project.name;
    let steps = vec![
        BuildStep {
            label: "Synthesis",
            tool: "quartus_map",
            args: vec![name.clone(), "--read_settings_files=on".to_string()],
        },
        BuildStep {
            label: "Fitter",
            tool: "quartus_fit",
            args: vec![name.clone(), "--read_settings_files=on".to_string()],
        },
        BuildStep {
            label: "Assembler",
            tool: "quartus_asm",
            args: vec![name.clone(), "--read_settings_files=on".to_string()],
        },
        BuildStep {
            label: "Timing analysis",
            tool: "quartus_sta",
            args: vec![name.clone()],
        },
    ];

    let output_sof = build_dir
        .join("output_files")
        .join(format!("{}.sof", manifest.project.name));

    Ok(BuildPlan {
        build_dir,
        steps,
        output_sof,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use chipsmith_toolchain::manifest::{Hdl, PinMapping, Project, Target, ToolchainSpec};
    use std::collections::BTreeMap;

    /// Lay out a minimal project on disk and return its directory.
    fn scratch_project(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("chipsmith-test-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("src")).unwrap();
        std::fs::write(dir.join("src/blinky.vhd"), "-- test\n").unwrap();
        dir
    }

    fn manifest_with_clocks(clocks: BTreeMap<String, String>) -> Manifest {
        let mut pins = BTreeMap::new();
        pins.insert("clk".to_string(), PinMapping::Single("PIN_Y2".to_string()));

        Manifest {
            project: Project {
                name: "blinky".to_string(),
                top: "blinky".to_string(),
            },
            toolchain: ToolchainSpec::new("quartus-prime", "23.1"),
            target: Target {
                family: "Cyclone V".to_string(),
                device: "5CSEBA6U23I7".to_string(),
                io_standard: None,
            },
            hdl: Hdl {
                standard: "VHDL_2008".to_string(),
                sources: vec!["src/*.vhd".to_string()],
            },
            pins,
            clocks,
            io_standards: BTreeMap::new(),
        }
    }

    #[test]
    fn writes_sdc_alongside_qsf_when_clocks_are_declared() {
        let dir = scratch_project("with-clocks");
        let mut clocks = BTreeMap::new();
        clocks.insert("clk".to_string(), "50 MHz".to_string());

        let plan = prepare_build(&dir, &manifest_with_clocks(clocks)).unwrap();

        let sdc = std::fs::read_to_string(plan.build_dir.join("blinky.sdc")).unwrap();
        assert!(sdc.contains("create_clock -name clk -period 20.000 [get_ports clk]"));

        let qsf = std::fs::read_to_string(plan.build_dir.join("blinky.qsf")).unwrap();
        assert!(qsf.contains("set_global_assignment -name SDC_FILE blinky.sdc"));

        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// The examples in the repo are the first thing anyone runs, so hold them to
    /// the real path: load the shipped manifest and generate a full build dir.
    #[test]
    fn shipped_examples_produce_a_constrained_project() {
        let repo = Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .and_then(Path::parent)
            .unwrap()
            .to_path_buf();

        for (example, clock_port, period) in [
            ("example", "clk", "20.000"),
            ("example-de2", "CLOCK_50", "20.000"),
        ] {
            let source = repo.join(example);
            let dir = scratch_project(example);
            std::fs::copy(source.join("chipsmith.toml"), dir.join("chipsmith.toml")).unwrap();

            let manifest = Manifest::load(&dir).unwrap();
            let plan = prepare_build(&dir, &manifest).unwrap();

            let sdc = std::fs::read_to_string(
                plan.build_dir
                    .join(format!("{}.sdc", manifest.project.name)),
            )
            .unwrap();
            assert!(
                sdc.contains(&format!(
                    "create_clock -name {clock_port} -period {period} [get_ports {clock_port}]"
                )),
                "{example} sdc:\n{sdc}"
            );

            let qsf = std::fs::read_to_string(
                plan.build_dir
                    .join(format!("{}.qsf", manifest.project.name)),
            )
            .unwrap();
            assert!(qsf.contains("SDC_FILE"), "{example} qsf:\n{qsf}");
            assert!(
                qsf.contains("IO_STANDARD \"3.3-V LVTTL\""),
                "{example} qsf:\n{qsf}"
            );

            std::fs::remove_dir_all(&dir).unwrap();
        }
    }

    #[test]
    fn writes_no_sdc_when_clocks_are_absent() {
        let dir = scratch_project("no-clocks");

        let plan = prepare_build(&dir, &manifest_with_clocks(BTreeMap::new())).unwrap();

        assert!(!plan.build_dir.join("blinky.sdc").exists());
        let qsf = std::fs::read_to_string(plan.build_dir.join("blinky.qsf")).unwrap();
        assert!(!qsf.contains("SDC_FILE"));

        std::fs::remove_dir_all(&dir).unwrap();
    }
}
