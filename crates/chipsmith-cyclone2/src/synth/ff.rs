//! Gate-level registers onto the LE register (yosys' `dfflegalize` to the
//! six types cyclone2.ys allows, then `cells_map.v`).
//!
//! The LE register has a rising-edge clock, a clock enable, an
//! asynchronous clear and a synchronous clear that only acts on enabled
//! edges, and powers up 0. Everything else is made from those: inverters in
//! front of a falling clock or an active-low control, the enable widened to
//! cover a reset that acts without it, and a register that resets or powers
//! up to 1 stored inverted.

use anyhow::{bail, Result};
use chipsmith_netlist::{
    Bit, CellKind, Ctrl, DffControl, Ff, FfKind, Module, Net, Polarity, SigBit, State, SyncEnable,
};
use chipsmith_synth::rewrite::{define_inits, inits, set_inits, Gates, Subst};
use rustc_hash::FxHashMap;

use super::cells::Reg;

/// A control as an active-high signal.
fn active_high(g: &mut Gates, c: Ctrl) -> SigBit {
    match c.polarity {
        Polarity::Pos => c.sig,
        Polarity::Neg => g.not(c.sig),
    }
}

/// Maps every gate-level register to `LCELL_FF`. Returns how many.
pub fn map(m: &mut Module) -> Result<usize> {
    define_inits(m);
    let mut init = inits(m);
    let mut g = Gates::new(m, "ff");
    let cells = std::mem::take(&mut g.m.cells);
    let mut subst = Subst::new(g.m);
    let mut kept = Vec::new();
    let mut regs = Vec::new();
    for c in cells {
        match &c.kind {
            CellKind::FfGate(ff) => regs.push((c.name.clone(), ff.clone())),
            _ => kept.push(c),
        }
    }
    g.m.cells.extend(kept);
    let count = regs.len();
    for (name, ff) in regs {
        legalize(&mut g, &mut subst, &mut init, &name, ff)?;
    }
    subst.apply(m);
    // The LE register powers up 0, and every register now does.
    init.clear();
    set_inits(m, &init);
    Ok(count)
}

fn legalize(
    g: &mut Gates,
    subst: &mut Subst,
    init: &mut FxHashMap<Net, State>,
    name: &str,
    ff: Ff<Bit>,
) -> Result<()> {
    let FfKind::Dff { clk, d, control } = ff.kind else {
        bail!(
            "register {name}: latches and set/reset-only cells are not supported on the Cyclone II"
        );
    };
    let SigBit::Net(q) = ff.q else {
        bail!("register {name} drives a constant");
    };
    let clk = active_high(g, clk);
    let mut d = d;
    let (mut ena, mut aclr, mut sclr) = (SigBit::ONE, SigBit::ZERO, SigBit::ZERO);
    let powers_up_one = init.get(&q) == Some(&State::One);
    // Whether the register holds the inverse of Q: it must reset to 0 and
    // power up 0.
    let invert = match control {
        DffControl::Plain { en } => {
            if let Some(en) = en {
                ena = active_high(g, en);
            }
            powers_up_one
        }
        DffControl::Async { arst, value, en } => {
            if value != powers_up_one {
                bail!(
                    "register {name}: an asynchronous reset to {} with a power-up value of {} \
                     is not supported",
                    u8::from(value),
                    u8::from(powers_up_one)
                );
            }
            aclr = active_high(g, arst);
            if let Some(en) = en {
                ena = active_high(g, en);
            }
            value
        }
        DffControl::Sync { srst, value, en } => {
            let r = active_high(g, srst);
            let gated = match en {
                SyncEnable::Always => {
                    sclr = r;
                    None
                }
                SyncEnable::EnableOverReset(e) => {
                    ena = active_high(g, e);
                    sclr = r;
                    Some(ena)
                }
                // The reset acts on every edge: enable those edges too.
                SyncEnable::ResetOverEnable(e) => {
                    let e = active_high(g, e);
                    ena = g.or(e, r);
                    sclr = r;
                    None
                }
            };
            if value != powers_up_one {
                // The clear can only store the power-up value: the reset
                // goes into the LUT in front instead.
                let k = SigBit::Const(State::from(value));
                d = g.mux(d, k, r);
                sclr = SigBit::ZERO;
                if let Some(e) = gated {
                    ena = e;
                } else if matches!(en, SyncEnable::Always) {
                    ena = SigBit::ONE;
                }
            }
            powers_up_one
        }
        DffControl::Load { .. } | DffControl::SetClr { .. } => {
            bail!("register {name}: asynchronous load and set/clear registers are not supported on the Cyclone II")
        }
    };
    let reg_q = if invert {
        d = g.not(d);
        let inner = SigBit::Net(g.m.new_net());
        let outer = g.not(inner);
        subst.set(q, outer);
        inner
    } else {
        SigBit::Net(q)
    };
    init.remove(&q);
    g.add(
        Reg {
            d,
            clk,
            ena,
            aclr,
            sclr,
            q: reg_q,
        }
        .cell(),
    );
    Ok(())
}
