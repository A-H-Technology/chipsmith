//! Resolving x: where the RTL says "any value" (`'bx` assignments, an
//! out-of-range `$shiftx` part-select, a register without a power-up
//! value), synthesis picks one, once, before anything else.
//!
//! A multiplexer input that is x takes the other input's value (yosys'
//! `opt_expr -mux_undef`: the multiplexer then goes away), `$shiftx` is
//! lowered the same way, and every x left becomes 0 (`setundef -zero`).
//! A `$pmux` whose selects aren't exclusive by construction (comparisons
//! of one signal with different constants, as `proc` makes for a `case`)
//! gets them made so, the last one winning as in yosys' own model of it:
//! two at once is "any value" in RTLIL, and the passes after this assume
//! one at a time.
//!
//! After this the netlist means one thing, so the passes after it can only
//! keep its behaviour, never choose among behaviours: which is what lets
//! [`crate::opt::seqconst`] reason about it exactly, and a checker prove its
//! claims on this netlist.

use std::collections::{HashMap, HashSet};

use anyhow::Result;
use chipsmith_netlist::{BinaryOp, CellKind, Const, Module, Net, SigBit, SigSpec, State, UnaryOp};

use crate::rewrite::{define_inits, Gates};
use crate::techmap;

fn take_other(a: &mut SigBit, b: &mut SigBit) {
    if *a == SigBit::X {
        *a = *b;
    } else if *b == SigBit::X {
        *b = *a;
    }
}

/// The constants a select compares one signal with, if it is such a
/// comparison (or an OR of them).
fn compared(m: &Module, drivers: &HashMap<Net, usize>, s: SigBit) -> Option<(SigSpec, Vec<Const>)> {
    let c = &m.cells[*drivers.get(&s.net()?)?];
    match &c.kind {
        CellKind::Binary(b) if matches!(b.op, BinaryOp::Eq | BinaryOp::Eqx) && b.y.len() == 1 => {
            let konst = |x: &[SigBit]| -> Option<Const> {
                x.iter()
                    .map(|b| match b {
                        SigBit::Const(s @ (State::Zero | State::One)) => Some(*s),
                        _ => None,
                    })
                    .collect::<Option<Vec<_>>>()
                    .map(Const)
            };
            let w = b.a.sig.len().max(b.b.sig.len());
            let (a, bb) = (
                crate::techmap::extend(&b.a.sig, w, b.a.signed && b.b.signed),
                crate::techmap::extend(&b.b.sig, w, b.a.signed && b.b.signed),
            );
            match (konst(&a), konst(&bb)) {
                (None, Some(k)) => Some((a, vec![k])),
                (Some(k), None) => Some((bb, vec![k])),
                _ => None,
            }
        }
        CellKind::Unary(u)
            if matches!(u.op, UnaryOp::ReduceOr | UnaryOp::ReduceBool) && u.y.len() == 1 =>
        {
            let mut sig = None;
            let mut all = Vec::new();
            for &b in &u.a.sig {
                let (x, ks) = compared(m, drivers, b)?;
                if sig.get_or_insert_with(|| x.clone()) != &x {
                    return None;
                }
                all.extend(ks);
            }
            Some((sig?, all))
        }
        CellKind::Binary(b)
            if matches!(b.op, BinaryOp::Or | BinaryOp::LogicOr) && b.y.len() == 1 =>
        {
            let (x, mut ks) = compared(m, drivers, *b.a.sig.first()?)?;
            let (y, ks2) = compared(m, drivers, *b.b.sig.first()?)?;
            (x == y && b.a.sig.len() == 1 && b.b.sig.len() == 1).then(|| {
                ks.extend(ks2);
                (x, ks)
            })
        }
        _ => None,
    }
}

