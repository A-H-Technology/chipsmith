//! chipsmith's synthesis for the 7-series: a flat RTL netlist (chipsmith's
//! Verilog front end, yosys' `proc; flatten` level) to the primitives yosys'
//! `synth_xilinx` ends in: LUT1 to LUT6, CARRY4, FDRE/FDSE/FDCE/FDPE,
//! RAM32M, RAMB18E1/RAMB36E1, DSP48E1 and the pin and clock buffers
//! ([`cells`]).
//!
//! The device-independent passes are chipsmith-synth's, run in the order
//! the Cyclone II's synthesis runs them; what is the 7-series' own is the
//! carry chain, the slice register, distributed and block RAM and the
//! buffers, and LUT mapping with six inputs instead of four.

mod arith;
mod bram;
pub mod cells;
mod dsp;
mod ff;
mod io;
mod ram;
mod replicate;

use anyhow::{bail, Context, Result};
use chipsmith_netlist::{attr, is_set, CellKind, Design, Module, SigBit, State};
use chipsmith_synth::lutmap::{self, LutSize};
use chipsmith_synth::rewrite::{define_inits, Subst};
use chipsmith_synth::{alumacc, clean, divmod, fsm, memory, opt, techmap, tribuf, undef, wreduce};

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
    /// Copies of registers read by many cells, among `registers`.
    pub replicas: usize,
    pub carry4s: usize,
    pub ram32ms: usize,
    /// Block RAM in RAMB18 halves (a RAMB36E1 is two).
    pub bram_halves: usize,
    pub dsps: usize,
    pub bufgs: usize,
}

/// Choices synthesis leaves open.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Options {
    /// Small memories in RAM32Ms; else in registers and multiplexers.
    pub distributed_ram: bool,
    /// Memories with synchronous reads in block RAM; else as above.
    pub block_ram: bool,
    /// Registers read by many cells copied ([`replicate`]). yosys'
    /// equivalence proof can't tie a copy to its original (induction leaves
    /// the copy free), so the proofs run without.
    pub register_copies: bool,
}

impl Default for Options {
    fn default() -> Self {
        Options {
            distributed_ram: true,
            block_ram: true,
            register_copies: true,
        }
    }
}

/// A synthesized design: its top module alone, in 7-series primitives.
pub struct Synthesis {
    pub design: Design,
    pub stats: Stats,
}

/// Synthesizes the design's top module (flattened first: instances of the
/// design's own modules are inlined, device cells stay).
pub fn synthesize(design: Design) -> Result<Synthesis> {
    synthesize_observed(design, Options::default(), &mut ())
}

pub fn synthesize_observed(
    mut design: Design,
    options: Options,
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
    let stats = run(&mut m, options, obs).with_context(|| format!("synthesizing {}", m.name))?;
    Ok(Synthesis {
        design: Design { modules: vec![m] },
        stats,
    })
}

fn run(m: &mut Module, options: Options, obs: &mut dyn Observer) -> Result<Stats> {
    if let Some(c) = m.cells.iter().find(|c| match &c.kind {
        CellKind::Other(g) => !g.ty.starts_with('$') && !cells::is_device_cell(&g.ty),
        _ => false,
    }) {
        bail!(
            "cell {} instantiates {}: the design must be flat",
            c.name,
            c.kind.type_name()
        );
    }
    let removable = cells::REMOVABLE;
    let clean = |m: &mut Module| clean::clean(m, removable);
    clean(m);
    tribuf::tribuf(m);
    memory::collect(m)?;
    opt::opt_word(m);
    opt::opt_merge(m);
    // Read registers into the memories block RAM takes (its read is
    // synchronous); a memory declared as logic keeps its registers.
    if options.block_ram {
        let logic: std::collections::HashSet<String> = m
            .cells
            .iter()
            .filter_map(|c| match &c.kind {
                CellKind::Mem(mem) if bram::wants_logic(&c.attributes) => Some(mem.memid.clone()),
                _ => None,
            })
            .collect();
        memory::merge_registers(m, |mem| !logic.contains(&mem.memid) && bram::mappable(mem))?;
    }
    undef::refine(m)?;
    obs.stage("refined", m);
    fsm::compare_states(m);
    opt::lower_bitwise(m)?;
    opt::opt_seq(m, removable)?;
    obs.stage("opt", m);
    if divmod::lower(m) {
        opt::opt(m, removable);
    }
    while wreduce::wreduce(m) {
        opt::opt(m, removable);
    }
    obs.stage("wreduce", m);
    // Before alumacc: a product split over DSPs sums its partial products
    // with adders, which become carry chains like any other.
    let dsps = dsp::map(m);
    obs.stage("dsp", m);
    // Sums by arrival, as the Cyclone II's (sha256 by Vivado: README,
    // since branch xc7-qor).
    alumacc::alumacc_with(m, alumacc::SumOrder::Arrival);
    opt::opt(m, removable);
    obs.stage("alumacc", m);
    let bram_halves = if options.block_ram { bram::map(m) } else { 0 };
    if bram_halves > 0 {
        opt::opt(m, removable);
    }
    let ram32ms = if options.distributed_ram {
        ram::map(m)
    } else {
        0
    };
    memory::map_to_logic(m)?;
    define_inits(m);
    opt::lower_bitwise(m)?;
    opt::opt_seq(m, removable)?;
    obs.stage("coarse", m);

    let carry4s = arith::map(m);
    techmap::lower(m, |k| {
        techmap::is_lowerable(k) || matches!(k, CellKind::Ff(_))
    })?;
    opt::opt_seq(m, removable)?;
    obs.stage("gates", m);

    let registers = ff::map(m)?;
    io::pads(m)?;
    let lutmap = lutmap::map_module_k(m, LutSize::SIX)?;
    lut_cells(m);
    clean(m);
    obs.stage("luts", m);
    let replicas = if options.register_copies { replicate::registers(m, replicate::MAX_FANOUT) } else { 0 };
    let bufgs = io::clock_buffers(m);
    clean(m);
    obs.stage("cells", m);
    clean::purge_wires(m);
    Ok(Stats {
        lutmap,
        registers: registers + replicas,
        replicas,
        carry4s,
        ram32ms,
        bram_halves,
        dsps,
        bufgs,
    })
}

