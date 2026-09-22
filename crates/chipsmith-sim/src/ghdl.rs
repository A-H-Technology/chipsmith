//! GHDL: the Simulator that needs no vendor toolchain and no licence.
//!
//! Argument construction is pure and separate from running, so what chipsmith
//! actually asks GHDL to do gets a test rather than an installed GHDL.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use chipsmith_toolchain::error::ChipsmithError;
use chipsmith_toolchain::manifest::VhdlStandard;
use chipsmith_toolchain::process::{ProcessHost, RealHost};

use crate::{SimCommands, SimPlan, SimStep, Simulator, TestReport, TestbenchRun};

/// GHDL is looked up on `PATH` rather than installed by chipsmith: unlike a
/// Toolchain it is a small free package every distribution already carries.
pub const PROGRAM: &str = "ghdl";

/// Every invocation says which language revision it is reading and which work
/// library it is reading into. The library is the process's own directory,
/// which `execute` has already made the working directory.
fn common(standard: VhdlStandard) -> Vec<OsString> {
    vec![
        format!("--std={}", standard.ghdl_std()).into(),
        "--workdir=.".into(),
    ]
}

/// `ghdl -a` — analyse every source into the work library, design first and
/// Testbench last.
pub fn analyse_args(standard: VhdlStandard, sources: &[PathBuf]) -> Vec<OsString> {
    let mut args: Vec<OsString> = vec!["-a".into()];
    args.extend(common(standard));
    args.extend(sources.iter().map(|s| s.as_os_str().to_owned()));
    args
}

/// `ghdl -e` — elaborate one Testbench from the analysed library.
pub fn elaborate_args(standard: VhdlStandard, testbench: &str) -> Vec<OsString> {
    let mut args: Vec<OsString> = vec!["-e".into()];
    args.extend(common(standard));
    args.push(testbench.into());
    args
}

/// `ghdl -r` — run one elaborated Testbench.
///
/// `--assert-level=error` is what makes a failed assertion a failed test.
/// Without it GHDL prints the assertion and still exits zero, so a broken
/// design passes — which is also the severity ModelSim breaks at once
/// chipsmith has written its `modelsim.ini`, so a Testbench means the same
/// thing on both Simulators.
pub fn run_args(standard: VhdlStandard, testbench: &str) -> Vec<OsString> {
    let mut args: Vec<OsString> = vec!["-r".into()];
    args.extend(common(standard));
    args.push(testbench.into());
    args.push("--assert-level=error".into());
    args
}

/// Everything GHDL is asked to do for one run.
pub fn commands(program: &Path, plan: &SimPlan) -> SimCommands {
    SimCommands {
        files: Vec::new(),
        setup: vec![SimStep::new(
            format!("Analysing {} source(s)", plan.sources().len()),
            program,
            analyse_args(plan.standard(), plan.sources()),
        )],
        runs: plan
            .testbenches()
            .iter()
            .map(|testbench| TestbenchRun {
                testbench: testbench.clone(),
                steps: vec![
                    SimStep::new(
                        format!("Elaborating {testbench}"),
                        program,
                        elaborate_args(plan.standard(), testbench),
                    ),
                    SimStep::new(
                        format!("Running {testbench}"),
                        program,
                        run_args(plan.standard(), testbench),
                    ),
                ],
            })
            .collect(),
    }
}

pub struct Ghdl {
    host: Arc<dyn ProcessHost>,
}

impl Ghdl {
    pub fn new() -> Self {
        Self::with_host(Arc::new(RealHost))
    }

    pub fn with_host(host: Arc<dyn ProcessHost>) -> Self {
        Self { host }
    }
}

impl Default for Ghdl {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait::async_trait]
impl Simulator for Ghdl {
    fn name(&self) -> &str {
        "ghdl"
    }

    async fn test(&self, plan: &SimPlan) -> Result<TestReport, ChipsmithError> {
        let program =
            self.host
                .which(PROGRAM)
                .ok_or_else(|| ChipsmithError::SimulatorNotFound {
                    program: PROGRAM.to_string(),
                    hint: "install it, e.g. `nix shell nixpkgs#ghdl` or your distribution's \
                           ghdl package"
                        .to_string(),
                })?;

        let commands = commands(&program, plan);
        crate::execute(&*self.host, self.name(), plan, commands, &|spec| spec).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chipsmith_toolchain::process::fake::RecordingHost;

    fn strings(args: Vec<OsString>) -> Vec<String> {
        args.into_iter()
            .map(|a| a.to_string_lossy().into_owned())
            .collect()
    }

    #[test]
    fn analysis_reads_every_source_in_the_order_it_was_given() {
        let sources = vec![
            PathBuf::from("/p/src/blinky.vhd"),
            PathBuf::from("/p/tb/blinky_tb.vhd"),
        ];
        assert_eq!(
            strings(analyse_args(VhdlStandard::Vhdl2008, &sources)),
            vec![
                "-a",
                "--std=08",
                "--workdir=.",
                "/p/src/blinky.vhd",
                "/p/tb/blinky_tb.vhd"
            ]
        );
    }

    #[test]
    fn the_manifests_vhdl_standard_reaches_the_simulator() {
        for (standard, flag) in [
            (VhdlStandard::Vhdl1987, "--std=87"),
            (VhdlStandard::Vhdl1993, "--std=93"),
            (VhdlStandard::Vhdl2008, "--std=08"),
        ] {
            assert!(strings(run_args(standard, "tb")).contains(&flag.to_string()));
        }
    }

    /// Without this GHDL prints the failed assertion and exits zero, so the
    /// whole point of running a Testbench is lost.
    #[test]
    fn a_run_fails_the_process_when_an_assertion_fails() {
        assert!(strings(run_args(VhdlStandard::Vhdl2008, "blinky_tb"))
            .contains(&"--assert-level=error".to_string()));
    }

    #[test]
    fn elaboration_names_the_testbench_entity() {
        assert_eq!(
            strings(elaborate_args(VhdlStandard::Vhdl2008, "blinky_tb")),
            vec!["-e", "--std=08", "--workdir=.", "blinky_tb"]
        );
    }

    #[tokio::test]
    async fn a_missing_ghdl_says_how_to_get_one_instead_of_failing_to_spawn() {
        let simulator = Ghdl::with_host(Arc::new(
            RecordingHost::new().only_these_on_path(vec!["quartus_sh".to_string()]),
        ));
        let err = simulator.test(&crate::tests::plan()).await.unwrap_err();
        let message = err.to_string();
        assert!(message.contains("ghdl"), "{message}");
        assert!(message.contains("nixpkgs#ghdl"), "{message}");
    }
}
