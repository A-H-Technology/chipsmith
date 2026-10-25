//! State registers: a register that only ever takes one of a few constant
//! values (every assignment a constant, as in `state <= S_FETCH`) is
//! compared with those constants by the bits that tell them apart, not
//! all of them. A one-hot state register's `state == S_FETCH` becomes one
//! bit. yosys gets the same by extracting and re-encoding the FSM
//! (`fsm`); keeping the register as it is keeps its name and bits, so the
//! equivalence check can still match it, and needs only the invariant that
//! the register holds one of its values (which the check proves).

use std::collections::BTreeSet;

use chipsmith_netlist::{
    BinaryOp, CellKind, Const, DffControl, Ff, FfKind, Module, SigBit, SigSpec, State, Word,
};
use rustc_hash::FxHashMap;

use crate::rewrite::{inits, Gates, Subst};

/// A register and every value it can hold.
pub struct StateRegister {
    pub q: SigSpec,
    pub values: Vec<Const>,
}

fn konst(s: &[SigBit]) -> Option<Const> {
    // Bits as `State`s, all defined.
    s.iter()
        .map(|b| match b {
            SigBit::Const(v @ (State::Zero | State::One)) => Some(*v),
            _ => None,
        })
        .collect::<Option<Vec<_>>>()
        .map(Const)
}

/// At most this many values a state register can hold.
const MAX_VALUES: usize = 64;

/// What a multiplexer output bit can be: (cell, bit).
type MuxBits = FxHashMap<chipsmith_netlist::Net, (usize, usize)>;

/// The words `sig` can be when the register holds `qv`, through the
/// multiplexers in front of it (any select): `None` if some bit comes from
/// anything else, or there are too many.
fn possible(
    m: &Module,
    mux_bits: &MuxBits,
    q: &[SigBit],
    qv: &[State],
    sig: &[SigBit],
    depth: usize,
) -> Option<BTreeSet<Vec<State>>> {
    if depth > 64 {
        return None;
    }
    let mut words: BTreeSet<Vec<State>> = BTreeSet::from([Vec::new()]);
    let mut i = 0;
    while i < sig.len() {
        // A run of bits from one source: constants and the register's own
        // bits one at a time, a multiplexer's output bits together.
        let (part, len): (BTreeSet<Vec<State>>, usize) = match sig[i] {
            SigBit::Const(s @ (State::Zero | State::One)) => (BTreeSet::from([vec![s]]), 1),
            SigBit::Const(_) => return None,
            SigBit::Net(n) => {
                if let Some(k) = q.iter().position(|b| *b == SigBit::Net(n)) {
                    (BTreeSet::from([vec![qv[k]]]), 1)
                } else {
                    let &(cell, bit) = mux_bits.get(&n)?;
                    let mut len = 1;
                    while i + len < sig.len()
                        && sig[i + len].net().and_then(|n| mux_bits.get(&n))
                            == Some(&(cell, bit + len))
                    {
                        len += 1;
                    }
                    let inputs: Vec<SigSpec> = match &m.cells[cell].kind {
                        CellKind::Mux(x) => {
                            vec![x.a[bit..bit + len].to_vec(), x.b[bit..bit + len].to_vec()]
                        }
                        CellKind::Pmux(x) => {
                            let w = x.a.len();
                            let mut v = vec![x.a[bit..bit + len].to_vec()];
                            v.extend(
                                (0..x.s.len())
                                    .map(|j| x.b[j * w + bit..j * w + bit + len].to_vec()),
                            );
                            v
                        }
                        _ => return None,
                    };
                    let mut part = BTreeSet::new();
                    for input in inputs {
                        part.extend(possible(m, mux_bits, q, qv, &input, depth + 1)?);
                        if part.len() > MAX_VALUES {
                            return None;
                        }
                    }
                    (part, len)
                }
            }
        };
        let mut next = BTreeSet::new();
        for w in &words {
            for p in &part {
                let mut x = w.clone();
                x.extend_from_slice(p);
                next.insert(x);
                if next.len() > MAX_VALUES {
                    return None;
                }
            }
        }
        words = next;
        i += len;
    }
    Some(words)
}

