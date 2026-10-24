//! Gate-level `opt_expr` and `opt_merge`: constants propagated through
//! the gates, trivial gates replaced by what they compute (an input, its
//! inverse, a constant), and identical gates merged.
//!
//! One sweep in topological order does it: a gate's inputs are final when
//! it is visited, so a chain of simplifications resolves in one pass.

use std::collections::HashMap;

use chipsmith_netlist::{CellKind, Gate, GateOp, Module, Net, SigBit, State};

use crate::rewrite::{Namer, Subst};

/// What a gate simplifies to.
enum Simple {
    /// It is this signal.
    Is(SigBit),
    /// It is the inverse of this signal.
    Not(SigBit),
    /// It stays, with these (normalized) inputs.
    Gate(GateOp, Vec<SigBit>),
}

fn konst(b: SigBit) -> Option<bool> {
    match b {
        SigBit::Const(State::Zero) => Some(false),
        SigBit::Const(State::One) => Some(true),
        _ => None,
    }
}

/// A fixed order on signals, for putting commutative inputs in one order.
fn order(b: &SigBit) -> (u8, usize) {
    match b {
        SigBit::Const(s) => (0, *s as usize),
        SigBit::Net(n) => (1, n.index()),
    }
}

/// `inv[x]` is the signal `x` is the inverse of, where one is known.
fn simplify(op: GateOp, ins: &[SigBit], inv: &HashMap<SigBit, SigBit>) -> Simple {
    let is_not = |a: SigBit, b: SigBit| inv.get(&a) == Some(&b) || inv.get(&b) == Some(&a);
    let x = SigBit::X;
    match op {
        GateOp::Buf => Simple::Is(ins[0]),
        GateOp::Not => match konst(ins[0]) {
            Some(v) => Simple::Is(SigBit::Const(State::from(!v))),
            None if ins[0] == x => Simple::Is(x),
            None => match inv.get(&ins[0]) {
                Some(&orig) => Simple::Is(orig),
                None => Simple::Gate(GateOp::Not, ins.to_vec()),
            },
        },
        GateOp::And | GateOp::Or => {
            let and = op == GateOp::And;
            let (a, b) = (ins[0], ins[1]);
            let absorbing = SigBit::Const(State::from(!and));
            let neutral = SigBit::Const(State::from(and));
            if a == absorbing || b == absorbing || is_not(a, b) {
                Simple::Is(absorbing)
            } else if a == neutral || a == x {
                // An x input may be taken as the neutral value.
                Simple::Is(b)
            } else if b == neutral || b == x || a == b {
                Simple::Is(a)
            } else {
                let mut v = vec![a, b];
                v.sort_by_key(order);
                Simple::Gate(op, v)
            }
        }
        GateOp::Nand | GateOp::Nor => {
            let base = if op == GateOp::Nand {
                GateOp::And
            } else {
                GateOp::Or
            };
            match simplify(base, ins, inv) {
                Simple::Is(s) => Simple::Not(s),
                Simple::Not(s) => Simple::Is(s),
                Simple::Gate(_, v) => Simple::Gate(op, v),
            }
        }
        GateOp::AndNot | GateOp::OrNot => {
            // A & ~B, A | ~B: as AND/OR with B inverted when B is simple.
            let base = if op == GateOp::AndNot {
                GateOp::And
            } else {
                GateOp::Or
            };
            let (a, b) = (ins[0], ins[1]);
            let nb = match konst(b) {
                Some(v) => SigBit::Const(State::from(!v)),
                None if b == x => x,
                None => match inv.get(&b) {
                    Some(&o) => o,
                    None => return Simple::Gate(op, vec![a, b]),
                },
            };
            simplify(base, &[a, nb], inv)
        }
        GateOp::Xor | GateOp::Xnor => {
            let flip = op == GateOp::Xnor;
            let (a, b) = (ins[0], ins[1]);
            let out = |s: Simple| match (s, flip) {
                (s, false) => s,
                (Simple::Is(v), true) => Simple::Not(v),
                (Simple::Not(v), true) => Simple::Is(v),
                (g, true) => g,
            };
            if a == x || b == x {
                return Simple::Is(x);
            }
            match (konst(a), konst(b)) {
                (Some(p), Some(q)) => out(Simple::Is(SigBit::Const(State::from(p ^ q)))),
                (Some(false), None) => out(Simple::Is(b)),
                (None, Some(false)) => out(Simple::Is(a)),
                (Some(true), None) => out(Simple::Not(b)),
                (None, Some(true)) => out(Simple::Not(a)),
                _ if a == b => out(Simple::Is(SigBit::ZERO)),
                _ if is_not(a, b) => out(Simple::Is(SigBit::ONE)),
                _ => {
                    let mut v = vec![a, b];
                    v.sort_by_key(order);
                    Simple::Gate(op, v)
                }
            }
        }
        GateOp::Mux | GateOp::Nmux => {
            let (a, b, s) = (ins[0], ins[1], ins[2]);
            let r = match konst(s) {
                Some(false) => Simple::Is(a),
                Some(true) => Simple::Is(b),
                None if a == b || b == x => Simple::Is(a),
                None if a == x => Simple::Is(b),
                None => match (konst(a), konst(b)) {
                    (Some(false), Some(true)) => Simple::Is(s),
                    (Some(true), Some(false)) => Simple::Not(s),
                    (Some(false), None) => simplify(GateOp::And, &[s, b], inv),
                    (None, Some(true)) => simplify(GateOp::Or, &[s, a], inv),
                    // ~s & a, ~s | b: keep the mux, it is no bigger.
                    _ if s == a => simplify(GateOp::And, &[s, b], inv),
                    _ if s == b => simplify(GateOp::Or, &[s, a], inv),
                    _ => match inv.get(&s) {
                        // A select that is an inverse: swap the inputs.
                        Some(&ns) => Simple::Gate(GateOp::Mux, vec![b, a, ns]),
                        None => Simple::Gate(GateOp::Mux, vec![a, b, s]),
                    },
                },
            };
            if op == GateOp::Nmux {
                match r {
                    Simple::Is(v) => Simple::Not(v),
                    Simple::Not(v) => Simple::Is(v),
                    Simple::Gate(GateOp::Mux, v) => Simple::Gate(GateOp::Nmux, v),
                    Simple::Gate(o, v) => Simple::Gate(o, v),
                }
            } else {
                r
            }
        }
        _ => Simple::Gate(op, ins.to_vec()),
    }
}

