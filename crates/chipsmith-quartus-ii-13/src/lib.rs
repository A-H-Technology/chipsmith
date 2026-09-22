//! The Quartus II 13.0sp1 Product — the last release that supports the older
//! Cyclone and MAX families. All behaviour lives in `chipsmith-quartus-common`.

use chipsmith_quartus_common::product::{
    BundledSimulator, DeviceSupport, InstallerSpec, KnownVersion, QuartusProduct, QuartusVersion,
    SpawnStrategy,
};

pub static PRODUCT: QuartusProduct = QuartusProduct {
    backend: "quartus-ii-13",
    display_name: "Quartus II",
    install_root: "altera",
    versions: VERSIONS,
    latest: "13.0sp1",
    // The 13.0sp1 installer and tools are 32-bit binaries that load
    // /lib/ld-linux.so.2 by absolute path, which patchelf cannot redirect.
    installer: InstallerSpec {
        accepts_eula_flag: false,
    },
    spawn: SpawnStrategy::Bubblewrap32,
    // 32-bit like the rest of 13.0sp1, and the last Starter Edition that
    // needed no licence at all.
    simulator: BundledSimulator {
        subdir: "modelsim_ase",
        display_name: "ModelSim-Altera Starter Edition",
        needs_license: false,
    },
};

/// Kept as a separate const so the CLI and docs can name the default directly.
pub const LATEST: &str = PRODUCT.latest;

const VERSIONS: &[KnownVersion] = &[KnownVersion {
    key: "13.0sp1",
    download: QuartusVersion {
        version: "13.0sp1",
        revision: "232",
        filename: "QuartusSetupWeb-13.0.1.232.run",
    },
    install_subdir: "13.0sp1",
    devices: &[
        DeviceSupport {
            family: "cyclone",
            filename: "cyclone_web-13.0.1.232.qdz",
        },
        DeviceSupport {
            family: "max",
            filename: "max_web-13.0.1.232.qdz",
        },
    ],
}];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lookup_finds_known_version() {
        assert!(PRODUCT.lookup("13.0sp1").is_ok());
    }

    #[test]
    fn lookup_rejects_unknown() {
        assert!(PRODUCT.lookup("14.0").is_err());
    }

    #[test]
    fn latest_is_valid() {
        assert!(PRODUCT.lookup(LATEST).is_ok());
    }

    #[test]
    fn install_dir_contains_version() {
        let dir = PRODUCT.install_dir_for(PRODUCT.lookup("13.0sp1").unwrap());
        assert!(dir.ends_with("altera/13.0sp1"));
    }

    /// The two facts that made this a separate crate in the first place.
    #[test]
    fn needs_the_32_bit_sandbox_and_rejects_the_eula_flag() {
        assert!(PRODUCT.spawn.needs_32bit());
        assert!(!PRODUCT.installer.accepts_eula_flag);
    }
}