/// Whether at most one of `sels` can be 1: each compares the same signal
/// with constants no other one compares it with.
fn exclusive(m: &Module, drivers: &HashMap<Net, usize>, sels: &[SigBit]) -> bool {
    let mut sig = None;
    let mut seen = HashSet::new();
    for &s in sels {
        let Some((x, ks)) = compared(m, drivers, s) else {
            return false;
        };
        if sig.get_or_insert_with(|| x.clone()) != &x {
            return false;
        }
        for k in ks {
            if !seen.insert(k) {
                return false;
            }
        }
    }
    true
}

/// Makes every `$pmux`'s selects exclusive: the last active one wins.
fn exclusive_selects(m: &mut Module) {
    let mut drivers: HashMap<Net, usize> = HashMap::new();
    for (i, c) in m.cells.iter().enumerate() {
        c.kind.for_each_port(|p| {
            if p.dir == Some(chipsmith_netlist::Dir::Out) {
                for b in p.bits {
                    if let SigBit::Net(n) = b {
                        drivers.insert(*n, i);
                    }
                }
            }
        });
    }
    let todo: Vec<usize> = (0..m.cells.len())
        .filter(|&i| match &m.cells[i].kind {
            CellKind::Pmux(p) => p.s.len() > 1 && !exclusive(m, &drivers, &p.s),
            _ => false,
        })
        .collect();
    let mut g = Gates::new(m, "pmux");
    for i in todo {
        let CellKind::Pmux(p) = &g.m.cells[i].kind else {
            unreachable!()
        };
        let s = p.s.clone();
        let mut later = SigBit::ZERO;
        let mut out = vec![SigBit::ZERO; s.len()];
        for j in (0..s.len()).rev() {
            let nl = g.not(later);
            out[j] = g.and(s[j], nl);
            later = g.or(later, s[j]);
        }
        if let CellKind::Pmux(p) = &mut g.m.cells[i].kind {
            p.s = out;
        }
    }
}

pub fn refine(m: &mut Module) -> Result<()> {
    define_inits(m);
    exclusive_selects(m);
    techmap::lower(
        m,
        |k| matches!(k, CellKind::Binary(b) if b.op == BinaryOp::Shiftx),
    )?;
    for c in &mut m.cells {
        match &mut c.kind {
            CellKind::Mux(x) => {
                for (a, b) in x.a.iter_mut().zip(x.b.iter_mut()) {
                    take_other(a, b);
                }
            }
            CellKind::Pmux(p) => {
                let w = p.a.len();
                let n = p.s.len();
                for i in 0..w {
                    // An x default is the last case; an x case is the
                    // default.
                    if p.a[i] == SigBit::X && n > 0 {
                        p.a[i] = p.b[(n - 1) * w + i];
                    }
                    for j in 0..n {
                        if p.b[j * w + i] == SigBit::X {
                            p.b[j * w + i] = p.a[i];
                        }
                    }
                }
            }
            CellKind::Gate(g) if g.op() == chipsmith_netlist::GateOp::Mux => {
                let ins = g.inputs_mut();
                let (a, rest) = ins.split_at_mut(1);
                take_other(&mut a[0], &mut rest[0]);
            }
            _ => {}
        }
    }
    // Everything still x is 0.
    for c in &mut m.cells {
        if matches!(c.kind, CellKind::Other(_)) {
            continue;
        }
        c.kind.for_each_bit_mut(|b| {
            if *b == SigBit::X {
                *b = SigBit::ZERO;
            }
        });
        let zero = |v: &mut chipsmith_netlist::Const| {
            v.0.iter_mut()
                .filter(|s| **s == State::X)
                .for_each(|s| *s = State::Zero)
        };
        if let CellKind::Ff(ff) = &mut c.kind {
            if let chipsmith_netlist::FfKind::Dff {
                control:
                    chipsmith_netlist::DffControl::Async { value, .. }
                    | chipsmith_netlist::DffControl::Sync { value, .. },
                ..
            } = &mut ff.kind
            {
                zero(value);
            }
        }
    }
    Ok(())
}
