//! Running a Project's Testbenches.
//!
//! A Simulator decides what to invoke; this crate owns everything that is the
//! same whichever one you picked — what a run needs, how a plan is turned into
//! processes, and what a verdict looks like coming back out.
//!
//! The split matters because the two Simulators chipsmith supports disagree
//! about almost everything at the command line and about nothing at all above
//! it: both analyse a pile of VHDL into a work library, elaborate one
//! Testbench entity, run it, and exit non-zero if an assertion failed.

pub mod ghdl;

use std::ffi::OsString;
use std::path::{Path, PathBuf};

use chipsmith_toolchain::error::ChipsmithError;
use chipsmith_toolchain::manifest::{Manifest, Sim, VhdlStandard};
use chipsmith_toolchain::process::{Capture, ProcessHost, ProcessSpec};

/// Everything a Simulator needs in order to run, and nothing about any
/// particular one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SimPlan {
    work_dir: PathBuf,
    standard: VhdlStandard,
    sources: Vec<PathBuf>,
    testbenches: Vec<String>,
}

impl SimPlan {
    /// Resolve what a `chipsmith test` run consists of.
    ///
    /// `only` narrows the run to one Testbench, and is checked here rather
    /// than by the Simulator: a name that is not in the Manifest is a mistake
    /// worth catching before anything spawns.
    pub fn new(
        project_dir: &Path,
        manifest: &Manifest,
        sim: &Sim,
        only: Option<&str>,
    ) -> Result<Self, ChipsmithError> {
        let testbenches = match only {
            None => sim.testbenches.clone(),
            Some(name) if sim.testbenches.iter().any(|tb| tb == name) => vec![name.to_string()],
            Some(name) => {
                return Err(ChipsmithError::UnknownTestbench {
                    name: name.to_string(),
                    available: sim.testbenches.clone(),
                })
            }
        };

        Ok(Self {
            // Alongside the Build Directory and disposable for the same
            // reason: it holds a work library, not source.
            work_dir: project_dir.join("build").join("sim"),
            standard: manifest.hdl.standard,
            sources: manifest.resolve_sim_sources(sim, project_dir)?,
            testbenches,
        })
    }

    /// The work library directory. Every Simulator runs with this as its
    /// working directory, so nothing it scatters lands in the Project.
    pub fn work_dir(&self) -> &Path {
        &self.work_dir
    }

    pub fn standard(&self) -> VhdlStandard {
        self.standard
    }

    pub fn sources(&self) -> &[PathBuf] {
        &self.sources
    }

    pub fn testbenches(&self) -> &[String] {
        &self.testbenches
    }
}

/// A file a Simulator needs sitting in the work directory before it runs.
pub struct SimFile {
    pub name: String,
    pub contents: String,
}

/// One invocation, with a label for whoever is reading the output.
pub struct SimStep {
    pub label: String,
    pub program: PathBuf,
    pub args: Vec<OsString>,
}

impl SimStep {
    pub fn new(label: impl Into<String>, program: impl Into<PathBuf>, args: Vec<OsString>) -> Self {
        Self {
            label: label.into(),
            program: program.into(),
            args,
        }
    }

    /// The command as a human would have typed it.
    pub fn command_line(&self) -> String {
        ProcessSpec::new(&self.program)
            .args(self.args.clone())
            .command_line()
    }
}

/// One Testbench's invocations. They run in order and stop at the first
/// failure, because a Testbench that would not elaborate cannot be run.
pub struct TestbenchRun {
    pub testbench: String,
    pub steps: Vec<SimStep>,
}

