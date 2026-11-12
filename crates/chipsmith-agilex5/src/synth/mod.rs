//! Synthesis for the Agilex 5 (agilex5-re's `a5synth`, README (z12)): the
//! cells `pnr` reads, from chipsmith's Verilog front end and its
//! device-independent passes in the order the Cyclone II synthesis runs
//! them, with the LUT mapper at K = 6 (ADR-0013): `$lut` of up to six
//! inputs, `A5_ARITH` carry chains ([`arith`]), flops with the LAB's
//! controls ([`ff`]), memories that fit as `$__A5_MLAB_` RAM ([`ram`]),
//! products in DSP blocks ([`mult`]), and the hand-instantiated `A5_M20K`
//! and `A5_IOPLL` as they are. The netlist is written as yosys' JSON, which
//! `pnr` reads.

mod arith;
mod ff;
mod mult;
mod ram;

use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use chipsmith_netlist::{attr, is_set, CellKind, Const, Design, Lut, Module, SigBit, State};
use chipsmith_synth::lutmap::{self, LutCost, LutSize, MapOptions};
use chipsmith_synth::rewrite::{define_inits, Subst};
use chipsmith_synth::{
    alumacc, clean, divmod, fsm, memory, opt, restructure, rom, techmap, tribuf, undef, wreduce,
};

/// pnr's cells as Verilog blackboxes, for the front end.
const LIBRARY: &str = include_str!("cells.v");
/// Cells that are pure functions of their inputs: unused ones go.
const REMOVABLE: &[&str] = &[arith::ARITH, ram::MLAB, mult::MUL];

pub struct Options {
    /// Memories that fit as MLAB RAM; else they stay flops.
    pub mlab: bool,
    /// Products of up to 27 x 27 bits in DSP blocks; else LUTs.
    pub dsp: bool,
    /// The LUT mapper's cost by input count.
    pub lut_cost: LutCost,
}

impl Default for Options {
    fn default() -> Options {
        Options {
            mlab: true,
            dsp: true,
            lut_cost: LutCost::by_inputs(ALM_COST).expect("no cost of 0"),
        }
    }
}

/// Synthesize `top` from `sources` and write the netlist to `out`.
/// Returns every file the front end read (the sources and what they
/// include).
pub fn run(top: &str, sources: &[&Path], out: &Path, opts: &Options) -> Result<Vec<PathBuf>> {
    let t0 = std::time::Instant::now();
    let mut fe = chipsmith_verilog::Frontend::new();
    fe.read_library_source("a5pnr/yosys/cells.v", LIBRARY)?;
    for p in sources {
        if let Some(d) = p.parent() {
            fe.include_dir(d);
        }
        fe.read_file(p)?;
    }
    let mut design = fe.elaborate(top)?;
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
    let s = passes(&mut m, opts).with_context(|| format!("synthesizing {top}"))?;
    let file = std::fs::File::create(out).with_context(|| format!("creating {}", out.display()))?;
    chipsmith_netlist::write_json(&Design { modules: vec![m] }, file)?;
    eprintln!(
        "synth: {} gates to {} LUTs (depth {}), {} flops ({} inverted), {} carry halves in {} chains, {} MLAB RAM bits, {} multipliers; {:.1} s",
        s.lutmap.gates,
        s.luts,
        s.lutmap.depth,
        s.ffs.flops,
        s.ffs.inverted,
        s.arith.halves,
        s.arith.chains,
        s.mlab_bits,
        s.dsps,
        t0.elapsed().as_secs_f64()
    );
    Ok(fe.files_read())
}

struct Stats {
    lutmap: lutmap::Stats,
    luts: usize,
    ffs: ff::Counts,
    arith: arith::Counts,
    mlab_bits: usize,
    dsps: usize,
}