/// Registers of at least two bits whose every value is a constant from
/// their logic, power-up value or reset: the values reachable from power-up
/// through the multiplexers in front of the register, whichever way they
/// select.
pub fn state_registers(m: &Module) -> Vec<StateRegister> {
    let init = inits(m);
    let mut mux_bits: MuxBits = FxHashMap::default();
    for (i, c) in m.cells.iter().enumerate() {
        let y = match &c.kind {
            CellKind::Mux(x) => &x.y,
            CellKind::Pmux(x) => &x.y,
            _ => continue,
        };
        for (k, b) in y.iter().enumerate() {
            if let SigBit::Net(n) = b {
                mux_bits.insert(*n, (i, k));
            }
        }
    }
    let mut out = Vec::new();
    for c in &m.cells {
        let CellKind::Ff(Ff::<Word> {
            q,
            kind: FfKind::Dff { d, control, .. },
        }) = &c.kind
        else {
            continue;
        };
        if q.len() < 2 {
            continue;
        }
        let power_up: Option<Vec<State>> = q
            .iter()
            .map(|b| b.net().and_then(|n| init.get(&n)).copied())
            .collect();
        let Some(power_up) = power_up else { continue };
        let mut values = BTreeSet::from([power_up]);
        match control {
            DffControl::Plain { .. } => {}
            DffControl::Async { value, .. } | DffControl::Sync { value, .. } => {
                if value
                    .0
                    .iter()
                    .any(|s| !matches!(s, State::Zero | State::One))
                {
                    continue;
                }
                values.insert(value.0.clone());
            }
            DffControl::Load { .. } | DffControl::SetClr { .. } => continue,
        }
        // Grow the set until every value's successors are in it.
        let mut todo: Vec<Vec<State>> = values.iter().cloned().collect();
        let mut ok = true;
        while let Some(v) = todo.pop() {
            match possible(m, &mux_bits, q, &v, d, 0) {
                Some(next) => {
                    for n in next {
                        if values.insert(n.clone()) {
                            todo.push(n);
                        }
                    }
                    if values.len() > MAX_VALUES {
                        ok = false;
                        break;
                    }
                }
                None => {
                    ok = false;
                    break;
                }
            }
        }
        if !ok || values.len() < 2 {
            continue;
        }
        out.push(StateRegister {
            q: q.clone(),
            values: values.into_iter().map(Const).collect(),
        });
    }
    out
}

/// The bit positions that tell `v` from every other value: greedily, the
/// position that tells it from the most values still undecided.
fn distinguishing(v: &Const, all: &[Const]) -> Vec<usize> {
    let mut left: Vec<&Const> = all.iter().filter(|u| *u != v).collect();
    let mut bits = Vec::new();
    while !left.is_empty() {
        let w = v.len();
        let best = (0..w)
            .max_by_key(|&p| {
                (
                    left.iter().filter(|u| u.0[p] != v.0[p]).count(),
                    std::cmp::Reverse(p),
                )
            })
            .expect("a register has bits");
        bits.push(best);
        left.retain(|u| u.0[best] == v.0[best]);
    }
    bits
}

/// Rewrites comparisons of a state register with a constant: a value it
/// can hold by the bits that tell that value apart, one it can't by its
/// answer. Returns how many.
pub fn compare_states(m: &mut Module) -> usize {
    let regs = state_registers(m);
    if regs.is_empty() {
        return 0;
    }
    let by_q: FxHashMap<SigSpec, &StateRegister> = regs.iter().map(|r| (r.q.clone(), r)).collect();
    let mut g = Gates::new(m, "fsm");
    let mut subst = Subst::new(g.m);
    let mut remove = vec![false; g.m.cells.len()];
    let mut count = 0;
    for i in 0..g.m.cells.len() {
        let CellKind::Binary(b) = &g.m.cells[i].kind else {
            continue;
        };
        let eq = match b.op {
            BinaryOp::Eq | BinaryOp::Eqx => true,
            BinaryOp::Ne | BinaryOp::Nex => false,
            _ => continue,
        };
        let (reg, k) = match (
            by_q.get(&b.a.sig),
            konst(&b.b.sig),
            by_q.get(&b.b.sig),
            konst(&b.a.sig),
        ) {
            (Some(r), Some(k), _, _) | (_, _, Some(r), Some(k)) => (*r, k),
            _ => continue,
        };
        if k.len() != reg.q.len() {
            continue;
        }
        let y = b.y.clone();
        let is = if reg.values.contains(&k) {
            let lits: Vec<SigBit> = distinguishing(&k, &reg.values)
                .into_iter()
                .map(|p| {
                    if k.0[p] == State::One {
                        reg.q[p]
                    } else {
                        g.not(reg.q[p])
                    }
                })
                .collect();
            g.reduce_and(&lits)
        } else {
            SigBit::ZERO
        };
        let bit = if eq { is } else { g.not(is) };
        for (n, yb) in y.iter().enumerate() {
            if let SigBit::Net(net) = yb {
                subst.set(*net, if n == 0 { bit } else { SigBit::ZERO });
            }
        }
        remove[i] = true;
        count += 1;
    }
    let mut it = remove.into_iter();
    g.m.cells.retain(|_| !it.next().unwrap_or(false));
    subst.apply(m);
    count
}

#[cfg(test)]
mod tests {
    use super::*;

    fn c(s: &str) -> Const {
        Const::parse(s).unwrap()
    }

    #[test]
    fn one_hot_states_need_one_bit() {
        let all = [c("0001"), c("0010"), c("0100"), c("1000")];
        assert_eq!(distinguishing(&c("0100"), &all), vec![2]);
    }

    #[test]
    fn binary_states_need_what_tells_them_apart() {
        let all = [c("00"), c("01"), c("10")];
        assert_eq!(distinguishing(&c("01"), &all), vec![0]);
        assert_eq!(distinguishing(&c("10"), &all), vec![1]);
        assert_eq!(distinguishing(&c("00"), &all).len(), 2);
    }
}
