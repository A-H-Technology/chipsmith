//! The Cyclone II bitstream: a routed design (nextpnr's JSON, which
//! `route.rs` writes) to an RBF, and an RBF to the JTAG stream that loads
//! it. This is cyclone2-re's `cram pack` and `cram svf`, moved here
//! (ADR-0003); cyclone2-re's `cram` now calls this module, so its
//! byte-identical checks against Quartus guard this one implementation.
//!
//! Device facts come from cyclone2-re's shipped database (`db`); the rest
//! of the module turns them into CRAM: `phys` is the design in device
//! terms, `logic`, `io`, `m4k` and `hard` its blocks, `addr` and `rbf`
//! where their bits land. `nextpnr` reads a routed design into `phys`;
//! cyclone2-re has the other frontend, Quartus' post-fit netlist and
//! routing, for its checks.
//!
//! Where a comment cites the README, fuzz designs, `specimens/` or
//! `tools/`, it means cyclone2-re's: that is where the evidence for each
//! fact lives.

pub mod addr;
pub mod asmdb;
pub mod blank;
pub mod db;
pub mod device;
pub mod hard;
pub mod io;
pub mod jtag;
pub mod lab;
pub mod le;
pub mod logic;
pub mod m4k;
pub mod nextpnr;
pub mod phys;
pub mod rbf;

use std::collections::BTreeMap;
use std::path::Path;

use anyhow::{ensure, Context, Result};

use phys::{Bits, Db, PhysDesign, Source};
use rbf::Rbf;

/// The shipped database under `CHIPSMITH_CYCLONE2_DB` (`route::DB_VAR`).
pub fn load_db() -> Result<Db> {
    let root = std::env::var_os(crate::route::DB_VAR).with_context(|| {
        format!(
            "{} is not set; run chipsmith from its nix package or `nix develop`",
            crate::route::DB_VAR
        )
    })?;
    Db::load(&Path::new(&root).join(db::DEVICE))
}

/// `base` with every bit `design` fixes written over it and its check
/// words recomputed. Also returns the bits, for reporting.
pub fn assemble(db: &Db, mut base: Rbf, design: &PhysDesign) -> Result<(Rbf, Bits)> {
    let bits = design.bits(db)?;
    ensure!(
        bits.conflicts().is_empty(),
        "model sets {} bits both ways",
        bits.conflicts().len()
    );
    for &(b, v, _) in &bits.core {
        base.set(b, v);
    }
    for &(b, v) in &bits.rep {
        base.rep_set(b, v);
    }
    for &(byte, bit) in &bits.options {
        base.set_option(byte, bit);
    }
    base.fix_crcs();
    Ok((base, bits))
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Stats {
    /// Core bits the model set, by where they came from.
    pub bits: BTreeMap<Source, usize>,
    /// I/O bits in the replicated frames.
    pub rep_bits: usize,
    pub les: usize,
    pub pins: usize,
    pub nets: usize,
}

/// The routed design at `routed` as an RBF at `out`, on the base image
/// built from the database alone.
pub fn pack_file(db: &Db, routed: &Path, out: &Path) -> Result<Stats> {
    let design = nextpnr::read(&db.dev, routed)?.to_phys(&db.dev, &db.io)?;
    let base = blank::image(&db.asmdb, &db.io)?;
    let (rbf, bits) = assemble(db, base, &design)?;
    std::fs::write(out, rbf.bytes()).with_context(|| format!("writing {}", out.display()))?;
    let mut by_source = BTreeMap::new();
    for &(_, _, s) in &bits.core {
        *by_source.entry(s).or_default() += 1;
    }
    Ok(Stats {
        bits: by_source,
        rep_bits: bits.rep.len(),
        les: design.logic.les.len(),
        pins: design.pins.len(),
        nets: design.nets.len(),
    })
}

/// The JTAG configuration (an SVF) of the RBF at `rbf`, written to `svf`.
pub fn svf_file(rbf: &Path, svf: &Path) -> Result<()> {
    std::fs::write(svf, jtag::svf(&Rbf::read(rbf)?))
        .with_context(|| format!("writing {}", svf.display()))
}
