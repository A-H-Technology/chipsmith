//! The one `Toolchain` implementation for every Quartus Product.

use std::path::{Path, PathBuf};

use chipsmith_toolchain::error::ChipsmithError;
use chipsmith_toolchain::manifest::Manifest;
use chipsmith_toolchain::toolchain::Toolchain;

use crate::product::QuartusProduct;
use crate::{build, install, runner, timing};

pub struct QuartusToolchain {
    product: &'static QuartusProduct,
}

impl QuartusToolchain {
    pub const fn new(product: &'static QuartusProduct) -> Self {
        Self { product }
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

    fn install_dir(&self, version: &str) -> Result<PathBuf, ChipsmithError> {
        Ok(self.product.install_dir_for(self.product.lookup(version)?))
    }

    fn is_installed(&self, version: &str) -> Result<bool, ChipsmithError> {
        Ok(self.product.is_installed(self.product.lookup(version)?))
    }

    async fn ensure_installed(&self, version: &str) -> Result<PathBuf, ChipsmithError> {
        install::ensure_installed(self.product, version).await
    }

    async fn install_from_local(
        &self,
        installer: &Path,
        version: &str,
    ) -> Result<(), ChipsmithError> {
        let dir = self.product.install_dir_for(self.product.lookup(version)?);
        runner::install_quartus(self.product, installer, &dir).await
    }

    async fn run_tool(
        &self,
        version: &str,
        tool: &str,
        args: &[String],
        working_dir: Option<&Path>,
    ) -> Result<(), ChipsmithError> {
        let dir = install::ensure_installed(self.product, version).await?;
        runner::run_tool(self.product, &dir, tool, args, working_dir).await
    }

    async fn build(
        &self,
        project_dir: &Path,
        manifest: &Manifest,
    ) -> Result<PathBuf, ChipsmithError> {
        let install_dir =
            install::ensure_installed(self.product, manifest.toolchain.version()).await?;
        let plan = build::prepare_build(project_dir, manifest)?;

        for step in &plan.steps {
            eprintln!("==> {} ({})", step.label, step.tool);
            runner::run_tool(
                self.product,
                &install_dir,
                step.tool,
                &step.args,
                Some(&plan.build_dir),
            )
            .await?;
        }

        timing::report_timing(&plan.build_dir, &manifest.project.name);

        eprintln!("Build complete: {}", plan.output_sof.display());
        Ok(plan.output_sof)
    }
}
