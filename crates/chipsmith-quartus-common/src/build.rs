//! Deciding what a build consists of, and then doing it.
//!
//! `plan_build` is pure: given a Manifest and its sources it returns a complete
//! description of the Build Directory — every file, every Build Step, every
//! artifact path — without touching the filesystem. `materialize` writes that
//! description out. Splitting them means the whole decision surface is
//! assertable without a scratch directory.

use std::path::{Path, PathBuf};

use chipsmith_toolchain::error::ChipsmithError;
use chipsmith_toolchain::manifest::Manifest;

use crate::{qsf, sdc};

/// Where a Project's build artifacts live. The single owner of the layout —
/// anything that needs to name a build artifact asks this, rather than
/// re-deriving `build/output_files/<name>.sof` for itself.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BuildLayout {
    build_dir: PathBuf,
    project_name: String,
}

impl BuildLayout {
    pub fn new(project_dir: &Path, manifest: &Manifest) -> Self {
        Self {
            build_dir: project_dir.join("build"),
            project_name: manifest.project.name.clone(),
        }
    }

    pub fn build_dir(&self) -> &Path {
        &self.build_dir
    }

    /// Quartus writes everything it produces under `PROJECT_OUTPUT_DIRECTORY`,
    /// which the generated Settings File pins to `output_files`.
    fn output_dir(&self) -> PathBuf {
        self.build_dir.join("output_files")
    }

    /// The Bitstream a successful build produces.
    pub fn bitstream(&self) -> PathBuf {
        self.output_dir().join(format!("{}.sof", self.project_name))
    }

    /// Where `quartus_sta` leaves its Timing Summary.
    pub fn timing_summary(&self) -> PathBuf {
        self.output_dir()
            .join(format!("{}.sta.summary", self.project_name))
    }

    pub fn settings_file(&self) -> PathBuf {
        self.build_dir.join(format!("{}.qsf", self.project_name))
    }

    pub fn project_file(&self) -> PathBuf {
        self.build_dir.join(format!("{}.qpf", self.project_name))
    }

    pub fn constraints_file(&self) -> PathBuf {
        self.build_dir.join(format!("{}.sdc", self.project_name))
    }

    /// The Constraints File as the Settings File has to refer to it — relative,
    /// because Quartus resolves it against the Build Directory.
    fn constraints_file_name(&self) -> String {
        format!("{}.sdc", self.project_name)
    }
}

pub struct BuildStep {
    pub label: &'static str,
    pub tool: &'static str,
    pub args: Vec<String>,
}

/// A file the Build Directory needs, and what goes in it.
pub struct PlannedFile {
    pub path: PathBuf,
    pub contents: String,
}

pub struct BuildPlan {
    pub layout: BuildLayout,
    pub files: Vec<PlannedFile>,
    pub steps: Vec<BuildStep>,
}

impl BuildPlan {
    pub fn build_dir(&self) -> &Path {
        self.layout.build_dir()
    }

    pub fn bitstream(&self) -> PathBuf {
        self.layout.bitstream()
    }

    pub fn timing_summary(&self) -> PathBuf {
        self.layout.timing_summary()
    }
}

