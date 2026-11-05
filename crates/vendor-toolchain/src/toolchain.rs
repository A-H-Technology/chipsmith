use std::path::{Path, PathBuf};

use crate::error::ChipsmithError;
use crate::manifest::Manifest;
use crate::timing::TimingSummary;

/// What a finished build yields. Distinct from success: a build can complete,
/// produce a perfectly loadable Bitstream, and still miss timing. Deciding
/// what to do about that is the caller's business, so the verdict travels out
/// rather than being printed and dropped here.
#[derive(Debug)]
pub struct BuildOutcome {
    pub bitstream: PathBuf,
    /// The timing analyser's own summary file, which `timing` was read from.
    /// May not exist, for the same reasons `timing` may be `None`.
    pub timing_report: PathBuf,
    /// `None` when timing analysis left no summary behind — which after a
    /// build that ran the timing step means something went wrong, not that
    /// everything was fine.
    pub timing: Option<TimingSummary>,
}

impl BuildOutcome {
    /// Whether this build is trustworthy at its constrained frequency. A build
    /// with no Timing Summary is not.
    pub fn meets_timing(&self) -> bool {
        self.timing.as_ref().is_some_and(TimingSummary::met)
    }

    /// Copy the Bitstream, and the timing report if there is one, into `dir`
    /// under their own names. For a caller that wants the artifacts without
    /// knowing where a Toolchain keeps them — a nix derivation's `$out`.
    pub fn copy_to(&self, dir: &Path) -> Result<(), ChipsmithError> {
        std::fs::create_dir_all(dir).map_err(ChipsmithError::file("create", dir))?;
        let artifacts = std::iter::once(&self.bitstream)
            .chain(Some(&self.timing_report).filter(|report| report.exists()));
        for artifact in artifacts {
            let name = artifact.file_name().expect("artifacts are files");
            std::fs::copy(artifact, dir.join(name))
                .map_err(ChipsmithError::file("copy", artifact))?;
        }
        Ok(())
    }
}

#[async_trait::async_trait]
pub trait Toolchain: Send + Sync {
    /// The Backend name that selects this Toolchain.
    fn name(&self) -> &str;

    /// Every Version this Toolchain can install. The legal Version space is
    /// part of this interface, so callers can offer it rather than discover it
    /// from an `UnknownVersion` error.
    fn versions(&self) -> Vec<&str>;

    /// The Version to use when the caller doesn't name one. Backend-dependent,
    /// which is why it has to be asked for rather than defaulted globally.
    fn default_version(&self) -> &str;

    fn install_dir(&self, version: &str) -> Result<PathBuf, ChipsmithError>;

    fn is_installed(&self, version: &str) -> Result<bool, ChipsmithError>;

    /// Install the Version if it isn't already, and return its Install Root.
    /// May download several gigabytes and drive a vendor installer.
    async fn ensure_installed(&self, version: &str) -> Result<PathBuf, ChipsmithError>;

    async fn install_from_local(
        &self,
        installer: &Path,
        version: &str,
    ) -> Result<(), ChipsmithError>;

    /// Run one of this Toolchain's tools. Installs the Version first if needed.
    async fn run_tool(
        &self,
        version: &str,
        tool: &str,
        args: &[String],
        working_dir: Option<&Path>,
    ) -> Result<(), ChipsmithError>;

    async fn build(
        &self,
        project_dir: &Path,
        manifest: &Manifest,
    ) -> Result<BuildOutcome, ChipsmithError>;

    /// Load a Bitstream onto the chip over a Cable.
    async fn flash(
        &self,
        version: &str,
        bitstream: &Path,
        cable: Option<&str>,
    ) -> Result<(), ChipsmithError>;
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A build whose timing analysis left nothing behind still hands over its
    /// Bitstream, rather than failing the copy on the missing report.
    #[test]
    fn copying_out_takes_the_bitstream_and_whatever_timing_report_exists() {
        let scratch = std::env::temp_dir().join(format!("chipsmith-copy-{}", std::process::id()));
        let built = scratch.join("build/output_files");
        std::fs::create_dir_all(&built).unwrap();
        std::fs::write(built.join("blinky.sof"), "sof").unwrap();

        let outcome = BuildOutcome {
            bitstream: built.join("blinky.sof"),
            timing_report: built.join("blinky.sta.summary"),
            timing: None,
        };
        let out = scratch.join("out");
        outcome.copy_to(&out).unwrap();

        let mut copied: Vec<_> = std::fs::read_dir(&out)
            .unwrap()
            .map(|e| e.unwrap().file_name().into_string().unwrap())
            .collect();
        copied.sort();
        std::fs::remove_dir_all(&scratch).unwrap();
        assert_eq!(copied, ["blinky.sof"]);
    }
}
