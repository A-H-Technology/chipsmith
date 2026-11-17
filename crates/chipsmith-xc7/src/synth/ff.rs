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
//! A latch is a slice storage element in latch mode (LDCE, LDPE): its gate
//! on the slice's clock, a gate enable on CE, a clear or preset on SR
//! ([`latch_reset`], [`latch_gate`] find them in the front end's enable).
//! Asynchronous loads, registers or latches with both an asynchronous set
//! and clear, and `$sr` have no slice element of their own and aren't
//! mapped.

use anyhow::{bail, Result};
use chipsmith_netlist::{
    Bit, CellKind, Ctrl, DffControl, Ff, FfKind, Gate, GateOp, LatchReset, Module, Polarity,
    SigBit, State, SyncEnable,
};
use chipsmith_synth::rewrite::{define_inits, inits, set_inits, Gates};
use rustc_hash::FxHashMap;

use super::cells::{Fd, Ld, Reset};

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
    let regs: Vec<_> = regs
        .into_iter()
        .map(|(name, ff)| {
            let ff = latch_reset(&mut g, ff);
            let (ff, ge) = latch_gate(g.m, ff);
            (name, ff, ge)
        })
        .collect();
    let count = regs.len();
    for (name, ff, ge) in regs {
        let cell = legalize(&mut g, &init, &name, ff, ge)?;
        g.add(cell);
    }
    // Each register holds its power-up value as its INIT now.
    set_inits(m, &FxHashMap::default());
    Ok(count)
}

/// Gates by the net they drive.
fn drivers(m: &Module) -> FxHashMap<chipsmith_netlist::Net, Gate> {
    m.cells
        .iter()
        .filter_map(|c| match &c.kind {
            CellKind::Gate(g) => Some((g.y.net()?, g.clone())),
            _ => None,
        })
        .collect()
}

/// The most gates [`cofactor`] rebuilds for one latch.
const COFACTOR_GATES: usize = 256;

/// `s` with `leaf` held at `value`: its cone rebuilt with the constant
/// folded in (`None` past [`COFACTOR_GATES`]).
fn cofactor(
    g: &mut Gates,
    drv: &FxHashMap<chipsmith_netlist::Net, Gate>,
    memo: &mut FxHashMap<SigBit, SigBit>,
    s: SigBit,
    leaf: SigBit,
    value: bool,
) -> Option<SigBit> {
    if s == leaf {
        return Some(SigBit::Const(State::from(value)));
    }
    if let Some(&r) = memo.get(&s) {
        return Some(r);
    }
    let Some(gate) = s.net().and_then(|n| drv.get(&n)) else {
        return Some(s);
    };
    if memo.len() >= COFACTOR_GATES {
        return None;
    }
    let gate = gate.clone();
    let mut i = Vec::new();
    for &x in gate.inputs() {
        i.push(cofactor(g, drv, memo, x, leaf, value)?);
    }
    let r = match gate.op() {
        GateOp::Buf => i[0],
        GateOp::Not => g.not(i[0]),
        GateOp::And => g.and(i[0], i[1]),
        GateOp::Nand => {
            let a = g.and(i[0], i[1]);
            g.not(a)
        }
        GateOp::Or => g.or(i[0], i[1]),
        GateOp::Nor => {
            let o = g.or(i[0], i[1]);
            g.not(o)
        }
        GateOp::Xor => g.xor(i[0], i[1]),
        GateOp::Xnor => g.xnor(i[0], i[1]),
        GateOp::AndNot => {
            let n = g.not(i[1]);
            g.and(i[0], n)
        }
        GateOp::OrNot => {
            let n = g.not(i[1]);
            g.or(i[0], n)
        }
        GateOp::Mux => g.mux(i[0], i[1], i[2]),
        GateOp::Nmux => {
            let m = g.mux(i[0], i[1], i[2]);
            g.not(m)
        }
        GateOp::Aoi3 => {
            let a = g.and(i[0], i[1]);
            let o = g.or(a, i[2]);
            g.not(o)
        }
        GateOp::Oai3 => {
            let o = g.or(i[0], i[1]);
            let a = g.and(o, i[2]);
            g.not(a)
        }
        GateOp::Aoi4 => {
            let a = g.and(i[0], i[1]);
            let b = g.and(i[2], i[3]);
            let o = g.or(a, b);
            g.not(o)
        }
        GateOp::Oai4 => {
            let a = g.or(i[0], i[1]);
            let b = g.or(i[2], i[3]);
            let o = g.and(a, b);
            g.not(o)
        }
        GateOp::Tbuf => return None,
    };
    memo.insert(s, r);
    Some(r)
}

/// The leaves of `s`'s gate cone, up to `limit` of them.
fn leaves(drv: &FxHashMap<chipsmith_netlist::Net, Gate>, s: SigBit, limit: usize) -> Vec<SigBit> {
    let mut out = Vec::new();
    let mut seen = std::collections::HashSet::new();
    let mut stack = vec![s];
    while let Some(x) = stack.pop() {
        if !seen.insert(x) || out.len() > limit {
            continue;
        }
        match x.net().and_then(|n| drv.get(&n)) {
            Some(gate) => stack.extend(gate.inputs().iter().copied()),
            None if x.net().is_some() => out.push(x),
            None => {}
        }
    }
    out
}