/// What a Simulator decided to do.
pub struct SimCommands {
    pub files: Vec<SimFile>,
    /// Runs once. Failing here is an error, not a verdict — nothing got far
    /// enough to have one.
    pub setup: Vec<SimStep>,
    pub runs: Vec<TestbenchRun>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TestVerdict {
    Passed,
    /// What the Simulator said before it gave up, when it said anything.
    Failed {
        detail: Option<String>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TestResult {
    pub testbench: String,
    pub verdict: TestVerdict,
}

impl TestResult {
    pub fn passed(&self) -> bool {
        self.verdict == TestVerdict::Passed
    }
}

/// What a finished `chipsmith test` yields. Like a Build Outcome, the verdict
/// travels out: the library reports, the caller decides the exit code.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TestReport {
    /// How the Simulator that produced this calls itself, so a report can say
    /// what actually ran.
    pub simulator: String,
    pub results: Vec<TestResult>,
}

impl TestReport {
    pub fn passed(&self) -> bool {
        self.results.iter().all(TestResult::passed)
    }

    pub fn failures(&self) -> impl Iterator<Item = &TestResult> {
        self.results.iter().filter(|r| !r.passed())
    }
}

/// A simulator chipsmith can run Testbenches on.
#[async_trait::async_trait]
pub trait Simulator: Send + Sync {
    /// The name a Manifest selects this Simulator with.
    fn name(&self) -> &str;

    /// How the Simulator describes itself in a report — the vendor's name for
    /// it, which is not always the name the Manifest uses.
    fn display_name(&self) -> String {
        self.name().to_string()
    }

    async fn test(&self, plan: &SimPlan) -> Result<TestReport, ChipsmithError>;
}

/// Run a Simulator's decisions and turn exit codes into verdicts.
///
/// Every Simulator ends up here, which is what keeps "a failed assertion is a
/// failed test, a Testbench that will not compile is a failed run" one rule
/// rather than one per simulator.
pub async fn execute(
    host: &dyn ProcessHost,
    simulator: &str,
    plan: &SimPlan,
    commands: SimCommands,
    dress: &(dyn Fn(ProcessSpec) -> ProcessSpec + Sync),
) -> Result<TestReport, ChipsmithError> {
    let work_dir = plan.work_dir();
    std::fs::create_dir_all(work_dir).map_err(ChipsmithError::file("create", work_dir))?;
    for file in &commands.files {
        let path = work_dir.join(&file.name);
        std::fs::write(&path, &file.contents).map_err(ChipsmithError::file("write", &path))?;
    }

    let spec_for = |step: &SimStep| {
        dress(
            ProcessSpec::new(&step.program)
                .args(step.args.clone())
                .current_dir(work_dir)
                // Tee: a simulation prints its reports as it goes and the user
                // wants to watch, but a failure has to explain itself after.
                .capture(Capture::Tee),
        )
    };

    for step in &commands.setup {
        eprintln!("==> {}", step.label);
        let outcome = host.run(spec_for(step)).await?;
        if !outcome.success() {
            return Err(ChipsmithError::ProcessFailed {
                command: step.command_line(),
                code: outcome.code,
                stderr_tail: outcome.stderr_tail,
            });
        }
    }

    let mut results = Vec::new();
    for run in &commands.runs {
        let mut verdict = TestVerdict::Passed;
        for step in &run.steps {
            eprintln!("==> {}", step.label);
            let outcome = host.run(spec_for(step)).await?;
            if !outcome.success() {
                verdict = TestVerdict::Failed {
                    detail: outcome.stderr_tail,
                };
                break;
            }
        }
        results.push(TestResult {
            testbench: run.testbench.clone(),
            verdict,
        });
    }

    Ok(TestReport {
        simulator: simulator.to_string(),
        results,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use chipsmith_toolchain::process::fake::RecordingHost;

    pub(crate) fn plan() -> SimPlan {
        SimPlan {
            work_dir: PathBuf::from("/proj/build/sim"),
            standard: VhdlStandard::Vhdl2008,
            sources: vec![PathBuf::from("/proj/src/blinky.vhd")],
            testbenches: vec!["a_tb".to_string(), "b_tb".to_string()],
        }
    }

    fn step(label: &str) -> SimStep {
        SimStep::new(label, "/bin/sim", vec![label.into()])
    }

    fn commands() -> SimCommands {
        SimCommands {
            files: Vec::new(),
            setup: vec![step("analyse")],
            runs: vec![
                TestbenchRun {
                    testbench: "a_tb".to_string(),
                    steps: vec![step("elaborate a"), step("run a")],
                },
                TestbenchRun {
                    testbench: "b_tb".to_string(),
                    steps: vec![step("elaborate b"), step("run b")],
                },
            ],
        }
    }

    /// `execute` creates the work directory, so give each test its own.
    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("chipsmith-sim-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    fn plan_in(dir: &Path) -> SimPlan {
        SimPlan {
            work_dir: dir.to_path_buf(),
            ..plan()
        }
    }

    #[tokio::test]
    async fn every_testbench_reports_a_verdict_and_the_simulator_names_itself() {
        let dir = scratch("all-pass");
        let host = RecordingHost::new();
        let report = execute(&host, "ghdl", &plan_in(&dir), commands(), &|s| s)
            .await
            .unwrap();

        assert_eq!(report.simulator, "ghdl");
        assert!(report.passed());
        assert_eq!(
            report
                .results
                .iter()
                .map(|r| &r.testbench)
                .collect::<Vec<_>>(),
            vec!["a_tb", "b_tb"]
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// A Testbench that fails must not hide the ones after it — running two and
    /// hearing about one is exactly how a broken suite stays broken.
    #[tokio::test]
    async fn a_failing_testbench_is_a_verdict_and_the_rest_still_run() {
        let dir = scratch("one-fails");
        let host = RecordingHost::new()
            // analyse, elaborate a, run a
            .will_reply(RecordingHost::succeeding_with(""))
            .will_reply(RecordingHost::succeeding_with(""))
            .will_reply(RecordingHost::failing(1, "assertion failed: led stuck"));

        let report = execute(&host, "ghdl", &plan_in(&dir), commands(), &|s| s)
            .await
            .unwrap();

        assert!(!report.passed());
        assert_eq!(report.results.len(), 2, "b_tb still ran");
        assert_eq!(
            report.results[0].verdict,
            TestVerdict::Failed {
                detail: Some("assertion failed: led stuck".to_string())
            }
        );
        assert!(report.results[1].passed());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// Elaboration failing means the run never happened, so there is nothing
    /// to learn from running it anyway.
    #[tokio::test]
    async fn a_testbench_that_will_not_elaborate_is_not_then_run() {
        let dir = scratch("no-elab");
        let host = RecordingHost::new()
            .will_reply(RecordingHost::succeeding_with(""))
            .will_reply(RecordingHost::failing(1, "entity not found"));

        let report = execute(&host, "ghdl", &plan_in(&dir), commands(), &|s| s)
            .await
            .unwrap();

        let labels: Vec<_> = host
            .runs
            .lock()
            .unwrap()
            .iter()
            .map(|s| s.command_line())
            .collect();
        assert!(!labels.iter().any(|c| c.ends_with("run a")), "{labels:?}");
        assert!(!report.results[0].passed());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// Sources that do not compile are not a test result — no Testbench got as
    /// far as having one.
    #[tokio::test]
    async fn a_setup_failure_is_an_error_rather_than_a_report_of_failures() {
        let dir = scratch("bad-setup");
        let host = RecordingHost::new()
            .will_reply(RecordingHost::failing(1, "blinky.vhd:14: syntax error"));

        let err = execute(&host, "ghdl", &plan_in(&dir), commands(), &|s| s)
            .await
            .unwrap_err();

        let message = err.to_string();
        assert!(message.contains("syntax error"), "{message}");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[tokio::test]
    async fn the_work_directory_is_created_and_is_where_everything_runs() {
        let dir = scratch("workdir");
        let host = RecordingHost::new();
        let mut commands = commands();
        commands.files.push(SimFile {
            name: "modelsim.ini".to_string(),
            contents: "[Library]\n".to_string(),
        });

        execute(&host, "quartus", &plan_in(&dir), commands, &|s| s)
            .await
            .unwrap();

        assert_eq!(
            std::fs::read_to_string(dir.join("modelsim.ini")).unwrap(),
            "[Library]\n"
        );
        for spec in host.runs.lock().unwrap().iter() {
            assert_eq!(spec.working_dir.as_deref(), Some(dir.as_path()));
        }
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// The seam the Quartus simulator needs: its binaries have to be dressed
    /// for NixOS exactly like every other vendor binary.
    #[tokio::test]
    async fn a_simulator_can_dress_every_spec_before_it_is_spawned() {
        let dir = scratch("dressed");
        let host = RecordingHost::new();
        execute(&host, "quartus", &plan_in(&dir), commands(), &|spec| {
            spec.env("LD_LIBRARY_PATH", "/nix/lib")
        })
        .await
        .unwrap();

        for spec in host.runs.lock().unwrap().iter() {
            assert_eq!(
                spec.env[0],
                ("LD_LIBRARY_PATH".to_string(), "/nix/lib".to_string())
            );
        }
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
