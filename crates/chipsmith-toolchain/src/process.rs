//! The seam between chipsmith and the operating system.
//!
//! Every subprocess and every "what kind of host is this" question goes
//! through a `ProcessHost`. In production that's `RealHost`; in tests it's a
//! recording fake, which is what makes the NixOS and non-NixOS paths — one of
//! which never runs on any given machine — both reachable.

use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};
use std::process::Stdio;

use tokio::io::{AsyncBufReadExt, BufReader};

use crate::error::ChipsmithError;

/// What to do with a child's output.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Capture {
    /// Straight to the user's terminal. Nothing is available afterwards.
    Inherit,
    /// Collected and returned; nothing reaches the terminal.
    Piped,
    /// Discarded.
    Null,
    /// stderr goes to the terminal *and* its tail is kept, so a tool that runs
    /// for ten minutes still shows progress live but can explain itself if it
    /// fails. stdout is inherited, so a tool that reports errors there behaves
    /// as it always has.
    Tee,
}

/// A process to run.
#[derive(Debug, Clone)]
pub struct ProcessSpec {
    pub program: PathBuf,
    pub args: Vec<OsString>,
    pub env: Vec<(String, String)>,
    pub working_dir: Option<PathBuf>,
    pub capture: Capture,
}

impl ProcessSpec {
    pub fn new(program: impl Into<PathBuf>) -> Self {
        Self {
            program: program.into(),
            args: Vec::new(),
            env: Vec::new(),
            working_dir: None,
            capture: Capture::Inherit,
        }
    }

    pub fn arg(mut self, arg: impl Into<OsString>) -> Self {
        self.args.push(arg.into());
        self
    }

    pub fn args<I, S>(mut self, args: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<OsString>,
    {
        self.args.extend(args.into_iter().map(Into::into));
        self
    }

    pub fn env(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.env.push((key.into(), value.into()));
        self
    }

    pub fn current_dir(mut self, dir: impl Into<PathBuf>) -> Self {
        self.working_dir = Some(dir.into());
        self
    }

    pub fn capture(mut self, capture: Capture) -> Self {
        self.capture = capture;
        self
    }

    /// Run `program` under a wrapper — the wrapper's argv, then this program
    /// and its arguments. Used to put a sandbox in front of a vendor binary.
    pub fn wrapped_in(self, wrapper: Vec<OsString>) -> Self {
        let (program, prefix) = wrapper.split_first().expect("wrapper argv is never empty");
        let mut args: Vec<OsString> = prefix.to_vec();
        args.push(self.program.into_os_string());
        args.extend(self.args);
        Self {
            program: PathBuf::from(program),
            args,
            ..self
        }
    }

    /// The command as a human would write it, for error messages.
    pub fn command_line(&self) -> String {
        std::iter::once(self.program.as_os_str())
            .chain(self.args.iter().map(OsString::as_os_str))
            .map(OsStr::to_string_lossy)
            .collect::<Vec<_>>()
            .join(" ")
    }
}

#[derive(Debug, Clone, Default)]
pub struct ProcessOutcome {
    pub code: Option<i32>,
    pub stdout: Vec<u8>,
    /// The last lines of stderr, when the spec asked for them.
    pub stderr_tail: Option<String>,
}

impl ProcessOutcome {
    pub fn success(&self) -> bool {
        self.code == Some(0)
    }
}

#[async_trait::async_trait]
pub trait ProcessHost: Send + Sync {
    async fn run(&self, spec: ProcessSpec) -> Result<ProcessOutcome, ChipsmithError>;

    /// Whether this host needs the Nix Compat layer. A method rather than a
    /// free function reading `/etc/NIXOS`, so both answers are testable.
    fn is_nixos(&self) -> bool;

    /// Whether a path exists. Here for the same reason as `is_nixos`.
    fn path_exists(&self, path: &Path) -> bool;

