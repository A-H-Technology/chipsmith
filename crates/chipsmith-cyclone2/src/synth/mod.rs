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

use std::collections::BTreeMap;

use anyhow::{bail, Context, Result};
use chipsmith_netlist::{attr, is_set, CellKind, Design, Module};
use chipsmith_synth::rewrite::define_inits;
use chipsmith_synth::{
    alumacc, clean, divmod, fsm, lutmap, memory, opt, techmap, tribuf, undef, wreduce,
};

use crate::pin::IoRegisters;

/// What each stage left, for checking stages one at a time.
pub trait Observer {
    fn stage(&mut self, name: &str, m: &Module);
}

impl Observer for () {
    fn stage(&mut self, _: &str, _: &Module) {}
}

/// What synthesis made, for the build's log.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Stats {
    /// The LUT mapping: gates in, LUTs out, their depth.
    pub lutmap: lutmap::Stats,
    pub registers: usize,
    /// Latches, each a LUT holding its own output (power-up undefined on
    /// the hardware, as Quartus warns).
    pub latches: usize,
    /// Registers with an asynchronous load or set, emulated with a latch
    /// (counted in `registers` too).
    pub emulated: usize,
    /// Registers moved into pins' I/O cells (copied, for an output or OE
    /// register whose original something else reads).
    pub io_registers: usize,
    /// Carry chains (adders, subtractors, comparisons).
    pub chains: usize,
    pub m4ks: usize,
}

/// A synthesized design: its top module alone, in Cyclone II cells.
pub struct Synthesis {
    pub design: Design,
    pub stats: Stats,
}

/// Synthesizes the design's top module (flattened first: instances of the
/// design's own modules are inlined, device cells stay). `io_registers`
/// names, per port bit (`LEDR[0]`), the I/O-cell registers to fill.
pub fn synthesize(
    design: Design,
    io_registers: &BTreeMap<String, IoRegisters>,
) -> Result<Synthesis> {
    synthesize_observed(design, io_registers, &mut ())
}

pub fn synthesize_observed(
    mut design: Design,
    io_registers: &BTreeMap<String, IoRegisters>,
    obs: &mut dyn Observer,
) -> Result<Synthesis> {
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
    let stats =
        run(&mut m, io_registers, obs).with_context(|| format!("synthesizing {}", m.name))?;
    Ok(Synthesis {
        design: Design { modules: vec![m] },
        stats,
    })
}

fn run(
    m: &mut Module,
    io_registers: &BTreeMap<String, IoRegisters>,
    obs: &mut dyn Observer,
) -> Result<Stats> {
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
    // A memory declared as logic keeps its registers where they are.
    let logic: std::collections::HashSet<String> = m
        .cells
        .iter()
        .filter_map(|c| match &c.kind {
            CellKind::Mem(mem) if m4k::wants_logic(&c.attributes) => Some(mem.memid.clone()),
            _ => None,
        })
        .collect();
    memory::merge_registers(m, |mem| !logic.contains(&mem.memid) && m4k::mappable(mem))?;
    undef::refine(m)?;
    obs.stage("refined", m);
    fsm::compare_states(m);
    opt::lower_bitwise(m)?;
    opt::opt_seq(m, removable)?;
    obs.stage("opt", m);
    // Before wreduce and alumacc: its stages are subtractions and
    // comparisons, which they narrow and put on carry chains.
    if divmod::lower(m) {
        opt::opt(m, removable);
    }
    while wreduce::wreduce(m) {
        opt::opt(m, removable);
    }
    alumacc::alumacc(m);
    opt::opt(m, removable);
    obs.stage("alumacc", m);
    let m4ks = m4k::map(m);
    obs.stage("m4k", m);
    memory::map_to_logic(m)?;
    define_inits(m);
    opt::lower_bitwise(m)?;
    opt::opt_seq(m, removable)?;
    obs.stage("coarse", m);

    let chains = arith::map(m);
    techmap::lower(m, |k| {
        techmap::is_lowerable(k) || matches!(k, CellKind::Ff(_))
    })?;
    opt::opt_seq(m, removable)?;
    obs.stage("gates", m);

    let io_registers = io::registers(m, io_registers)?;
    let ff::Counts {
        registers,
        latches,
        emulated,
    } = ff::map(m)?;
    let lutmap = lutmap::map_module(m)?;
    io::luts(m);
    clean(m);
    obs.stage("luts", m);

    io::pads(m)?;
    clean(m);
    obs.stage("cells", m);
    clean::purge_wires(m);
    Ok(Stats {
        lutmap,
        registers,
        latches,
        emulated,
        io_registers,
        chains,
        m4ks,
    })
}

/// Device cells ([`cells::LIBRARY`]) a design may instantiate itself.
fn is_device_cell(ty: &str) -> bool {
    crate::hard::Block::from_cell_type(ty).is_some()
        || [cells::LCELL_COMB, cells::LCELL_FF, cells::M4K].contains(&ty)
}
