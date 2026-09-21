use std::path::{Path, PathBuf};

use chipsmith_toolchain::error::ChipsmithError;
use chipsmith_toolchain::manifest::Manifest;

use crate::install;
use crate::runner;

pub async fn build(project_dir: &Path, manifest: &Manifest) -> Result<PathBuf, ChipsmithError> {
    let install_dir = install::ensure_installed(manifest.toolchain.version()).await?;
    let plan = chipsmith_quartus_common::build::prepare_build(project_dir, manifest)?;

    for step in &plan.steps {
        eprintln!("==> {} ({})", step.label, step.tool);
        runner::run_tool(&install_dir, step.tool, &step.args, Some(&plan.build_dir)).await?;
    }

    chipsmith_quartus_common::timing::report_timing(&plan.build_dir, &manifest.project.name);

    eprintln!("Build complete: {}", plan.output_sof.display());
    Ok(plan.output_sof)
}