    /// Resolve an executable on `PATH`. A host question like the other two, so
    /// a tool chipsmith expects to find rather than install can be reported
    /// missing by name instead of as a bare spawn failure.
    fn which(&self, program: &str) -> Option<PathBuf>;
}

/// How many lines of stderr to keep for an error message.
const TAIL_LINES: usize = 20;

pub struct RealHost;

#[async_trait::async_trait]
impl ProcessHost for RealHost {
    async fn run(&self, spec: ProcessSpec) -> Result<ProcessOutcome, ChipsmithError> {
        let mut cmd = tokio::process::Command::new(&spec.program);
        cmd.args(&spec.args);
        for (key, value) in &spec.env {
            cmd.env(key, value);
        }
        if let Some(dir) = &spec.working_dir {
            cmd.current_dir(dir);
        }
        match spec.capture {
            Capture::Inherit => {}
            Capture::Piped => {
                cmd.stdout(Stdio::piped()).stderr(Stdio::piped());
            }
            Capture::Null => {
                cmd.stdout(Stdio::null()).stderr(Stdio::null());
            }
            Capture::Tee => {
                cmd.stderr(Stdio::piped());
            }
        }

        let spawn_error = |source| ChipsmithError::Spawn {
            command: spec.program.display().to_string(),
            source,
        };

        if spec.capture == Capture::Tee {
            let mut child = cmd.spawn().map_err(spawn_error)?;
            let stderr = child.stderr.take().expect("stderr was piped");
            let mut lines = BufReader::new(stderr).lines();
            let mut tail: Vec<String> = Vec::new();

            while let Some(line) = lines.next_line().await? {
                eprintln!("{line}");
                if tail.len() == TAIL_LINES {
                    tail.remove(0);
                }
                tail.push(line);
            }

            let status = child.wait().await?;
            return Ok(ProcessOutcome {
                code: status.code(),
                stdout: Vec::new(),
                stderr_tail: (!tail.is_empty()).then(|| tail.join("\n")),
            });
        }

        let output = cmd.spawn().map_err(spawn_error)?.wait_with_output().await?;
        Ok(ProcessOutcome {
            code: output.status.code(),
            stdout: output.stdout,
            stderr_tail: (spec.capture == Capture::Piped && !output.stderr.is_empty())
                .then(|| String::from_utf8_lossy(&output.stderr).into_owned()),
        })
    }

    fn is_nixos(&self) -> bool {
        Path::new("/etc/NIXOS").exists()
    }

    fn path_exists(&self, path: &Path) -> bool {
        path.exists()
    }

    fn which(&self, program: &str) -> Option<PathBuf> {
        let named = Path::new(program);
        if named.components().count() > 1 {
            return named.is_file().then(|| named.to_path_buf());
        }
        std::env::split_paths(&std::env::var_os("PATH")?)
            .map(|dir| dir.join(program))
            .find(|path| path.is_file())
    }
}

#[cfg(any(test, feature = "test-host"))]
pub mod fake {
    use super::*;
    use std::sync::Mutex;

    /// Records every spec it is asked to run and replies with canned outcomes.
    pub struct RecordingHost {
        pub nixos: bool,
        /// Paths the fake host should claim exist. `None` means everything does.
        pub existing: Option<Vec<PathBuf>>,
        /// Executables the fake host should claim are on `PATH`. `None` means
        /// every one asked for is.
        pub on_path: Option<Vec<String>>,
        replies: Mutex<Vec<ProcessOutcome>>,
        pub runs: Mutex<Vec<ProcessSpec>>,
    }

    impl RecordingHost {
        /// A host where every process succeeds silently.
        pub fn new() -> Self {
            Self {
                nixos: false,
                existing: None,
                on_path: None,
                replies: Mutex::new(Vec::new()),
                runs: Mutex::new(Vec::new()),
            }
        }

        pub fn on_nixos(mut self) -> Self {
            self.nixos = true;
            self
        }

        pub fn only_these_exist(mut self, paths: Vec<PathBuf>) -> Self {
            self.existing = Some(paths);
            self
        }

        pub fn only_these_on_path(mut self, programs: Vec<String>) -> Self {
            self.on_path = Some(programs);
            self
        }

        /// Queue an outcome. Replies are handed out in order; once the queue is
        /// empty every further run succeeds.
        pub fn will_reply(self, outcome: ProcessOutcome) -> Self {
            self.replies.lock().unwrap().push(outcome);
            self
        }

        pub fn failing(code: i32, stderr: &str) -> ProcessOutcome {
            ProcessOutcome {
                code: Some(code),
                stdout: Vec::new(),
                stderr_tail: Some(stderr.to_string()),
            }
        }

        pub fn succeeding_with(stdout: &str) -> ProcessOutcome {
            ProcessOutcome {
                code: Some(0),
                stdout: stdout.as_bytes().to_vec(),
                stderr_tail: None,
            }
        }