/// `$lut` cells as LUT1 to LUT6, constant inputs folded into the table; a
/// LUT left with no input is its constant, and LUTs with the same inputs
/// and table are one. The mapper writes a LUT per gate output it maps
/// (every register's inverted active-low reset its own LUT1, on sha256
/// 1,817 of them): merged here, the way yosys' `opt_merge` would.
fn lut_cells(m: &mut Module) {
    let cells = std::mem::take(&mut m.cells);
    let mut subst = Subst::new(m);
    let mut seen: std::collections::HashMap<(Vec<SigBit>, u64), SigBit> =
        std::collections::HashMap::new();
    for c in cells {
        let CellKind::Lut(lut) = c.kind else {
            m.cells.push(c);
            continue;
        };
        let mut table: u64 = (0..lut.table.0.len())
            .filter(|&i| lut.table.0[i] == State::One)
            .fold(0, |t, i| t | 1 << i);
        let mut inputs: Vec<SigBit> = Vec::new();
        let mut width = lut.a.len();
        // Each input in turn: kept, or folded (a constant, or a repeat of
        // an input already kept).
        let mut i = 0;
        for &a in &lut.a {
            let fold = match a {
                SigBit::Const(s) => Some((None, s == State::One)),
                SigBit::Net(_) => inputs.iter().position(|&k| k == a).map(|k| (Some(k), false)),
            };
            match fold {
                None => {
                    inputs.push(a);
                    i += 1;
                }
                Some((same_as, v)) => {
                    table = cofactor(table, width, i, same_as, v);
                    width -= 1;
                }
            }
        }
        debug_assert_eq!(width, inputs.len());
        if inputs.is_empty() {
            if let SigBit::Net(y) = lut.y {
                subst.set(y, SigBit::Const(State::from(table & 1 == 1)));
            }
            continue;
        }
        match seen.entry((inputs, table)) {
            std::collections::hash_map::Entry::Occupied(e) => {
                if let SigBit::Net(y) = lut.y {
                    subst.set(y, *e.get());
                }
            }
            std::collections::hash_map::Entry::Vacant(e) => {
                m.add_cell(c.name, cells::lut(&e.key().0, table, lut.y));
                e.insert(lut.y);
            }
        }
    }
    subst.apply(m);
}

/// The `width`-input table with input `i` removed: fixed at `v`, or equal
/// to input `same_as` (`< i`).
fn cofactor(table: u64, width: usize, i: usize, same_as: Option<usize>, v: bool) -> u64 {
    let mut out = 0u64;
    for idx in 0..1u64 << (width - 1) {
        let low = idx & ((1 << i) - 1);
        let high = (idx >> i) << (i + 1);
        let bit = match same_as {
            Some(k) => idx >> k & 1,
            None => u64::from(v),
        };
        out |= (table >> (low | bit << i | high) & 1) << idx;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cofactors_fold_constants_and_repeats() {
        // a & b with b = 1: a.
        assert_eq!(cofactor(0b1000, 2, 1, None, true), 0b10);
        // a & b with b = 0: 0.
        assert_eq!(cofactor(0b1000, 2, 1, None, false), 0b00);
        // a ^ b with b = a: 0.
        assert_eq!(cofactor(0b0110, 2, 1, Some(0), false), 0b00);
        // (a & c) | b over (a, b, c), c = a: a | b.
        let t = (0..8u64).fold(0, |t, x| {
            let (a, b, c) = (x & 1, x >> 1 & 1, x >> 2 & 1);
            t | ((a & c) | b) << x
        });
        assert_eq!(cofactor(t, 3, 2, Some(0), false), 0b1110);
    }
}