fn passes(m: &mut Module, opts: &Options) -> Result<Stats> {
    if let Some(c) = m.cells.iter().find(|c| match &c.kind {
        CellKind::Other(g) => {
            !g.ty.starts_with('$') && !LIBRARY.contains(&format!("module {} ", g.ty))
        }
        _ => false,
    }) {
        bail!(
            "cell {} instantiates {}: the design must be flat",
            c.name,
            c.kind.type_name()
        );
    }
    let clean = |m: &mut Module| clean::clean(m, REMOVABLE);
    clean(m);
    tribuf::tribuf(m);
    // Read registers stay where the RTL has them: an MLAB reads with no
    // clock (README (z9), as synth.sh's -nordff).
    memory::collect(m)?;
    opt::opt_word(m);
    opt::opt_merge(m);
    undef::refine(m)?;
    rom::tables(m);
    fsm::compare_states(m);
    opt::lower_bitwise(m)?;
    opt::opt_seq(m, REMOVABLE)?;
    if divmod::lower(m) {
        opt::opt(m, REMOVABLE);
    }
    while wreduce::wreduce(m) {
        opt::opt(m, REMOVABLE);
    }
    // Before alumacc, which would fold products into adder trees.
    let dsps = if opts.dsp { mult::map(m) } else { 0 };
    alumacc::alumacc(m);
    opt::opt(m, REMOVABLE);
    let mlab_bits = if opts.mlab { ram::map(m) } else { 0 };
    memory::map_to_logic(m)?;
    define_inits(m);
    opt::lower_bitwise(m)?;
    opt::opt_seq(m, REMOVABLE)?;
    restructure::restructure(m);
    arith::fold_carry_in(m);
    let arith = arith::map(m);
    techmap::lower(m, |k| {
        techmap::is_lowerable(k) || matches!(k, CellKind::Ff(_))
    })?;
    opt::opt_seq(m, REMOVABLE)?;
    let ffs = ff::map(m)?;
    let lutmap = lutmap::map_module_with(
        m,
        LutSize::SIX,
        MapOptions {
            cost: opts.lut_cost,
            keep_best_cut: true,
            restructure: false,
        },
    )?;
    let luts = merge_luts(m);
    ff::fold_constants(m)?;
    // The RTL's wire names stay (no `purge_wires`): simcheck lines up the
    // image's flops with the RTL's registers by them.
    clean(m);
    Ok(Stats {
        lutmap,
        luts,
        ffs,
        arith,
        mlab_bits,
        dsps,
    })
}

/// What a LUT costs by input count, in thirds of an ALM: a 6-input LUT the
/// whole ALM; one of up to four inputs half of it, since any two pair; a
/// 5-input one more, since two of them pair only when they share two inputs
/// (agilex5-re README (z14): the best of eight tables on the bench's
/// designs).
pub const ALM_COST: [u32; 7] = [3, 3, 3, 3, 3, 4, 6];

/// Constant and repeated LUT inputs folded out, constant LUTs replaced by
/// their value, and LUTs with the same inputs and table made one (the
/// mapper writes a LUT per gate output it maps; yosys' flow merges them
/// with `opt_merge -share_all`). Returns the LUTs left.
fn merge_luts(m: &mut Module) -> usize {
    let cells = std::mem::take(&mut m.cells);
    let mut subst = Subst::new(m);
    let mut seen: std::collections::HashMap<(Vec<SigBit>, u64), SigBit> = Default::default();
    for c in cells {
        let CellKind::Lut(lut) = &c.kind else {
            m.cells.push(c);
            continue;
        };
        let mut table: u64 = (0..lut.table.0.len())
            .filter(|&i| lut.table.0[i] == State::One)
            .fold(0, |t, i| t | 1 << i);
        let mut inputs: Vec<SigBit> = Vec::new();
        let mut width = lut.a.len();
        let mut i = 0;
        for &a in &lut.a {
            let fold = match a {
                SigBit::Const(s) => Some((None, s == State::One)),
                SigBit::Net(_) => inputs
                    .iter()
                    .position(|&k| k == a)
                    .map(|k| (Some(k), false)),
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
        let SigBit::Net(y) = lut.y else { continue };
        if inputs.is_empty() {
            subst.set(y, SigBit::Const(State::from(table & 1 == 1)));
            continue;
        }
        match seen.entry((inputs, table)) {
            std::collections::hash_map::Entry::Occupied(e) => subst.set(y, *e.get()),
            std::collections::hash_map::Entry::Vacant(e) => {
                let (a, t) = e.key().clone();
                let table = Const(
                    (0..1usize << a.len())
                        .map(|j| State::from(t >> j & 1 == 1))
                        .collect(),
                );
                m.cells.push(chipsmith_netlist::Cell {
                    kind: CellKind::Lut(Lut { a, y: lut.y, table }),
                    ..c
                });
                e.insert(lut.y);
            }
        }
    }
    subst.apply(m);
    m.cells
        .iter()
        .filter(|c| matches!(c.kind, CellKind::Lut(_)))
        .count()
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
