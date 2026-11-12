//! What a DE25-Nano build needs around the stages: where the data is, a
//! placed and routed design assembled into the HPS-first core, and the HPS
//! IO hash a core carries.
//!
//! The core is phase 2 of an HPS-first boot: the HPS has already been
//! configured from phase 1, and the SDM takes the core only if its HPS IO
//! hash is phase 1's. With any other it wedges until phase 1 is loaded
//! again (agilex5-re README, "A wrong HPS IO hash wedges the SDM").

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

use crate::{rbf, rebuild, route, sof::Sof, sofgen};

/// The DE25-Nano's data: none of it is chipsmith's own.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Data {
    /// agilex5-re's `db` package: `db/sm4revb` and `keys/`.
    pub db: PathBuf,
    /// The routing graph agilex5-re dumps from Quartus (`specimens/rg/full`).
    pub graph: PathBuf,
    /// A Quartus Prime Pro 26.1.1 install, for the SDM firmware and the
    /// codebooks (docs/legal.md).
    pub quartus: PathBuf,
}

impl Data {
    pub fn chip(&self) -> PathBuf {
        self.db.join("db/sm4revb")
    }
    pub fn delays(&self) -> PathBuf {
        self.chip().join("timing/delays.tsv")
    }
}

/// A design file (place and route's, with the board's HPS shell lines) into
/// the `.sof` at `sof` and the core.rbf the SDM loads after the phase 1
/// whose HPS IO hash is `hps_io_hash`.
pub fn assemble(data: &Data, design: &str, hps_io_hash: [u8; 32], sof: &Path) -> Result<Vec<u8>> {
    let path = |p: PathBuf| {
        p.to_str()
            .map(str::to_owned)
            .with_context(|| format!("{} is not UTF-8", p.display()))
    };
    let (mut db, graph) = route::open(&path(data.chip())?, &path(data.graph.clone())?)?;
    let image = rebuild::build(&mut db, &graph, design)?;
    let install = rbf::install::Install::open(&data.quartus)?;
    let key = |name: &str| {
        std::fs::read(data.db.join("keys").join(name))
            .with_context(|| format!("reading keys/{name}"))
    };
    let sof_path = path(sof.to_path_buf())?;
    Sof::from_raw(sofgen::build(&install, &image)?)?.write(&sof_path, &key("sof_hmac.key")?)?;
    let sof = Sof::read(&sof_path)?;
    rbf::write(
        &mut db,
        &install,
        &sof,
        &key("rbf_keyed_hash.key")?,
        Some(&rbf::Phase1::HpsIoHash(hps_io_hash)),
    )
}

/// The HPS IO hash an `.rbf` carries at +0x374 of its IO section, or None
/// if it has no IO section (or isn't an Agilex 5 `.rbf`).
pub fn hps_io_hash(rbf: &[u8]) -> Option<[u8; 32]> {
    const SECTION_MAGIC: u32 = 0x9756_6593;
    let u32_at = |o: usize| Some(u32::from_le_bytes(rbf.get(o..o + 4)?.try_into().ok()?));
    let mut base = 0;
    while base < rbf.len() {
        // The first (CMF) section's header block starts at +0x400.
        let h = base + if base == 0 { 0x400 } else { 0 };
        if u32_at(h)? != SECTION_MAGIC {
            return None;
        }
        if rbf.get(h + 12..h + 16)? == b"IO\0\0" {
            return rbf.get(base + 0x374..base + 0x374 + 32)?.try_into().ok();
        }
        let size = u32_at(h + 8)? as usize;
        if size == 0 {
            return None;
        }
        base += size;
    }
    None
}

/// Is this file an Agilex 5 `.rbf`: an SDM section header where the first
/// section's is?
pub fn is_rbf(file: &[u8]) -> bool {
    file.get(0x400..0x404) == Some(&0x9756_6593u32.to_le_bytes()[..])
}
