//! Acquiring a Version of a Product: download, install, add Device Support.

use std::path::{Path, PathBuf};

use chipsmith_toolchain::error::ChipsmithError;

use crate::download;
use crate::product::{InstallBase, KnownVersion, QuartusProduct, QuartusVersion};
use crate::runner;
use chipsmith_toolchain::process::ProcessHost;

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

/// What to do about an install patched for a different nixpkgs than this
/// chipsmith runs against.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StalePatch {
    /// Patch it again. What `chipsmith install` does: it is the command the
    /// user runs to put a Version right, and it owns the install.
    Repatch,
    /// Refuse, naming the command that fixes it. What everything else does —
    /// a build should not rewrite a toolchain, and inside a build sandbox it
    /// could not if it tried.
    Refuse,
}

pub async fn ensure_installed(
    host: &dyn ProcessHost,
    product: &QuartusProduct,
    base: &InstallBase,
    version_key: &str,
    stale: StalePatch,
) -> Result<PathBuf, ChipsmithError> {
    let version = product.lookup(version_key)?;
    let dir = product.install_dir_for(base, version);

    if !product.is_installed(base, version) {
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

    if let Some(compat) = runner::host_compat(host, product).await? {
        if !compat.patched(&dir) {
            match stale {
                StalePatch::Repatch => {
                    compat
                        .patch_install(host, &dir, &runner::patch_dirs(product, &dir))
                        .await?
                }
                StalePatch::Refuse => {
                    return Err(ChipsmithError::StaleNixPatch {
                        path: dir,
                        backend: product.backend.to_string(),
                        version: version_key.to_string(),
                    })
                }
            }
        }
    }

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
    use chipsmith_toolchain::process::fake::RecordingHost;

    /// What a nix-built chipsmith carries in place of a `nix eval`.
    const PINNED: &str = r#"{
        "glibc": "/nix/store/new-glibc",
        "patchelf": "/nix/store/patchelf",
        "bash": "/nix/store/new-bash"
    }"#;

    /// An installed Version under a Shared base, with one script in it that an
    /// earlier patch pointed at a bash this chipsmith no longer knows.
    struct Scratch {
        base: InstallBase,
        script: PathBuf,
        root: PathBuf,
    }

    impl Scratch {
        fn new(name: &str) -> Self {
            let root =
                std::env::temp_dir().join(format!("chipsmith-{name}-{}", std::process::id()));
            let bin = root.join("test-product/1.0/quartus/bin");
            std::fs::create_dir_all(&bin).unwrap();
            let script = bin.join("quartus_sh");
            std::fs::write(&script, "#!/nix/store/old-bash/bin/bash -e\nexec true\n").unwrap();
            Self {
                base: InstallBase::Shared(root.clone()),
                script,
                root,
            }
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }

    #[tokio::test]
    async fn a_build_refuses_an_install_patched_against_other_store_paths() {
        let scratch = Scratch::new("refuse");
        let host = RecordingHost::new().with_pinned_compat(PINNED);

        let err = ensure_installed(
            &host,
            &TEST_PRODUCT,
            &scratch.base,
            "1.0",
            StalePatch::Refuse,
        )
        .await
        .unwrap_err();

        assert!(matches!(err, ChipsmithError::StaleNixPatch { .. }), "{err}");
        assert!(
            err.to_string()
                .contains("chipsmith install --backend test-product 1.0"),
            "{err}"
        );
        assert!(host.commands().is_empty(), "nothing may be rewritten");
    }

    /// `chipsmith install` on a Version that is already there is how a user
    /// puts it right, and afterwards a build accepts it.
    #[tokio::test]
    async fn installing_again_repatches_a_stale_install_against_the_pinned_paths() {
        let scratch = Scratch::new("repatch");
        let host = RecordingHost::new().with_pinned_compat(PINNED);

        ensure_installed(
            &host,
            &TEST_PRODUCT,
            &scratch.base,
            "1.0",
            StalePatch::Repatch,
        )
        .await
        .unwrap();

        let script = std::fs::read_to_string(&scratch.script).unwrap();
        assert_eq!(script, "#!/nix/store/new-bash/bin/bash -e\nexec true\n");
        assert!(
            !host.commands().iter().any(|c| c.starts_with("nix eval")),
            "a pinned chipsmith never evaluates <nixpkgs>"
        );

        ensure_installed(
            &host,
            &TEST_PRODUCT,
            &scratch.base,
            "1.0",
            StalePatch::Refuse,
        )
        .await
        .expect("a freshly patched install is accepted");
    }

    #[tokio::test]
    async fn off_nix_an_install_needs_no_patch_record() {
        let scratch = Scratch::new("fhs");
        ensure_installed(
            &RecordingHost::new(),
            &TEST_PRODUCT,
            &scratch.base,
            "1.0",
            StalePatch::Refuse,
        )
        .await
        .unwrap();
    }

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
