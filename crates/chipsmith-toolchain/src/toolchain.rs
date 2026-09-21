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
