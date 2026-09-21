//! The one `Toolchain` implementation for every Quartus Product.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use chipsmith_toolchain::error::ChipsmithError;
use chipsmith_toolchain::manifest::Manifest;
use chipsmith_toolchain::toolchain::{BuildOutcome, Toolchain};

use crate::process::{ProcessHost, RealHost};
use crate::product::QuartusProduct;
use crate::{build, install, runner, timing};

pub struct QuartusToolchain {
    product: &'static QuartusProduct,
    host: Arc<dyn ProcessHost>,
}

impl QuartusToolchain {
    pub fn new(product: &'static QuartusProduct) -> Self {
        Self::with_host(product, Arc::new(RealHost))
    }

    /// Same Toolchain against a different host. The seam exists because two
    /// things sit at it: the real OS, and the recording fake the tests use to
    /// reach the NixOS and non-NixOS paths that cannot both run on one machine.
    pub fn with_host(product: &'static QuartusProduct, host: Arc<dyn ProcessHost>) -> Self {
        Self { product, host }
    }

    pub const fn product(&self) -> &'static QuartusProduct {
        self.product
    }
}

#[async_trait::async_trait]
impl Toolchain for QuartusToolchain {
    fn name(&self) -> &str {
        self.product.backend
    }

    fn versions(&self) -> Vec<&str> {
        self.product.versions.iter().map(|v| v.key).collect()
    }

    fn default_version(&self) -> &str {
        self.product.latest
    }

    fn install_dir(&self, version: &str) -> Result<PathBuf, ChipsmithError> {
        Ok(self.product.install_dir_for(self.product.lookup(version)?))
    }

    fn is_installed(&self, version: &str) -> Result<bool, ChipsmithError> {
        Ok(self.product.is_installed(self.product.lookup(version)?))
    }

    async fn ensure_installed(&self, version: &str) -> Result<PathBuf, ChipsmithError> {
        install::ensure_installed(&*self.host, self.product, version).await
    }

    async fn install_from_local(
        &self,
        installer: &Path,
        version: &str,
    ) -> Result<(), ChipsmithError> {
        let dir = self.product.install_dir_for(self.product.lookup(version)?);
        runner::install_quartus(&*self.host, self.product, installer, &dir).await
    }

    async fn run_tool(
        &self,
        version: &str,
        tool: &str,
        args: &[String],
        working_dir: Option<&Path>,
    ) -> Result<(), ChipsmithError> {
        let dir = install::ensure_installed(&*self.host, self.product, version).await?;
        runner::run_tool(&*self.host, self.product, &dir, tool, args, working_dir).await
    }

    async fn build(
        &self,
        project_dir: &Path,
        manifest: &Manifest,
    ) -> Result<BuildOutcome, ChipsmithError> {
        let install_dir =
            install::ensure_installed(&*self.host, self.product, manifest.toolchain.version())
                .await?;
        let plan = build::prepare_build(project_dir, manifest)?;

        for step in &plan.steps {
            eprintln!("==> {} ({})", step.label, step.tool);
            runner::run_tool(
                &*self.host,
                self.product,
                &install_dir,
                step.tool,
                &step.args,
                Some(plan.build_dir()),
            )
            .await?;
        }

        Ok(BuildOutcome {
            bitstream: plan.bitstream(),
            timing: timing::read_timing(&plan.timing_summary()),
        })
    }

    async fn flash(
        &self,
        version: &str,
        bitstream: &Path,
        cable: Option<&str>,
    ) -> Result<(), ChipsmithError> {
        if !bitstream.exists() {
            return Err(ChipsmithError::OutputNotFound {
                path: bitstream.to_path_buf(),
            });
        }

        let dir = install::ensure_installed(&*self.host, self.product, version).await?;
        let args = runner::flash_args(bitstream, cable);
        runner::run_tool(&*self.host, self.product, &dir, "quartus_pgm", &args, None).await
    }
}