        pub fn commands(&self) -> Vec<String> {
            self.runs
                .lock()
                .unwrap()
                .iter()
                .map(ProcessSpec::command_line)
                .collect()
        }
    }

    impl Default for RecordingHost {
        fn default() -> Self {
            Self::new()
        }
    }

    #[async_trait::async_trait]
    impl ProcessHost for RecordingHost {
        async fn run(&self, spec: ProcessSpec) -> Result<ProcessOutcome, ChipsmithError> {
            self.runs.lock().unwrap().push(spec);
            let mut replies = self.replies.lock().unwrap();
            Ok(if replies.is_empty() {
                ProcessOutcome {
                    code: Some(0),
                    ..Default::default()
                }
            } else {
                replies.remove(0)
            })
        }

        fn is_nixos(&self) -> bool {
            self.nixos
        }

        fn path_exists(&self, path: &Path) -> bool {
            match &self.existing {
                Some(paths) => paths.iter().any(|p| p == path),
                None => true,
            }
        }

        fn which(&self, program: &str) -> Option<PathBuf> {
            let found = match &self.on_path {
                Some(programs) => programs.iter().any(|p| p == program),
                None => true,
            };
            found.then(|| PathBuf::from("/usr/bin").join(program))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_wrapper_keeps_the_program_and_its_arguments_in_order() {
        let spec = ProcessSpec::new("/q/quartus_map")
            .arg("blinky")
            .wrapped_in(vec!["/nix/bwrap".into(), "--tmpfs".into(), "/lib".into()]);

        assert_eq!(spec.program, Path::new("/nix/bwrap"));
        assert_eq!(
            spec.command_line(),
            "/nix/bwrap --tmpfs /lib /q/quartus_map blinky"
        );
    }

    #[test]
    fn the_command_line_is_what_a_human_would_have_typed() {
        let spec = ProcessSpec::new("/q/quartus_map").args(["blinky", "--read_settings_files=on"]);
        assert_eq!(
            spec.command_line(),
            "/q/quartus_map blinky --read_settings_files=on"
        );
    }

    #[tokio::test]
    async fn the_real_host_reports_a_failing_exit_code() {
        let outcome = RealHost
            .run(
                ProcessSpec::new("/bin/sh")
                    .args(["-c", "exit 3"])
                    .capture(Capture::Null),
            )
            .await
            .unwrap();
        assert_eq!(outcome.code, Some(3));
        assert!(!outcome.success());
    }

    #[tokio::test]
    async fn the_real_host_keeps_stderr_when_asked_to() {
        let outcome = RealHost
            .run(
                ProcessSpec::new("/bin/sh")
                    .args(["-c", "echo 'Error: entity not found' >&2; exit 1"])
                    .capture(Capture::Tee),
            )
            .await
            .unwrap();
        assert_eq!(outcome.code, Some(1));
        assert!(outcome.stderr_tail.unwrap().contains("entity not found"));
    }

    #[tokio::test]
    async fn the_real_host_returns_stdout_when_piped() {
        let outcome = RealHost
            .run(
                ProcessSpec::new("/bin/sh")
                    .args(["-c", "echo '{\"a\":\"b\"}'"])
                    .capture(Capture::Piped),
            )
            .await
            .unwrap();
        assert_eq!(
            String::from_utf8_lossy(&outcome.stdout).trim(),
            r#"{"a":"b"}"#
        );
    }

    #[test]
    fn the_real_host_finds_an_executable_on_path_and_admits_when_it_cannot() {
        let sh = RealHost.which("sh").expect("sh is on PATH everywhere");
        assert!(sh.is_absolute() && sh.ends_with("sh"), "{}", sh.display());
        assert_eq!(RealHost.which("definitely-not-a-program"), None);
        assert_eq!(
            RealHost.which("/definitely/not/a/program"),
            None,
            "a path that is not on PATH is still checked as a path"
        );
    }

    #[tokio::test]
    async fn spawning_something_that_is_not_there_is_a_spawn_error() {
        let err = RealHost
            .run(ProcessSpec::new("/definitely/not/a/binary"))
            .await
            .unwrap_err();
        assert!(matches!(err, ChipsmithError::Spawn { .. }), "{err}");
    }
}
