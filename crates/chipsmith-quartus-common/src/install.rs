//! Acquiring a Version of a Product: download, install, add Device Support.

use std::path::{Path, PathBuf};

use chipsmith_toolchain::error::ChipsmithError;

use crate::download;
use crate::process::ProcessHost;
use crate::product::{KnownVersion, QuartusProduct, QuartusVersion};
use crate::runner;

// Intel spun Altera out and retired the downloads.intel.com/akdlm paths — they now
// 301 into corpredirect.intel.com's 404 redirector. Altera serves the same directory
// layout from its own host. Revisit if Altera reorganises the CDN again.
const QUARTUS_CDN: &str = "https://download.altera.com/akdlm/software/acdsinst";

pub fn cdn_url(ver: &QuartusVersion, filename: &str) -> String {
    format!(
        "{}/{}/{}/ib_installers/{}",
        QUARTUS_CDN, ver.version, ver.revision, filename
    )
}

pub async fn ensure_installed(
    host: &dyn ProcessHost,
    product: &QuartusProduct,
    version_key: &str,
) -> Result<PathBuf, ChipsmithError> {
    let version = product.lookup(version_key)?;
    let dir = product.install_dir_for(version);

    if !product.is_installed(version) {
        eprintln!(
            "{} {} not found, downloading and installing...",
            product.display_name, version_key
        );

        let url = cdn_url(&version.download, version.download.filename);
        let installer = download::download_file(&url, version.download.filename).await?;
        download::make_executable(&installer).await?;

        eprintln!(
            "Installing {} {} to {}",
            product.display_name,
            version_key,
            dir.display()
        );
        runner::install_quartus(host, product, &installer, &dir).await?;
    }

    install_device_support(host, version, &dir).await?;

    Ok(dir)
}

/// Install any missing device support packages for a given version.
pub async fn install_device_support(
    host: &dyn ProcessHost,
    version: &KnownVersion,
    install_dir: &Path,
) -> Result<(), ChipsmithError> {
    for device in version.devices {
        let marker = install_dir
            .join(".chipsmith_device_installed_")
            .join(device.family);
        if marker.exists() {
            continue;
        }

        eprintln!("Installing {} device support...", device.family);
        let url = cdn_url(&version.download, device.filename);
        match download::download_file(&url, device.filename).await {
            Ok(qdz) => {
                if let Err(e) = download::unzip(host, &qdz, install_dir).await {
                    eprintln!(
                        "Warning: failed to install {} device support: {}",
                        device.family, e
                    );
                    continue;
                }
            }
            Err(e) => {
                eprintln!(
                    "Warning: failed to download {} device support: {}",
                    device.family, e
                );
                continue;
            }
        }

        if let Some(parent) = marker.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&marker, "")?;
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::product::TEST_PRODUCT;

    #[test]
    fn cdn_url_format() {
        let ver = &TEST_PRODUCT.versions[0].download;
        let url = cdn_url(ver, "device.qdz");
        assert_eq!(
            url,
            "https://download.altera.com/akdlm/software/acdsinst/1.0/100/ib_installers/device.qdz"
        );
    }
}
