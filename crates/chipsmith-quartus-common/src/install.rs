use std::path::Path;

use chipsmith_toolchain::error::ChipsmithError;

use crate::download;

#[derive(Debug)]
pub struct QuartusVersion {
    pub version: &'static str,
    pub revision: &'static str,
    pub filename: &'static str,
}

#[derive(Debug)]
pub struct DeviceSupport {
    pub family: &'static str,
    pub filename: &'static str,
}

#[derive(Debug)]
pub struct KnownVersion {
    pub key: &'static str,
    pub download: QuartusVersion,
    pub install_subdir: &'static str,
    pub devices: &'static [DeviceSupport],
}

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

pub fn lookup<'a>(
    versions: &'a [KnownVersion],
    key: &str,
) -> Result<&'a KnownVersion, ChipsmithError> {
    versions
        .iter()
        .find(|v| v.key == key)
        .ok_or_else(|| ChipsmithError::UnknownVersion {
            version: key.to_string(),
            available: versions.iter().map(|v| v.key.to_string()).collect(),
        })
}

/// Install any missing device support packages for a given version.
pub async fn install_device_support(
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
                if let Err(e) = download::unzip(&qdz, install_dir).await {
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

    static TEST_VERSIONS: &[KnownVersion] = &[
        KnownVersion {
            key: "1.0",
            download: QuartusVersion {
                version: "1.0",
                revision: "100",
                filename: "test.run",
            },
            install_subdir: "1.0",
            devices: &[],
        },
        KnownVersion {
            key: "2.0",
            download: QuartusVersion {
                version: "2.0",
                revision: "200",
                filename: "test2.run",
            },
            install_subdir: "2.0",
            devices: &[],
        },
    ];

    #[test]
    fn lookup_finds_known_version() {
        let v = lookup(TEST_VERSIONS, "1.0").unwrap();
        assert_eq!(v.key, "1.0");
        assert_eq!(v.download.revision, "100");
    }

    #[test]
    fn lookup_returns_error_for_unknown_version() {
        let err = lookup(TEST_VERSIONS, "99.0").unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("99.0"));
        assert!(msg.contains("1.0"));
        assert!(msg.contains("2.0"));
    }

    #[test]
    fn cdn_url_format() {
        let ver = &TEST_VERSIONS[0].download;
        let url = cdn_url(ver, "device.qdz");
        assert_eq!(
            url,
            "https://download.altera.com/akdlm/software/acdsinst/1.0/100/ib_installers/device.qdz"
        );
    }
}