/// Decide everything about a build. Performs no I/O.
pub fn plan_build(
    project_dir: &Path,
    manifest: &Manifest,
    sources: &[PathBuf],
) -> Result<BuildPlan, ChipsmithError> {
    let layout = BuildLayout::new(project_dir, manifest);

    // One decision, one owner: the Constraints File is written and referenced
    // on the same condition, so the Settings File can never point at a file
    // that was never generated.
    let constraints = (!manifest.clocks.is_empty()).then(|| layout.constraints_file_name());

    let mut files = vec![
        PlannedFile {
            path: layout.settings_file(),
            contents: qsf::generate_qsf(manifest, sources, project_dir, constraints.as_deref())?,
        },
        PlannedFile {
            path: layout.project_file(),
            contents: qsf::generate_qpf(manifest),
        },
    ];
    if constraints.is_some() {
        files.push(PlannedFile {
            path: layout.constraints_file(),
            contents: sdc::generate_sdc(&manifest.clocks),
        });
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

    Ok(BuildPlan {
        layout,
        files,
        steps,
    })
}

/// Write a plan's files into the Build Directory.
pub fn materialize(plan: &BuildPlan) -> Result<(), ChipsmithError> {
    std::fs::create_dir_all(plan.build_dir())?;
    for file in &plan.files {
        std::fs::write(&file.path, &file.contents)?;
    }
    Ok(())
}

/// Resolve sources, plan the build, and lay the Build Directory out.
pub fn prepare_build(project_dir: &Path, manifest: &Manifest) -> Result<BuildPlan, ChipsmithError> {
    let sources = manifest.resolve_sources(project_dir)?;
    let plan = plan_build(project_dir, manifest, &sources)?;
    materialize(&plan)?;
    Ok(plan)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chipsmith_toolchain::manifest::{Clock, Hdl, PinMapping, Project, Target, ToolchainSpec};
    use std::collections::BTreeMap;

    fn manifest_with_clocks(clocks: Vec<Clock>) -> Manifest {
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

    fn plan_for(clocks: Vec<Clock>) -> BuildPlan {
        let sources = vec![PathBuf::from("/proj/src/blinky.vhd")];
        plan_build(Path::new("/proj"), &manifest_with_clocks(clocks), &sources).unwrap()
    }

    fn clock(port: &str, period_ns: f64) -> Clock {
        Clock {
            port: port.to_string(),
            period_ns,
        }
    }

    fn file<'a>(plan: &'a BuildPlan, name: &str) -> Option<&'a PlannedFile> {
        plan.files
            .iter()
            .find(|f| f.path.file_name().unwrap() == name)
    }

    #[test]
    fn every_artifact_path_hangs_off_the_same_build_directory() {
        let plan = plan_for(vec![]);
        assert_eq!(plan.build_dir(), Path::new("/proj/build"));
        assert_eq!(
            plan.bitstream(),
            Path::new("/proj/build/output_files/blinky.sof")
        );
        assert_eq!(
            plan.timing_summary(),
            Path::new("/proj/build/output_files/blinky.sta.summary")
        );
    }

    /// The layout is derivable without a Build Plan, so `flash` can find a
    /// Bitstream from a Manifest alone and still agree with what `build` wrote.
    #[test]
    fn the_layout_agrees_with_the_plan_it_is_not_part_of() {
        let manifest = manifest_with_clocks(vec![]);
        let layout = BuildLayout::new(Path::new("/proj"), &manifest);
        assert_eq!(layout.bitstream(), plan_for(vec![]).bitstream());
    }

    #[test]
    fn a_constrained_project_writes_and_references_the_same_sdc() {
        let plan = plan_for(vec![clock("clk", 20.0)]);

        let sdc = file(&plan, "blinky.sdc").expect("sdc should be planned");
        assert!(sdc
            .contents
            .contains("create_clock -name clk -period 20.000 [get_ports clk]"));

        let qsf = file(&plan, "blinky.qsf").unwrap();
        assert!(qsf
            .contents
            .contains("set_global_assignment -name SDC_FILE blinky.sdc"));
    }

    #[test]
    fn an_unconstrained_project_neither_writes_nor_references_an_sdc() {
        let plan = plan_for(vec![]);
        assert!(file(&plan, "blinky.sdc").is_none());
        assert!(!file(&plan, "blinky.qsf").unwrap().contents.contains("SDC"));
    }

    #[test]
    fn the_steps_run_synthesis_through_timing_analysis_in_order() {
        let plan = plan_for(vec![]);
        let tools: Vec<_> = plan.steps.iter().map(|s| s.tool).collect();
        assert_eq!(
            tools,
            vec!["quartus_map", "quartus_fit", "quartus_asm", "quartus_sta"]
        );
    }

    /// The examples in the repo are the first thing anyone runs, so hold them
    /// to the real path: load the shipped Manifest and plan a full build.
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
            let dir = repo.join(example);
            let manifest = Manifest::load(&dir).unwrap();
            let sources = manifest.resolve_sources(&dir).unwrap();
            assert!(!sources.is_empty(), "{example} has no sources");

            let plan = plan_build(&dir, &manifest, &sources).unwrap();
            let name = &manifest.project.name;

            let sdc = file(&plan, &format!("{name}.sdc")).expect("example should be constrained");
            assert!(
                sdc.contents.contains(&format!(
                    "create_clock -name {clock_port} -period {period} [get_ports {clock_port}]"
                )),
                "{example} sdc:\n{}",
                sdc.contents
            );

            let qsf = file(&plan, &format!("{name}.qsf")).unwrap();
            assert!(qsf.contents.contains("SDC_FILE"), "{example}");
            assert!(
                qsf.contents.contains("IO_STANDARD \"3.3-V LVTTL\""),
                "{example}"
            );
            // the example's real sources, not a stub planted by the test
            assert!(
                qsf.contents.contains("VHDL_FILE ../src/"),
                "{example} qsf:\n{}",
                qsf.contents
            );
        }
    }

    #[test]
    fn materialize_writes_exactly_what_the_plan_described() {
        let dir =
            std::env::temp_dir().join(format!("chipsmith-materialize-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);

        let manifest = manifest_with_clocks(vec![clock("clk", 20.0)]);
        let plan = plan_build(&dir, &manifest, &[PathBuf::from("src/blinky.vhd")]).unwrap();
        materialize(&plan).unwrap();

        for planned in &plan.files {
            assert_eq!(
                std::fs::read_to_string(&planned.path).unwrap(),
                planned.contents,
                "{}",
                planned.path.display()
            );
        }

        std::fs::remove_dir_all(&dir).unwrap();
    }
}
