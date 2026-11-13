//! chipsmith's Agilex 5 (DE25-Nano, die sm4revb) back end, moved from
//! agilex5-re's `a5synth`, `a5pnr` and `a5db` (ADR-0003): synthesis of
//! Verilog into a netlist (`synth`), place and route of that netlist into a
//! design file (`pnr`), that design over the idle image into a
//! configuration image and its `.sof` (`rebuild`, `sofgen`), and the `.rbf`
//! the SDM loads from it (`rbf`). It reads agilex5-re's database, the
//! routing graph dumped from Quartus, and the firmware and codebooks of the
//! user's own Quartus Prime Pro install (docs/legal.md).
mod blank;
mod cache;
pub mod db;
mod ddb;
pub mod flow;
pub mod graph;
pub mod jic;
mod pins;
pub mod pnr;
pub mod rbf;
pub mod rebuild;
pub mod route;
pub mod sof;
pub mod sofgen;
pub mod synth;

/// The routing graph at `src` (agilex5-re's graph package) with both
/// loaders' caches added, in `out`: the package's files linked, then
/// `pnr-graph.cache` and `asm-graph.cache` (`crate::cache`).
pub fn graph_cache(src: &std::path::Path, out: &std::path::Path) -> anyhow::Result<()> {
    use anyhow::Context;
    std::fs::create_dir_all(out).with_context(|| format!("creating {}", out.display()))?;
    for e in std::fs::read_dir(src)? {
        let e = e?;
        std::os::unix::fs::symlink(e.path(), out.join(e.file_name()))?;
    }
    pnr::graph::Graph::load(src)?.save_cache(out)?;
    graph::Graph::load(src)?.save_cache(out)
}
