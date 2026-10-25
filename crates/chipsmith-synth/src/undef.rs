//! Resolving x: where the RTL says "any value" (`'bx` assignments, an
//! out-of-range `$shiftx` part-select, a register without a power-up
//! value), synthesis picks one, once, before anything else.
//!
//! A multiplexer input that is x takes the other input's value (yosys'
//! `opt_expr -mux_undef`: the multiplexer then goes away), `$shiftx` is
//! lowered the same way, and every x left becomes 0 (`setundef -zero`).
//! After this the netlist means one thing, so the passes after it can only
//! keep its behaviour, never choose among behaviours: which is what lets
//! [`crate::opt::seqconst`] reason about it exactly, and a checker prove its
//! claims on this netlist.

use anyhow::Result;
use chipsmith_netlist::{BinaryOp, CellKind, Module, SigBit, State};

use crate::rewrite::define_inits;
use crate::techmap;

fn take_other(a: &mut SigBit, b: &mut SigBit) {
    if *a == SigBit::X {
        *a = *b;
    } else if *b == SigBit::X {
        *b = *a;
    }
}

pub fn refine(m: &mut Module) -> Result<()> {
    define_inits(m);
    techmap::lower(m, |k| {
        matches!(k, CellKind::Binary(b) if b.op == BinaryOp::Shiftx)
    })?;
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
