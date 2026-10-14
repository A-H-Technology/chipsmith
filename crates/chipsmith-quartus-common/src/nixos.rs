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
use chipsmith_toolchain::process::{Capture, NixCompatSource, ProcessHost, ProcessSpec};

/// The store paths vendor binaries need, as a nix function of `pkgs`. The
/// flake bakes the same file into a nix-built chipsmith, so the two ways of
/// getting the paths cannot drift.
const COMPAT_NIX: &str = include_str!("../compat.nix");

/// The `nix eval` invocation that resolves every store path this Product needs
/// from the host's `<nixpkgs>`.
fn nix_eval_spec(strategy: SpawnStrategy) -> ProcessSpec {
    let expr = format!(
        "({COMPAT_NIX}) {{ pkgs = import <nixpkgs> {{}}; i686 = {}; }}",
        strategy.needs_32bit()
    );

    ProcessSpec::new("nix")
        .args(["eval", "--impure", "--json", "--expr"])
        .arg(expr)
        .capture(Capture::Piped)
}

fn parse_paths(json: &[u8]) -> Result<NixPaths, ChipsmithError> {
    // BTreeMap, not HashMap: `lib_paths` iterates this to build LD_LIBRARY_PATH,
    // and Rust's default hasher is seeded per process. With a HashMap, two store
    // paths shipping the same soname would resolve differently run to run.
    let map: BTreeMap<String, String> =
        facet_json::from_slice(json).map_err(|e| ChipsmithError::NixEvalOutput {
            message: e.to_string(),
        })?;
    Ok(NixPaths { paths: map })
}