/// Gate cells in topological order (inputs before readers); `None` if the
/// gates form a loop.
fn topo_order(m: &Module, gates: &[usize]) -> Option<Vec<usize>> {
    let mut driver: HashMap<Net, usize> = HashMap::new();
    for (k, &i) in gates.iter().enumerate() {
        if let CellKind::Gate(g) = &m.cells[i].kind {
            if let SigBit::Net(n) = g.y {
                driver.insert(n, k);
            }
        }
    }
    let n = gates.len();
    let mut indeg = vec![0u32; n];
    let mut readers: Vec<Vec<usize>> = vec![Vec::new(); n];
    for (k, &i) in gates.iter().enumerate() {
        let CellKind::Gate(g) = &m.cells[i].kind else {
            unreachable!()
        };
        for b in g.inputs() {
            if let Some(&d) = b.net().and_then(|net| driver.get(&net)) {
                indeg[k] += 1;
                readers[d].push(k);
            }
        }
    }
    let mut stack: Vec<usize> = (0..n).filter(|&k| indeg[k] == 0).rev().collect();
    let mut out = Vec::with_capacity(n);
    while let Some(k) = stack.pop() {
        out.push(gates[k]);
        for &r in readers[k].iter().rev() {
            indeg[r] -= 1;
            if indeg[r] == 0 {
                stack.push(r);
            }
        }
    }
    (out.len() == n).then_some(out)
}

/// Simplifies and merges the gates; returns whether anything changed.
pub fn opt_gates(m: &mut Module) -> bool {
    let gates: Vec<usize> = (0..m.cells.len())
        .filter(|&i| matches!(m.cells[i].kind, CellKind::Gate(_)))
        .collect();
    if gates.is_empty() {
        return false;
    }
    let Some(order) = topo_order(m, &gates) else {
        return false;
    };
    let mut namer = Namer::new(m, "optg");
    let mut subst = Subst::new(m);
    let mut inv: HashMap<SigBit, SigBit> = HashMap::new();
    let mut seen: HashMap<(GateOp, Vec<SigBit>), SigBit> = HashMap::new();
    let mut remove = vec![false; m.cells.len()];
    let mut new_gates: Vec<Gate> = Vec::new();
    let mut changed = false;
    for i in order {
        let CellKind::Gate(g) = &m.cells[i].kind else {
            unreachable!()
        };
        let y = g.y;
        let ins: Vec<SigBit> = g.inputs().iter().map(|&b| subst.resolve(b)).collect();
        let simple = simplify(g.op(), &ins, &inv);
        let SigBit::Net(yn) = y else { continue };
        let value = match simple {
            Simple::Is(v) => Some(v),
            Simple::Not(v) => {
                // An inverter of v, reusing one if it exists.
                let key = (GateOp::Not, vec![v]);
                if let Some(&e) = seen.get(&key) {
                    Some(e)
                } else if let Some(&orig) = inv.get(&v) {
                    Some(orig)
                } else if let Some(k) = konst(v) {
                    Some(SigBit::Const(State::from(!k)))
                } else if g.op() == GateOp::Not {
                    // Already an inverter of v.
                    seen.insert(key, y);
                    inv.insert(y, v);
                    None
                } else {
                    // Replace this gate by an inverter driving its output.
                    let ng = Gate::new(GateOp::Not, &[v], y).expect("arity");
                    new_gates.push(ng);
                    remove[i] = true;
                    changed = true;
                    seen.insert(key, y);
                    inv.insert(y, v);
                    continue;
                }
            }
            Simple::Gate(op, v) => {
                let key = (op, v.clone());
                if let Some(&e) = seen.get(&key) {
                    Some(e)
                } else {
                    seen.insert(key, y);
                    if op == GateOp::Not {
                        inv.insert(y, v[0]);
                    }
                    if op != g.op() || v != g.inputs() {
                        let ng = Gate::new(op, &v, y).expect("arity");
                        new_gates.push(ng);
                        remove[i] = true;
                        changed = true;
                    }
                    None
                }
            }
        };
        if let Some(v) = value {
            subst.set(yn, v);
            remove[i] = true;
            changed = true;
        }
    }
    let mut it = remove.into_iter();
    m.cells.retain(|_| !it.next().expect("one flag per cell"));
    for g in new_gates {
        m.add_cell(namer.fresh(), CellKind::Gate(g));
    }
    subst.apply(m);
    changed
}
