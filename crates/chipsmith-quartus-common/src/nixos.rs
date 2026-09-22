//! Running FHS-assuming vendor binaries on NixOS.
//!
//! One copy for every Quartus Product. The only thing that varies is the Spawn
//! Strategy, which decides whether the i686 package set gets instantiated and
//! whether binaries run under bubblewrap.

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::path::{Path, PathBuf};

use chipsmith_toolchain::error::ChipsmithError;

use crate::product::SpawnStrategy;
use chipsmith_toolchain::process::{Capture, ProcessHost, ProcessSpec};

/// The 64-bit store paths every Product needs.
const BASE_ATTRS: &str = r#"
            glibc = pkgs.glibc.outPath;
            gcc-lib = pkgs.gcc-unwrapped.lib.outPath;
            zlib = pkgs.zlib.outPath;
            patchelf = pkgs.patchelf.outPath;
            bash = pkgs.bash.outPath;
            ncurses = pkgs.ncurses.outPath;
            freetype = pkgs.freetype.outPath;
            fontconfig = pkgs.fontconfig.lib.outPath;
            libxrender = pkgs.xorg.libXrender.outPath;
            libxext = pkgs.xorg.libXext.outPath;
            libx11 = pkgs.xorg.libX11.outPath;
            libxi = pkgs.xorg.libXi.outPath;
            libxtst = pkgs.xorg.libXtst.outPath;
            libxft = pkgs.xorg.libXft.outPath;
            dbus = pkgs.dbus.lib.outPath;
            glib = pkgs.glib.out.outPath;
            libpng = pkgs.libpng.outPath;
            expat = pkgs.expat.outPath;
            libxml2 = pkgs.libxml2.outPath;
            libxcb = pkgs.xorg.libxcb.outPath;
            libxau = pkgs.xorg.libXau.outPath;
            libxdmcp = pkgs.xorg.libXdmcp.outPath;
            libxcrypt-legacy = pkgs.libxcrypt-legacy.outPath;
            libsm = pkgs.xorg.libSM.outPath;
            libice = pkgs.xorg.libICE.outPath;
            krb5 = pkgs.krb5.lib.outPath;
            bzip2 = pkgs.bzip2.out.outPath;
            systemd = pkgs.systemd.outPath;
            libxfixes = pkgs.xorg.libXfixes.outPath;
            libxdamage = pkgs.xorg.libXdamage.outPath;
            libxcomposite = pkgs.xorg.libXcomposite.outPath;
            libxrandr = pkgs.xorg.libXrandr.outPath;
            libxcursor = pkgs.xorg.libXcursor.outPath;
            libxinerama = pkgs.xorg.libXinerama.outPath;
            libuuid = pkgs.util-linux.lib.outPath;
"#;

/// Only `SpawnStrategy::Bubblewrap32` asks for these. Naming `pkgsi686Linux`
/// forces nix to instantiate the whole i686 package set, which is seconds of
/// evaluation a 64-bit-only Product should not pay.
const THIRTY_TWO_BIT_ATTRS: &str = r#"
            glibc32 = pkgs.pkgsi686Linux.glibc.outPath;
            gcc-lib32 = pkgs.pkgsi686Linux.gcc-unwrapped.lib.outPath;
            zlib32 = pkgs.pkgsi686Linux.zlib.outPath;
            bubblewrap = pkgs.bubblewrap.outPath;
"#;

/// The `nix eval` invocation that resolves every store path this Product needs.
fn nix_eval_spec(strategy: SpawnStrategy) -> ProcessSpec {
    let extra = if strategy.needs_32bit() {
        THIRTY_TWO_BIT_ATTRS
    } else {
        ""
    };
    let expr =
        format!("let pkgs = import <nixpkgs> {{}};\n        in {{{BASE_ATTRS}{extra}        }}");

    ProcessSpec::new("nix")
        .args(["eval", "--impure", "--json", "--expr"])
        .arg(expr)
        .capture(Capture::Piped)
}