/// A latch whose enable and data a reset forces, as RTL writes one (`if
/// (rst) q = v; else if (g) q = d;` comes from the front end as one latch
/// enabled by `rst | g` with `rst ? v : d` its data): that reset as the
/// slice latch's asynchronous clear or preset, the latch gated by the rest.
/// Otherwise the reset would be in the gate, which a BUFG carries late
/// against the data. A leaf `r` of the enable's cone is such a reset when
/// holding it active makes the enable 1 and the data a constant.
fn latch_reset(g: &mut Gates, ff: Ff<Bit>) -> Ff<Bit> {
    let FfKind::Dlatch {
        en,
        d,
        reset: LatchReset::None,
    } = &ff.kind
    else {
        return ff;
    };
    let drv = drivers(g.m);
    let en_sig = match en.polarity {
        Polarity::Pos => en.sig,
        Polarity::Neg => return ff,
    };
    for r in leaves(&drv, en_sig, 16) {
        for active in [true, false] {
            let mut memo = FxHashMap::default();
            let (Some(e1), Some(d1)) = (
                cofactor(g, &drv, &mut memo, en_sig, r, active),
                cofactor(g, &drv, &mut FxHashMap::default(), *d, r, active),
            ) else {
                continue;
            };
            let SigBit::Const(v) = d1 else { continue };
            if e1 != SigBit::ONE || !matches!(v, State::Zero | State::One) {
                continue;
            }
            let (Some(e0), Some(d0)) = (
                cofactor(g, &drv, &mut FxHashMap::default(), en_sig, r, !active),
                cofactor(g, &drv, &mut FxHashMap::default(), *d, r, !active),
            ) else {
                continue;
            };
            if e0 == SigBit::ZERO {
                continue; // the reset is all that opens it: no latch to keep
            }
            let arst = Ctrl {
                sig: r,
                polarity: if active { Polarity::Pos } else { Polarity::Neg },
            };
            return Ff {
                q: ff.q,
                kind: FfKind::Dlatch {
                    en: Ctrl::pos(e0),
                    d: d0,
                    reset: LatchReset::Async {
                        arst,
                        value: v == State::One,
                    },
                },
            };
        }
    }
    ff
}

/// A latch's gate and gate enable (GE) as Vivado makes them, so the gate is
/// a signal of the design rather than logic of it that a BUFG carries late:
/// an inverter's output is its input active low (IS_G_INVERTED), and an
/// AND of a register's (or pin's) output and something else is the gate
/// and its enable.
fn latch_gate(m: &Module, mut ff: Ff<Bit>) -> (Ff<Bit>, SigBit) {
    let mut ge = SigBit::ONE;
    if let FfKind::Dlatch { en, .. } = &mut ff.kind {
        let drv = drivers(m);
        if en.polarity == Polarity::Pos {
            if let Some(gate) = en.sig.net().and_then(|n| drv.get(&n)) {
                let i = gate.inputs();
                let plain = |s: SigBit| s.net().is_some_and(|n| !drv.contains_key(&n));
                if gate.op() == GateOp::And && (plain(i[0]) || plain(i[1])) {
                    let (g0, e) = if plain(i[0]) {
                        (i[0], i[1])
                    } else {
                        (i[1], i[0])
                    };
                    en.sig = g0;
                    ge = e;
                }
            }
        }
        if let Some(gate) = en.sig.net().and_then(|n| drv.get(&n)) {
            if gate.op() == GateOp::Not {
                en.sig = gate.inputs()[0];
                en.polarity = match en.polarity {
                    Polarity::Pos => Polarity::Neg,
                    Polarity::Neg => Polarity::Pos,
                };
            }
        }
    }
    (ff, ge)
}

fn legalize(
    g: &mut Gates,
    init: &FxHashMap<chipsmith_netlist::Net, State>,
    name: &str,
    ff: Ff<Bit>,
    ge: SigBit,
) -> Result<CellKind> {
    let SigBit::Net(q) = ff.q else {
        bail!("register {name} drives a constant");
    };
    let power_up = init.get(&q) == Some(&State::One);
    let (clk, d, control) = match ff.kind {
        FfKind::Dff { clk, d, control } => (clk, d, control),
        FfKind::Global { .. } => {
            bail!("register {name}: `$ff` (the formal global clock) has no hardware")
        }
        // A latch is a slice storage element in latch mode (LDCE, LDPE):
        // the gate on the slice's clock, its enable always on.
        FfKind::Dlatch { en, d, reset } => {
            let reset = match reset {
                LatchReset::None => (SigBit::ZERO, false),
                LatchReset::Async { arst, value } => (active_high(g, arst), value),
                LatchReset::SetClr(_) => bail!(
                    "{name}: latches with both an asynchronous set and clear have no slice latch"
                ),
            };
            return Ok(Ld {
                d,
                g: en.sig,
                g_inv: en.polarity == Polarity::Neg,
                ge,
                reset,
                init: power_up,
                q: ff.q,
            }
            .cell());
        }
        FfKind::Sr(_) => {
            bail!("{name}: set/clear-only latches (`$sr`) have no slice latch")
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
    }
    .cell())
}
