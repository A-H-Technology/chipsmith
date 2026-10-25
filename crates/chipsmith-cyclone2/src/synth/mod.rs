//! chipsmith's synthesis for the Cyclone II: a flat RTL netlist (yosys'
//! `proc; flatten` level, or chipsmith's Verilog front end) to the cells
//! cyclone2-re's `synth/cyclone2.ys` produces and [`crate::pack`] reads:
//! `LCELL_COMB`, `LCELL_FF`, `CY2_M4K` and the pin buffers.
//!
//! The device-independent passes are chipsmith-synth's; this module runs
//! them in yosys' order and adds what is the Cyclone II's own: carry
//! chains, the LE register, M4K blocks and pins.

mod arith;
pub mod cells;
mod ff;
mod io;
mod m4k;

use anyhow::{bail, Context, Result};
use chipsmith_netlist::{attr, is_set, CellKind, Design, Module};
use chipsmith_synth::rewrite::define_inits;
use chipsmith_synth::{alumacc, clean, lutmap, memory, opt, techmap, tribuf, undef, wreduce};

/// What each stage left, for checking stages one at a time.
pub trait Observer {
    fn stage(&mut self, name: &str, m: &Module);
}

impl Observer for () {
    fn stage(&mut self, _: &str, _: &Module) {}
}

/// Synthesizes the design's top module (flattened first: instances of the
/// design's own modules are inlined, device cells stay). The result holds
/// that module alone, in Cyclone II cells.
pub fn synthesize(design: Design) -> Result<Design> {
    synthesize_observed(design, &mut ())
}

pub fn synthesize_observed(mut design: Design, obs: &mut dyn Observer) -> Result<Design> {
    chipsmith_synth::flatten::flatten(&mut design)?;
    let mut tops: Vec<Module> = design
        .modules
        .into_iter()
        .filter(|m| is_set(&m.attributes, attr::TOP))
        .collect();
    let mut m = match tops.len() {
        1 => tops.pop().expect("one"),
        n => bail!("expected one top module, found {n}"),
    };
    run(&mut m, obs).with_context(|| format!("synthesizing {}", m.name))?;
    Ok(Design { modules: vec![m] })
}

fn run(m: &mut Module, obs: &mut dyn Observer) -> Result<()> {
    if let Some(c) = m.cells.iter().find(|c| match &c.kind {
        CellKind::Other(g) => !g.ty.starts_with('$') && !is_device_cell(&g.ty),
        _ => false,
    }) {
        bail!(
            "cell {} instantiates {}: the design must be flat",
            c.name,
            c.kind.type_name()
        );
    }
    let clean = |m: &mut Module| clean::clean(m, cells::REMOVABLE);
    let removable = cells::REMOVABLE;
    clean(m);
    tribuf::tribuf(m);
    // Read registers go into memories before power-up values are decided:
    // a block's address register can't hold one.
    memory::collect(m)?;
    // proc's `we ? 1 : 0` per enable bit folded, so a memory written a
    // word at a time has one enable.
    opt::opt_word(m);
    opt::opt_merge(m);
    memory::merge_registers(m, m4k::mappable)?;
    undef::refine(m)?;
    obs.stage("refined", m);
    opt::lower_bitwise(m)?;
    opt::opt_seq(m, removable)?;
    obs.stage("opt", m);
    while wreduce::wreduce(m) {
        opt::opt(m, removable);
    }
    alumacc::alumacc(m);
    opt::opt(m, removable);
    obs.stage("alumacc", m);
    m4k::map(m);
    obs.stage("m4k", m);
    memory::map_to_logic(m, |_| true)?;
    define_inits(m);
    opt::lower_bitwise(m)?;
    opt::opt_seq(m, removable)?;
    obs.stage("coarse", m);

    arith::map(m);
    techmap::lower(m, |k| techmap::is_lowerable(k) || matches!(k, CellKind::Ff(_)))?;
    opt::opt_seq(m, removable)?;
    obs.stage("gates", m);

    ff::map(m)?;
    lutmap::map_module(m)?;
    io::luts(m);
    clean(m);
    obs.stage("luts", m);

    io::pads(m)?;
    clean(m);
    obs.stage("cells", m);
    clean::purge_wires(m);
    Ok(())
}

/// Cells of cyclone2-re's library a design may instantiate itself.
fn is_device_cell(ty: &str) -> bool {
    crate::hard::Block::from_cell_type(ty).is_some()
        || [cells::LCELL_COMB, cells::LCELL_FF, cells::M4K].contains(&ty)
}