/// Resolve every required nix store path in a single `nix eval` call.
async fn resolve_nix_paths(
    host: &dyn ProcessHost,
    strategy: SpawnStrategy,
) -> Result<NixPaths, ChipsmithError> {
    let outcome = host.run(nix_eval_spec(strategy)).await?;

    if !outcome.success() {
        return Err(ChipsmithError::NixEvalFailed {
            message: outcome.stderr_tail.unwrap_or_default(),
        });
    }

    // BTreeMap, not HashMap: `lib_paths` iterates this to build LD_LIBRARY_PATH,
    // and Rust's default hasher is seeded per process. With a HashMap, two store
    // paths shipping the same soname would resolve differently run to run.
    let map: BTreeMap<String, String> =
        facet_json::from_slice(&outcome.stdout).map_err(|e| ChipsmithError::NixEvalOutput {
            message: e.to_string(),
        })?;

    Ok(NixPaths { paths: map })
}

struct NixPaths {
    paths: BTreeMap<String, String>,
}

impl NixPaths {
    fn get(&self, key: &str) -> Option<PathBuf> {
        self.paths.get(key).map(PathBuf::from)
    }

    fn dynamic_linker(&self) -> Option<PathBuf> {
        self.get("glibc")
            .map(|p| p.join("lib").join("ld-linux-x86-64.so.2"))
    }

    fn dynamic_linker_32(&self) -> Option<PathBuf> {
        self.get("glibc32")
            .map(|p| p.join("lib").join("ld-linux.so.2"))
    }

    fn patchelf(&self) -> Option<PathBuf> {
        self.get("patchelf").map(|p| p.join("bin").join("patchelf"))
    }

    fn bash(&self) -> Option<PathBuf> {
        self.get("bash").map(|p| p.join("bin").join("bash"))
    }

    fn bubblewrap(&self) -> Option<PathBuf> {
        self.get("bubblewrap").map(|p| p.join("bin").join("bwrap"))
    }

    fn lib_paths(&self) -> Vec<PathBuf> {
        self.paths
            .values()
            .map(|p| PathBuf::from(p).join("lib"))
            .filter(|p| p.exists())
            .collect()
    }
}

fn build_ld_library_path(lib_paths: &[PathBuf]) -> String {
    lib_paths
        .iter()
        .map(|p| p.display().to_string())
        .collect::<Vec<_>>()
        .join(":")
}

fn make_writable(path: &Path) -> Result<(), ChipsmithError> {
    use std::os::unix::fs::PermissionsExt;
    let meta = std::fs::metadata(path)?;
    let mut perms = meta.permissions();
    let mode = perms.mode();
    if mode & 0o200 == 0 {
        perms.set_mode(mode | 0o200);
        std::fs::set_permissions(path, perms)?;
    }
    Ok(())
}

/// Everything needed to run FHS binaries on NixOS.
pub struct NixCompat {
    pub ld_library_path: String,
    strategy: SpawnStrategy,
    bash_path: PathBuf,
    patchelf_bin: PathBuf,
    dynamic_linker: PathBuf,
    dynamic_linker_32: Option<PathBuf>,
    bwrap_bin: Option<PathBuf>,
}

impl NixCompat {
    pub async fn init(
        host: &dyn ProcessHost,
        strategy: SpawnStrategy,
    ) -> Result<Self, ChipsmithError> {
        eprintln!("Resolving NixOS library paths...");
        let paths = resolve_nix_paths(host, strategy).await?;

        let missing = |attr: &str| ChipsmithError::NixPackageMissing {
            attr: attr.to_string(),
        };
        let dynamic_linker = paths.dynamic_linker().ok_or_else(|| missing("glibc"))?;
        let patchelf_bin = paths.patchelf().ok_or_else(|| missing("patchelf"))?;
        let bash_path = paths.bash().ok_or_else(|| missing("bash"))?;

        Ok(Self {
            ld_library_path: build_ld_library_path(&paths.lib_paths()),
            strategy,
            bash_path,
            patchelf_bin,
            dynamic_linker,
            dynamic_linker_32: paths.dynamic_linker_32(),
            bwrap_bin: paths.bubblewrap(),
        })
    }

