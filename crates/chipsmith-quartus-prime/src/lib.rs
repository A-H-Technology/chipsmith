//! The Quartus Prime Lite Product: version table plus the four facts that
//! distinguish it from any other Quartus Product. All behaviour lives in
//! `chipsmith-quartus-common`.

use chipsmith_quartus_common::product::{
    DeviceSupport, InstallerSpec, KnownVersion, QuartusProduct, QuartusVersion, SpawnStrategy,
};

pub static PRODUCT: QuartusProduct = QuartusProduct {
    backend: "quartus-prime",
    display_name: "Quartus Prime",
    install_root: "intelFPGA_lite",
    versions: VERSIONS,
    latest: "23.1",
    installer: InstallerSpec {
        accepts_eula_flag: true,
    },
    spawn: SpawnStrategy::PatchElf,
};

/// Kept as a separate const so the CLI and docs can name the default directly.
pub const LATEST: &str = PRODUCT.latest;

const VERSIONS: &[KnownVersion] = &[
    KnownVersion {
        key: "23.1",
        download: QuartusVersion {
            version: "23.1std.1",
            revision: "993",
            filename: "QuartusLiteSetup-23.1std.1.993-linux.run",
        },
        install_subdir: "23.1std",
        devices: &[
            DeviceSupport {
                family: "cyclonev",
                filename: "cyclonev-23.1std.1.993.qdz",
            },
            DeviceSupport {
                family: "cyclone10lp",
                filename: "cyclone10lp-23.1std.1.993.qdz",
            },
            DeviceSupport {
                family: "cyclone",
                filename: "cyclone-23.1std.1.993.qdz",
            },
            DeviceSupport {
                family: "max10",
                filename: "max10-23.1std.1.993.qdz",
            },
            DeviceSupport {
                family: "max",
                filename: "max-23.1std.1.993.qdz",
            },
        ],
    },
    KnownVersion {
        key: "22.1",
        download: QuartusVersion {
            version: "22.1std.2",
            revision: "922",
            filename: "QuartusLiteSetup-22.1std.2.922-linux.run",
        },
        install_subdir: "22.1std",
        devices: &[DeviceSupport {
            family: "cyclonev",
            filename: "cyclonev-22.1std.2.922.qdz",
        }],
    },
    KnownVersion {
        key: "24.1",
        download: QuartusVersion {
            version: "24.1std",
            revision: "1077",
            filename: "QuartusLiteSetup-24.1std.0.1077-linux.run",
        },
        install_subdir: "24.1std",
        devices: &[DeviceSupport {
            family: "cyclonev",
            filename: "cyclonev-24.1std.0.1077.qdz",
        }],
    },
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lookup_finds_all_versions() {
        assert!(PRODUCT.lookup("23.1").is_ok());
        assert!(PRODUCT.lookup("22.1").is_ok());
        assert!(PRODUCT.lookup("24.1").is_ok());
    }

    #[test]
    fn lookup_rejects_unknown() {
        assert!(PRODUCT.lookup("99.0").is_err());
    }

    #[test]
    fn latest_is_valid() {
        assert!(PRODUCT.lookup(LATEST).is_ok());
    }

    #[test]
    fn install_dir_contains_version() {
        let dir = PRODUCT.install_dir_for(PRODUCT.lookup("23.1").unwrap());
        assert!(dir.ends_with("intelFPGA_lite/23.1std"));
    }

    /// Quartus Prime is 64-bit throughout, so it must never pay for the i686
    /// package set during nix evaluation.
    #[test]
    fn does_not_need_the_32_bit_sandbox() {
        assert_eq!(PRODUCT.spawn, SpawnStrategy::PatchElf);
        assert!(!PRODUCT.spawn.needs_32bit());
    }
}
