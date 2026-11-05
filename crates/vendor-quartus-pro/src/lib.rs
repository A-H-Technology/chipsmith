//! The Quartus Prime Pro Product, for the families Lite never got: Agilex,
//! Stratix 10, Arria 10. All behaviour lives in `chipsmith-quartus-common`.

use chipsmith_quartus_common::product::{
    BundledSimulator, DeviceSupport, InstallerSpec, KnownVersion, QuartusProduct, QuartusVersion,
    SpawnStrategy,
};

pub static PRODUCT: QuartusProduct = QuartusProduct {
    backend: "quartus-pro",
    display_name: "Quartus Prime Pro",
    // Where the installer puts itself since the Altera spin-out; Pro before
    // 24.x used `intelFPGA_pro`.
    install_root: "altera_pro",
    versions: VERSIONS,
    latest: "26.1.1",
    installer: InstallerSpec {
        accepts_eula_flag: true,
    },
    spawn: SpawnStrategy::PatchElf,
    synthesis_tool: "quartus_syn",
    // Pro ships Questa as its own `QuestaSetup-*.run`, which chipsmith does
    // not fetch, so `simulator = "quartus"` finds no `vsim` until it is
    // installed by hand into this directory.
    simulator: BundledSimulator {
        subdir: "questa_fse",
        display_name: "Questa-Altera FPGA Starter Edition",
        needs_license: true,
    },
};

/// Kept as a separate const so the CLI and docs can name the default directly.
pub const LATEST: &str = PRODUCT.latest;

const VERSIONS: &[KnownVersion] = &[KnownVersion {
    // 25.1.1 is the first with programming-file support for the DE25-Nano's
    // A5EB013BB23BE4SCS; 26.1.1 is the newest, and Agilex 5 E-series is
    // licence-free in both (a no-cost node-locked licence file is still
    // required to compile).
    key: "26.1.1",
    download: QuartusVersion {
        version: "26.1.1",
        revision: "130",
        filename: "QuartusProSetup-26.1.1.130-linux.run",
        companions: &["QuartusProSetup-part2-26.1.1.130.qdz"],
    },
    install_subdir: "26.1.1",
    // Pro device archives are rooted at the install directory (`devices/`,
    // `devdata/`), the same as Lite's, so they unzip in place. Agilex 5
    // will not load without the shared Agilex data beside it.
    devices: &[
        DeviceSupport {
            family: "agilex5",
            filename: "agilex5-26.1.1.130.qdz",
        },
        DeviceSupport {
            family: "agilex_common",
            filename: "agilex_common-26.1.1.130.qdz",
        },
    ],
}];

#[cfg(test)]
mod tests {
    use super::*;
    use chipsmith_quartus_common::product::InstallBase;

    #[test]
    fn latest_is_valid() {
        assert!(PRODUCT.lookup(LATEST).is_ok());
    }

    #[test]
    fn installs_where_the_vendor_installer_would() {
        let dir = PRODUCT.install_dir_for(
            &InstallBase::Home("/home/u".into()),
            PRODUCT.lookup("26.1.1").unwrap(),
        );
        assert_eq!(dir, std::path::Path::new("/home/u/altera_pro/26.1.1"));
    }

    /// The `.run` alone installs nothing; it unpacks the rest of Quartus out
    /// of the part2 archive beside it.
    #[test]
    fn the_base_install_brings_its_part2_archive() {
        let v = PRODUCT.lookup("26.1.1").unwrap();
        assert!(v
            .download
            .companions
            .iter()
            .any(|c| c.starts_with("QuartusProSetup-part2-")));
    }

    #[test]
    fn agilex_5_comes_with_the_shared_agilex_data_it_depends_on() {
        let families: Vec<_> = PRODUCT
            .lookup("26.1.1")
            .unwrap()
            .devices
            .iter()
            .map(|d| d.family)
            .collect();
        assert!(families.contains(&"agilex5"), "{families:?}");
        assert!(families.contains(&"agilex_common"), "{families:?}");
    }

    #[test]
    fn is_64_bit_and_never_needs_the_i686_sandbox() {
        assert_eq!(PRODUCT.spawn, SpawnStrategy::PatchElf);
    }
}
