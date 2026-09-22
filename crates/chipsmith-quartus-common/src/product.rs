//! What distinguishes one Quartus Product from another.
//!
//! Quartus Prime and Quartus II share an install layout, an installer, a tool
//! set and a build flow. They differ in four facts, and those four facts are
//! this module. A backend crate is a `QuartusProduct` constant and nothing else.

use std::path::{Path, PathBuf};

use chipsmith_toolchain::error::ChipsmithError;

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

/// How a Product's binaries have to be launched on NixOS.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SpawnStrategy {
    /// 64-bit binaries: rewrite the ELF interpreter to the nix dynamic linker.
    PatchElf,
    /// 32-bit binaries that dlopen `/lib/ld-linux.so.2` by absolute path, which
    /// patchelf cannot reach. Sandbox with bubblewrap so that path exists.
    /// Falls back to `PatchElf` when bubblewrap is unavailable.
    Bubblewrap32,
}

impl SpawnStrategy {
    /// Whether resolving Nix Compat has to instantiate the i686 package set.
    /// Doing so costs seconds of nix evaluation, so only `Bubblewrap32` asks.
    pub fn needs_32bit(self) -> bool {
        matches!(self, Self::Bubblewrap32)
    }
}

#[derive(Debug)]
pub struct InstallerSpec {
    /// Quartus II 13.0sp1 predates `--accept_eula` and rejects the flag.
    pub accepts_eula_flag: bool,
}

/// The simulator a Product's installer brings with it.
///
/// ModelSim and Questa are the same tool a decade apart and share the whole
/// `vlib`/`vcom`/`vsim` command set, so the only things that vary are where it
/// sits and what it is called.
///
/// Sitting on the Product is very slightly too high: Quartus Prime Lite
/// switched from bundling ModelSim to bundling Questa partway through the
/// line, so which simulator you get is really a fact about the Version. It
/// only matters once a Version predating the switch is in the table — move
/// this to `KnownVersion` then, and an older Prime becomes the licence-free
/// way to run the vendor simulator.
#[derive(Debug)]
pub struct BundledSimulator {
    /// Subdirectory of the Install Root. `modelsim_ase` is the free Starter
    /// Edition; `modelsim_ae` would be the full one, which does check a
    /// licence out.
    pub subdir: &'static str,
    /// What the vendor calls it, for reports and error messages.
    pub display_name: &'static str,
    /// Questa Starter Edition needs a zero-cost node-locked licence; ModelSim
    /// Starter Edition needs none. Intel's own licensing documentation says so
    /// outright: <https://www.intel.com/content/www/us/en/docs/programmable/683472/22-4/and-software-license.html>
    ///
    /// chipsmith cannot obtain a licence, so all it can do is say so when
    /// `vsim` fails.
    pub needs_license: bool,
}

impl BundledSimulator {
    /// Where its executables live. `bin` holds wrapper scripts that exec the
    /// real binaries out of a sibling directory.
    pub fn bin_dir(&self, install_dir: &Path) -> PathBuf {
        install_dir.join(self.subdir).join("bin")
    }

    /// The vendor's own `modelsim.ini`, which carries the `std` and `ieee`
    /// library mappings that chipsmith's generated one chains to.
    pub fn vendor_ini(&self, install_dir: &Path) -> PathBuf {
        install_dir.join(self.subdir).join("modelsim.ini")
    }
}

/// A line of Quartus releases. One per Backend.
#[derive(Debug)]
pub struct QuartusProduct {
    /// The name a Manifest selects this Product with, e.g. `quartus-prime`.
    pub backend: &'static str,
    /// How the Product names itself in progress output, e.g. `Quartus Prime`.
    pub display_name: &'static str,
    /// Install Root under `$HOME`, e.g. `intelFPGA_lite`.
    pub install_root: &'static str,
    pub versions: &'static [KnownVersion],
    pub latest: &'static str,
    pub installer: InstallerSpec,
    pub spawn: SpawnStrategy,
    pub simulator: BundledSimulator,
}

impl QuartusProduct {
    pub fn lookup(&self, key: &str) -> Result<&'static KnownVersion, ChipsmithError> {
        self.versions
            .iter()
            .find(|v| v.key == key)
            .ok_or_else(|| ChipsmithError::UnknownVersion {
                version: key.to_string(),
                available: self.versions.iter().map(|v| v.key.to_string()).collect(),
            })
    }

    pub fn install_dir_for(&self, version: &KnownVersion) -> PathBuf {
        dirs::home_dir()
            .expect("could not determine home directory")
            .join(self.install_root)
            .join(version.install_subdir)
    }

    pub fn is_installed(&self, version: &KnownVersion) -> bool {
        self.install_dir_for(version)
            .join("quartus")
            .join("bin")
            .join("quartus_sh")
            .exists()
    }
}

#[cfg(test)]
pub(crate) static TEST_PRODUCT: QuartusProduct = QuartusProduct {
    backend: "test-product",
    display_name: "Test Product",
    install_root: "testFPGA",
    versions: &[
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
    ],
    latest: "2.0",
    installer: InstallerSpec {
        accepts_eula_flag: true,
    },
    spawn: SpawnStrategy::PatchElf,
    simulator: BundledSimulator {
        subdir: "modelsim_ase",
        display_name: "Test Simulator",
        needs_license: false,
    },
};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lookup_finds_known_version() {
        let v = TEST_PRODUCT.lookup("1.0").unwrap();
        assert_eq!(v.key, "1.0");
        assert_eq!(v.download.revision, "100");
    }

    #[test]
    fn lookup_lists_the_alternatives_it_knows_about() {
        let err = TEST_PRODUCT.lookup("99.0").unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("99.0"));
        assert!(msg.contains("1.0"));
        assert!(msg.contains("2.0"));
    }

    #[test]
    fn latest_is_always_a_version_the_product_knows() {
        assert!(TEST_PRODUCT.lookup(TEST_PRODUCT.latest).is_ok());
    }

    #[test]
    fn install_dir_combines_install_root_and_subdir() {
        let dir = TEST_PRODUCT.install_dir_for(TEST_PRODUCT.lookup("1.0").unwrap());
        assert!(dir.ends_with("testFPGA/1.0"), "{}", dir.display());
    }

    #[test]
    fn only_the_32_bit_strategy_pays_for_the_i686_package_set() {
        assert!(!SpawnStrategy::PatchElf.needs_32bit());
        assert!(SpawnStrategy::Bubblewrap32.needs_32bit());
    }
}
