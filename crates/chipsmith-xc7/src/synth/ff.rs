//! Gate-level registers onto the slice's flip-flops, as yosys'
//! `synth_xilinx` legalizes them (`dfflegalize` to `$_DFFE_?P?P_` and
//! `$_SDFFE_?P?P_`, then `ff_map.v`).
//!
//! A slice register has a clock (either edge: the slice inverts it), a
//! clock enable and an SR input that either clears or sets it, on the
//! clock edge whether or not it is enabled (FDRE, FDSE) or at once (FDCE,
//! FDPE). Its power-up value (INIT) is its own. Active-low controls get an
//! inverter in front, which LUT mapping folds into the logic driving them;
//! a synchronous reset that only acts on enabled edges (`$sdffce`) gets
//! the enable ANDed in.
//!
//! Latches, asynchronous loads and registers with both an asynchronous set
//! and clear have no slice register of their own and aren't mapped yet.

use anyhow::{bail, Result};
use chipsmith_netlist::{
    Bit, CellKind, Ctrl, DffControl, Ff, FfKind, Module, Polarity, SigBit, State, SyncEnable,
};
use chipsmith_synth::rewrite::{define_inits, inits, set_inits, Gates};
use rustc_hash::FxHashMap;

use super::cells::{Fd, Reset};

/// A control as an active-high signal.
fn active_high(g: &mut Gates, c: Ctrl) -> SigBit {
    match c.polarity {
        Polarity::Pos => c.sig,
        Polarity::Neg => g.not(c.sig),
    }
}

/// Maps every gate-level register to a slice register; returns how many.
pub fn map(m: &mut Module) -> Result<usize> {
    define_inits(m);
    let init = inits(m);
    let mut g = Gates::new(m, "ff");
    let cells = std::mem::take(&mut g.m.cells);
    let mut regs = Vec::new();
    for c in cells {
        match c.kind {
            CellKind::FfGate(ff) => regs.push((c.name, ff)),
            kind => g.m.cells.push(chipsmith_netlist::Cell { kind, ..c }),
        }
    }
    let count = regs.len();
    for (name, ff) in regs {
        let fd = legalize(&mut g, &init, &name, ff)?;
        g.add(fd.cell());
    }
    // Each register holds its power-up value as its INIT now.
    set_inits(m, &FxHashMap::default());
    Ok(count)
}

fn legalize(
    g: &mut Gates,
    init: &FxHashMap<chipsmith_netlist::Net, State>,
    name: &str,
    ff: Ff<Bit>,
) -> Result<Fd> {
    let SigBit::Net(q) = ff.q else {
        bail!("register {name} drives a constant");
    };
    let power_up = init.get(&q) == Some(&State::One);
    let (clk, d, control) = match ff.kind {
        FfKind::Dff { clk, d, control } => (clk, d, control),
        FfKind::Global { .. } => {
            bail!("register {name}: `$ff` (the formal global clock) has no hardware")
        }
        FfKind::Dlatch { .. } | FfKind::Sr(_) => {
            bail!("{name}: latches aren't mapped on the 7-series yet")
        }
    };
    let enable = |g: &mut Gates, en: Option<Ctrl>| en.map_or(SigBit::ONE, |e| active_high(g, e));
    let (ce, reset) = match control {
        DffControl::Plain { en } => (enable(g, en), Reset::None),
        DffControl::Async { arst, value, en } => {
            let r = active_high(g, arst);
            let reset = if value {
                Reset::AsyncPreset(r)
            } else {
                Reset::AsyncClear(r)
            };
            (enable(g, en), reset)
        }
        DffControl::Sync { srst, value, en } => {
            let r = active_high(g, srst);
            let (ce, r) = match en {
                SyncEnable::Always => (SigBit::ONE, r),
                SyncEnable::ResetOverEnable(e) => (active_high(g, e), r),
                // The slice's reset acts on every edge: gate it.
                SyncEnable::EnableOverReset(e) => {
                    let e = active_high(g, e);
                    (e, g.and(r, e))
                }
            };
            let reset = if value {
                Reset::SyncSet(r)
            } else {
                Reset::SyncClear(r)
            };
            (ce, reset)
        }
        DffControl::Load { .. } => {
            bail!("{name}: registers with an asynchronous load aren't mapped on the 7-series yet")
        }
        DffControl::SetClr { .. } => bail!(
            "{name}: registers with an asynchronous set and clear aren't mapped on the 7-series yet"
        ),
    };
    Ok(Fd {
        d,
        clk: clk.sig,
        clk_inv: clk.polarity == Polarity::Neg,
        ce,
        reset,
        init: power_up,
        q: ff.q,
    })
}