/// Resolve every required nix store path in a single `nix eval` call.
async fn eval_nix_paths(
    host: &dyn ProcessHost,
    strategy: SpawnStrategy,
) -> Result<NixPaths, ChipsmithError> {
    let outcome = host.run(nix_eval_spec(strategy)).await?;

    if !outcome.success() {
        return Err(ChipsmithError::NixEvalFailed {
            message: outcome.stderr_tail.unwrap_or_default(),
        });
    }

    parse_paths(&outcome.stdout)
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

/// One entry directly under `/`, for rebuilding the root inside bubblewrap.
struct RootEntry {
    name: OsString,
    /// Where it points, if it is a symlink.
    link: Option<PathBuf>,
}

/// The host's top-level entries, minus the two the sandbox supplies itself:
/// its own /lib, and a /proc for its own pid namespace.
fn host_root() -> Vec<RootEntry> {
    let Ok(entries) = std::fs::read_dir("/") else {
        return Vec::new();
    };
    let mut root: Vec<RootEntry> = entries
        .flatten()
        .filter(|e| !matches!(e.file_name().to_str(), Some("lib" | "proc")))
        .map(|e| RootEntry {
            link: std::fs::read_link(e.path()).ok(),
            name: e.file_name(),
        })
        .collect();
    // read_dir order is arbitrary; a stable argv is easier to read in an error
    root.sort_by(|a, b| a.name.cmp(&b.name));
    root
}

/// The interpreter of a bash shebang that chipsmith patches, as bytes.
fn bash_shebang(content: &[u8]) -> Option<&[u8]> {
    let rest = content.strip_prefix(b"#!")?;
    let end = rest
        .iter()
        .position(|b| b.is_ascii_whitespace())
        .unwrap_or(rest.len());
    let interpreter = &rest[..end];
    let ours = interpreter == b"/bin/bash"
        || (interpreter.starts_with(b"/nix/store/") && interpreter.ends_with(b"/bin/bash"));
    ours.then_some(interpreter)
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
    /// The compat layer this host needs, or `None` if vendor binaries run on
    /// it as shipped.
    pub async fn for_host(
        host: &dyn ProcessHost,
        strategy: SpawnStrategy,
    ) -> Result<Option<Self>, ChipsmithError> {
        let paths = match host.nix_compat() {
            NixCompatSource::NotNeeded => return Ok(None),
            NixCompatSource::Pinned(json) => parse_paths(json.as_bytes())?,
            NixCompatSource::Channel => {
                eprintln!("Resolving NixOS library paths...");
                eval_nix_paths(host, strategy).await?
            }
        };
        Self::from_paths(&paths, strategy).map(Some)
    }

    fn from_paths(paths: &NixPaths, strategy: SpawnStrategy) -> Result<Self, ChipsmithError> {
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

    /// The bubblewrap argv to put in front of a vendor binary so it finds a
    /// 32-bit loader at `/lib/ld-linux.so.2`, or `None` if this Product doesn't
    /// need the sandbox or the pieces aren't available. Ends where the program
    /// goes: `ProcessSpec::wrapped_in` appends it.
    pub fn bwrap_wrapper(&self) -> Option<Vec<OsString>> {
        if !self.strategy.needs_32bit() {
            return None;
        }
        let bwrap = self.bwrap_bin.as_ref()?;
        let ld32 = self.dynamic_linker_32.as_ref()?;
        let mut argv: Vec<OsString> = vec![bwrap.into()];
        // The host's root rebuilt entry by entry on bubblewrap's own tmpfs,
        // rather than `--bind / /`: a NixOS root has no /lib to mount over,
        // and binding the root itself leaves nowhere to create one.
        //
        // Read-write, and /dev with its devices: the sandbox exists only to
        // put a loader at /lib, not to confine anything. The binary has the
        // user's own access — to an Install Root anywhere on disk, to a build
        // directory outside $HOME, to a JTAG cable.
        for entry in host_root() {
            let target = Path::new("/").join(&entry.name);
            match entry.link {
                Some(points_to) => argv.extend(["--symlink".into(), points_to.into()]),
                None if entry.name == "dev" => {
                    argv.extend(["--dev-bind".into(), target.clone().into()])
                }
                None => argv.extend(["--bind".into(), target.clone().into()]),
            }
            argv.push(target.into());
        }
        argv.extend([
            "--tmpfs".into(),
            "/lib".into(),
            "--symlink".into(),
            ld32.into(),
            "/lib/ld-linux.so.2".into(),
            "--proc".into(),
            "/proc".into(),
        ]);
        Some(argv)
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

    /// Point `#!/bin/bash` shebangs at the nix bash — and ones an earlier
    /// patch already pointed at some other nix bash, so re-patching an install
    /// against a newer nixpkgs leaves no script behind on the old one.
    fn patch_shebang(&self, path: &Path, content: &[u8]) -> Result<bool, ChipsmithError> {
        let Some(interpreter) = bash_shebang(content) else {
            return Ok(false);
        };

        make_writable(path)?;

        let replacement = format!("#!{}", self.bash_path.display());
        let mut new_content = replacement.into_bytes();
        new_content.extend_from_slice(&content[2 + interpreter.len()..]);
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

    /// Patch every ELF and shell script in `dirs_to_patch`, then record in
    /// `install_dir` what they were patched against.
    pub async fn patch_install(
        &self,
        host: &dyn ProcessHost,
        install_dir: &Path,
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

        let stamp = install_dir.join(PATCH_STAMP);
        std::fs::write(&stamp, self.patch_stamp())
            .map_err(ChipsmithError::file("write", &stamp))?;

        eprintln!(
            "Patched {} ELF binaries, {} shell scripts",
            total_elfs, total_scripts
        );
        Ok(())
    }

    /// What a patched binary names by absolute store path. If any of it
    /// changes, the install has to be patched again: an interpreter from one
    /// glibc loading the libc from another dies before `main`, and inside a
    /// build sandbox a path outside chipsmith's closure does not exist at all.
    fn patch_stamp(&self) -> String {
        format!(
            "interpreter={}\nshell={}\n",
            self.dynamic_linker.display(),
            self.bash_path.display()
        )
    }

    /// Whether `install_dir` was last patched against exactly these paths.
    /// An install patched before chipsmith kept a record is not.
    pub fn patched(&self, install_dir: &Path) -> bool {
        std::fs::read_to_string(install_dir.join(PATCH_STAMP))
            .is_ok_and(|stamp| stamp == self.patch_stamp())
    }
}

/// Where `patch_install` records what it patched against.
const PATCH_STAMP: &str = ".chipsmith-nix-patch";

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

    fn eval_expr(strategy: SpawnStrategy) -> String {
        nix_eval_spec(strategy)
            .args
            .last()
            .unwrap()
            .to_string_lossy()
            .into_owned()
    }

    #[test]
    fn only_a_32_bit_product_asks_nix_for_the_i686_package_set() {
        assert!(eval_expr(SpawnStrategy::PatchElf).ends_with("i686 = false; }"));
        assert!(eval_expr(SpawnStrategy::Bubblewrap32).ends_with("i686 = true; }"));
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
        assert!(compat.bwrap_wrapper().is_none());
    }

    #[test]
    fn sandbox_mounts_the_32_bit_loader_where_the_binary_looks_for_it() {
        let compat = compat(SpawnStrategy::Bubblewrap32, true);
        let argv = compat.bwrap_wrapper().unwrap();
        let argv: Vec<String> = argv
            .iter()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();

        assert_eq!(argv[0], "/nix/bwrap");
        assert!(argv
            .windows(3)
            .any(|w| w == ["--symlink", "/nix/ld-linux.so.2", "/lib/ld-linux.so.2"]));
        // /lib has to be a tmpfs or the symlink can't be created over the host's
        assert!(argv.windows(2).any(|w| w == ["--tmpfs", "/lib"]));
        // and the root must not be bound whole, or there is nowhere to create
        // /lib on a host that has none
        assert!(!argv.windows(3).any(|w| w[1] == "/" && w[2] == "/"));
        // while everything else stays writable: an Install Root or a build
        // directory can live anywhere
        assert!(argv.windows(3).any(|w| w == ["--bind", "/etc", "/etc"]));
        assert!(argv.windows(3).any(|w| w == ["--dev-bind", "/dev", "/dev"]));
    }

    #[test]
    fn falls_back_to_a_plain_spawn_when_bubblewrap_is_missing() {
        let compat = compat(SpawnStrategy::Bubblewrap32, false);
        assert!(compat.bwrap_wrapper().is_none());
    }
}