    /// The argv for running `program` under bubblewrap with a 32-bit loader
    /// mounted at `/lib/ld-linux.so.2`, or `None` if this Product doesn't need
    /// the sandbox or the pieces aren't available.
    ///
    /// Returns argv rather than a `Command` so the sandbox spec can be asserted
    /// on — `Command` gives no way to read its arguments back.
    pub fn bwrap_argv(&self, program: &Path) -> Option<Vec<OsString>> {
        if !self.strategy.needs_32bit() {
            return None;
        }
        let bwrap = self.bwrap_bin.as_ref()?;
        let ld32 = self.dynamic_linker_32.as_ref()?;
        let home = dirs::home_dir().unwrap_or_default();

        Some(vec![
            bwrap.into(),
            "--ro-bind".into(),
            "/".into(),
            "/".into(),
            "--bind".into(),
            "/tmp".into(),
            "/tmp".into(),
            "--bind".into(),
            home.clone().into(),
            home.into(),
            "--tmpfs".into(),
            "/lib".into(),
            "--symlink".into(),
            ld32.into(),
            "/lib/ld-linux.so.2".into(),
            "--dev".into(),
            "/dev".into(),
            "--proc".into(),
            "/proc".into(),
            program.into(),
        ])
    }

    pub async fn patch_elf(
        &self,
        host: &dyn ProcessHost,
        binary: &Path,
    ) -> Result<bool, ChipsmithError> {
        make_writable(binary)?;

        let outcome = host
            .run(
                ProcessSpec::new(&self.patchelf_bin)
                    .arg("--set-interpreter")
                    .arg(&self.dynamic_linker)
                    .arg(binary)
                    .capture(Capture::Null),
            )
            .await?;

        Ok(outcome.success())
    }

    /// Fix `#!/bin/bash` shebangs to point to the nix bash.
    fn patch_shebang(&self, path: &Path, content: &[u8]) -> Result<bool, ChipsmithError> {
        let shebang = b"#!/bin/bash";
        if content.len() < shebang.len() || &content[..shebang.len()] != shebang {
            return Ok(false);
        }

        make_writable(path)?;

        let replacement = format!("#!{}", self.bash_path.display());
        let mut new_content = replacement.into_bytes();
        new_content.extend_from_slice(&content[shebang.len()..]);
        std::fs::write(path, &new_content)?;

        Ok(true)
    }

    /// Patch the ELF binaries and bash scripts sitting directly in `dir`.
    /// Deliberately not recursive: the directories handed to `patch_install`
    /// are flat, and descending into `linux64` would rewrite interpreters on
    /// hundreds of shared objects that never get executed.
    pub async fn patch_dir(
        &self,
        host: &dyn ProcessHost,
        dir: &Path,
    ) -> Result<(u32, u32), ChipsmithError> {
        let mut elfs = 0u32;
        let mut scripts = 0u32;

        let mut entries = tokio::fs::read_dir(dir).await?;
        while let Some(entry) = entries.next_entry().await? {
            let path = entry.path();
            if path.is_dir() {
                continue;
            }

            if let Ok(bytes) = tokio::fs::read(&path).await {
                if bytes.len() > 4 && &bytes[0..4] == b"\x7fELF" {
                    if self.patch_elf(host, &path).await? {
                        elfs += 1;
                    }
                } else if self.patch_shebang(&path, &bytes)? {
                    scripts += 1;
                }
            }
        }

        Ok((elfs, scripts))
    }

    /// Patch all ELF and shell script files in the given directories.
    pub async fn patch_install(
        &self,
        host: &dyn ProcessHost,
        dirs_to_patch: &[PathBuf],
    ) -> Result<(), ChipsmithError> {
        eprintln!("Patching installation for NixOS...");

        let mut total_elfs = 0u32;
        let mut total_scripts = 0u32;

        for dir in dirs_to_patch {
            if dir.exists() {
                let (elfs, scripts) = self.patch_dir(host, dir).await?;
                total_elfs += elfs;
                total_scripts += scripts;
            }
        }

        eprintln!(
            "Patched {} ELF binaries, {} shell scripts",
            total_elfs, total_scripts
        );
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ld_library_path_order_follows_attribute_name() {
        let paths = NixPaths {
            paths: BTreeMap::from([
                ("zlib".to_string(), "/nix/store/z".to_string()),
                ("glibc".to_string(), "/nix/store/g".to_string()),
                ("bash".to_string(), "/nix/store/b".to_string()),
            ]),
        };
        // `lib_paths` filters on existence, so assert the ordering directly.
        let ordered: Vec<_> = paths.paths.keys().cloned().collect();
        assert_eq!(ordered, vec!["bash", "glibc", "zlib"]);
    }

    #[test]
    fn joins_lib_paths_with_colons() {
        let joined = build_ld_library_path(&[PathBuf::from("/a/lib"), PathBuf::from("/b/lib")]);
        assert_eq!(joined, "/a/lib:/b/lib");
    }

    #[test]
    fn only_the_32_bit_expression_names_the_i686_package_set() {
        assert!(!BASE_ATTRS.contains("pkgsi686Linux"));
        assert!(THIRTY_TWO_BIT_ATTRS.contains("pkgsi686Linux"));
        assert!(THIRTY_TWO_BIT_ATTRS.contains("bubblewrap"));
    }

    fn compat(strategy: SpawnStrategy, with_32bit: bool) -> NixCompat {
        NixCompat {
            ld_library_path: String::new(),
            strategy,
            bash_path: PathBuf::from("/nix/bash"),
            patchelf_bin: PathBuf::from("/nix/patchelf"),
            dynamic_linker: PathBuf::from("/nix/ld-linux-x86-64.so.2"),
            dynamic_linker_32: with_32bit.then(|| PathBuf::from("/nix/ld-linux.so.2")),
            bwrap_bin: with_32bit.then(|| PathBuf::from("/nix/bwrap")),
        }
    }

    #[test]
    fn a_64_bit_product_never_sandboxes() {
        let compat = compat(SpawnStrategy::PatchElf, true);
        assert!(compat.bwrap_argv(Path::new("/q/quartus_sh")).is_none());
    }

    #[test]
    fn sandbox_mounts_the_32_bit_loader_where_the_binary_looks_for_it() {
        let compat = compat(SpawnStrategy::Bubblewrap32, true);
        let argv = compat.bwrap_argv(Path::new("/q/quartus_sh")).unwrap();
        let argv: Vec<String> = argv
            .iter()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();

        assert_eq!(argv[0], "/nix/bwrap");
        assert_eq!(argv.last().unwrap(), "/q/quartus_sh");
        let symlink = argv.iter().position(|a| a == "--symlink").unwrap();
        assert_eq!(argv[symlink + 1], "/nix/ld-linux.so.2");
        assert_eq!(argv[symlink + 2], "/lib/ld-linux.so.2");
        // /lib has to be a tmpfs or the symlink can't be created over the host's
        assert!(argv.windows(2).any(|w| w == ["--tmpfs", "/lib"]));
    }

    #[test]
    fn falls_back_to_a_plain_spawn_when_bubblewrap_is_missing() {
        let compat = compat(SpawnStrategy::Bubblewrap32, false);
        assert!(compat.bwrap_argv(Path::new("/q/quartus_sh")).is_none());
    }
}
