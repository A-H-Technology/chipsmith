//! Registers that never leave their power-up value, found as a greatest
//! fixed point: assume every register holds its power-up value, simulate
//! the logic in three values (0, 1, unknown) with the module's inputs
//! unknown, and drop each register whose next value isn't then certainly
//! that value; repeat until none drops. What is left is an inductive
//! invariant (true at power-up, preserved by every clock edge), so those
//! registers are constants.
//!
//! yosys' `opt_dff -sat` proves registers one at a time with the others
//! free; assuming all candidates at once proves the ones that only stay
//! constant together (a decoder register whose input is another stuck
//! register), which is most of what a fixed program in ROM leaves behind.
//! A register without a power-up value is taken to power up 0, as
//! `setundef -zero -init` does.

use std::collections::HashMap;

use anyhow::Result;
use chipsmith_netlist::{
    Bit, CellKind, DffControl, Ff, FfKind, GateOp, Module, Net, Polarity, SigBit, State, SyncEnable,
};

use crate::rewrite::{inits, Subst};
use crate::techmap;

/// Three-valued logic: `None` is unknown.
type V = Option<bool>;

fn merge(a: V, b: V) -> V {
    if a == b {
        a
    } else {
        None
    }
}

fn eval_gate(op: GateOp, i: &[V]) -> V {
    let and = |a: V, b: V| match (a, b) {
        (Some(false), _) | (_, Some(false)) => Some(false),
        (Some(true), Some(true)) => Some(true),
        _ => None,
    };
    let or = |a: V, b: V| match (a, b) {
        (Some(true), _) | (_, Some(true)) => Some(true),
        (Some(false), Some(false)) => Some(false),
        _ => None,
    };
    let not = |a: V| a.map(|v| !v);
    let xor = |a: V, b: V| Some(a? ^ b?);
    let mux = |a: V, b: V, s: V| match s {
        Some(false) => a,
        Some(true) => b,
        None => merge(a, b),
    };
    match op {
        GateOp::Buf => i[0],
        GateOp::Not => not(i[0]),
        GateOp::And => and(i[0], i[1]),
        GateOp::Nand => not(and(i[0], i[1])),
        GateOp::Or => or(i[0], i[1]),
        GateOp::Nor => not(or(i[0], i[1])),
        GateOp::Xor => xor(i[0], i[1]),
        GateOp::Xnor => not(xor(i[0], i[1])),
        GateOp::AndNot => and(i[0], not(i[1])),
        GateOp::OrNot => or(i[0], not(i[1])),
        GateOp::Mux => mux(i[0], i[1], i[2]),
        GateOp::Nmux => not(mux(i[0], i[1], i[2])),
        GateOp::Aoi3 => not(or(and(i[0], i[1]), i[2])),
        GateOp::Oai3 => not(and(or(i[0], i[1]), i[2])),
        GateOp::Aoi4 => not(or(and(i[0], i[1]), and(i[2], i[3]))),
        GateOp::Oai4 => not(and(or(i[0], i[1]), or(i[2], i[3]))),
        GateOp::Tbuf => None,
    }
}

/// A register bit that might be constant.
struct Candidate {
    ff: Ff<Bit>,
    value: bool,
    alive: bool,
}

/// Next value of a register bit given its inputs' values.
fn next(ff: &Ff<Bit>, val: &dyn Fn(SigBit) -> V) -> V {
    let FfKind::Dff { clk, d, control } = &ff.kind else {
        return None;
    };
    let q = val(ff.q);
    if matches!(clk.sig, SigBit::Const(_)) {
        return q;
    }
    let active =
        |c: chipsmith_netlist::Ctrl| val(c.sig).map(|v| v == (c.polarity == Polarity::Pos));
    let d = val(*d);
    let gated = |en: Option<chipsmith_netlist::Ctrl>, v: V| match en {
        None => v,
        Some(e) => match active(e) {
            Some(true) => v,
            Some(false) => q,
            None => merge(v, q),
        },
    };
    match control {
        DffControl::Plain { en } => gated(*en, d),
        DffControl::Async { arst, value, en } => {
            let normal = gated(*en, d);
            match active(*arst) {
                Some(false) => normal,
                Some(true) => Some(*value),
                None => merge(Some(*value), normal),
            }
        }
        DffControl::Sync { srst, value, en } => {
            let reset = |v: V| match active(*srst) {
                Some(false) => v,
                Some(true) => Some(*value),
                None => merge(Some(*value), v),
            };
            match en {
                SyncEnable::Always => reset(d),
                SyncEnable::ResetOverEnable(e) => reset(gated(Some(*e), d)),
                SyncEnable::EnableOverReset(e) => gated(Some(*e), reset(d)),
            }
        }
        DffControl::Load { .. } | DffControl::SetClr { .. } => None,
    }
}

/// The register output nets that are provably constant, and their values.
pub fn find(m: &Module) -> Result<HashMap<Net, bool>> {
    let init = inits(m);
    let mut g = m.clone();
    techmap::lower(&mut g, |k| {
        techmap::is_lowerable(k) || matches!(k, CellKind::Ff(_))
    })?;
    let mut cands: Vec<Candidate> = Vec::new();
    let mut gates = Vec::new();
    for c in &g.cells {
        match &c.kind {
            CellKind::FfGate(ff) => {
                let SigBit::Net(q) = ff.q else { continue };
                if !matches!(ff.kind, FfKind::Dff { .. }) {
                    continue;
                }
                let value = init.get(&q).copied().unwrap_or(State::Zero) == State::One;
                cands.push(Candidate {
                    ff: ff.clone(),
                    value,
                    alive: true,
                });
            }
            CellKind::Gate(gate) => gates.push(gate.clone()),
            _ => {}
        }
    }
    // Gates in topological order.
    let mut driver: HashMap<Net, usize> = HashMap::new();
    for (k, gt) in gates.iter().enumerate() {
        if let SigBit::Net(n) = gt.y {
            driver.insert(n, k);
        }
    }
    let mut order = Vec::with_capacity(gates.len());
    let mut state = vec![0u8; gates.len()];
    for start in 0..gates.len() {
        if state[start] != 0 {
            continue;
        }
        let mut stack = vec![(start, false)];
        while let Some((k, done)) = stack.pop() {
            if done {
                state[k] = 2;
                order.push(k);
                continue;
            }
            if state[k] != 0 {
                continue;
            }
            state[k] = 1;
            stack.push((k, true));
            for b in gates[k].inputs() {
                if let Some(&d) = b.net().and_then(|n| driver.get(&n)) {
                    if state[d] == 0 {
                        stack.push((d, false));
                    }
                }
            }
        }
    }
    let mut order_pos = vec![usize::MAX; gates.len()];
    for (i, &k) in order.iter().enumerate() {
        order_pos[k] = i;
    }
    let n = g.net_count();
    loop {
        let mut vals: Vec<V> = vec![None; n];
        for c in &cands {
            if c.alive {
                if let SigBit::Net(q) = c.ff.q {
                    vals[q.index()] = Some(c.value);
                }
            }
        }
        let get = |vals: &[V], b: SigBit| match b {
            SigBit::Const(State::Zero) => Some(false),
            SigBit::Const(State::One) => Some(true),
            SigBit::Const(_) => None,
            SigBit::Net(n) => vals[n.index()],
        };
        for &k in &order {
            let gt = &gates[k];
            let ins: Vec<V> = gt.inputs().iter().map(|&b| get(&vals, b)).collect();
            if let SigBit::Net(y) = gt.y {
                vals[y.index()] = eval_gate(gt.op(), &ins);
            }
        }
        let mut dropped = false;
        let exact = Exact {
            gates: &gates,
            driver: &driver,
            order_pos: &order_pos,
            vals: &vals,
        };
        for c in cands.iter_mut().filter(|c| c.alive) {
            let v = next(&c.ff, &|b| get(&vals, b));
            let holds = match v {
                Some(v) => v == c.value,
                // Unknown in three values: decide it exactly when the
                // logic it depends on has few enough free inputs.
                None => exact.holds(&c.ff, c.value),
            };
            if !holds {
                c.alive = false;
                dropped = true;
            }
        }
        if !dropped {
            break;
        }
    }
    Ok(cands
        .into_iter()
        .filter(|c| c.alive)
        .filter_map(|c| c.ff.q.net().map(|q| (q, c.value)))
        .collect())
}

/// Free inputs an exact check enumerates at most: 2^14 assignments, 256
/// words per signal.
const EXACT_INPUTS: usize = 14;

/// Exact evaluation of one register's next value over every assignment of
/// the unknown signals its logic reads, bit-parallel. A cut-down SAT check
/// (yosys' `opt_dff -sat`), complete for small cones: it sees that two
/// conditions on the same signal contradict, which three values can't.
struct Exact<'a> {
    gates: &'a [chipsmith_netlist::Gate],
    driver: &'a HashMap<Net, usize>,
    order_pos: &'a [usize],
    vals: &'a [V],
}

impl Exact<'_> {
    fn known(&self, b: SigBit) -> Option<V> {
        match b {
            SigBit::Const(State::Zero) => Some(Some(false)),
            SigBit::Const(State::One) => Some(Some(true)),
            SigBit::Const(_) => Some(None),
            SigBit::Net(n) => self.vals[n.index()].map(Some),
        }
    }

    /// Whether the register keeps `value`: first whether D alone always is
    /// `value` (then enables don't matter, only that resets agree), which
    /// needs fewer free inputs; then with the controls.
    fn holds(&self, ff: &Ff<Bit>, value: bool) -> bool {
        let FfKind::Dff { clk, d, control } = &ff.kind else {
            return false;
        };
        let resets_agree = match control {
            DffControl::Async { value: v, .. } | DffControl::Sync { value: v, .. } => *v == value,
            DffControl::Plain { .. } => true,
            DffControl::Load { .. } | DffControl::SetClr { .. } => false,
        };
        let d_alone = Ff {
            q: ff.q,
            kind: FfKind::Dff {
                clk: *clk,
                d: *d,
                control: DffControl::Plain { en: None },
            },
        };
        (resets_agree && self.holds_with_controls(&d_alone, value))
            || self.holds_with_controls(ff, value)
    }

    fn holds_with_controls(&self, ff: &Ff<Bit>, value: bool) -> bool {
        let FfKind::Dff { d, control, .. } = &ff.kind else {
            return false;
        };
        let mut roots = vec![*d];
        let mut ctrl = |c: &chipsmith_netlist::Ctrl| roots.push(c.sig);
        match control {
            DffControl::Plain { en } => en.iter().for_each(&mut ctrl),
            DffControl::Async { arst, en, .. } => {
                ctrl(arst);
                en.iter().for_each(&mut ctrl);
            }
            DffControl::Sync { srst, en, .. } => {
                ctrl(srst);
                if let SyncEnable::ResetOverEnable(e) | SyncEnable::EnableOverReset(e) = en {
                    ctrl(e);
                }
            }
            _ => return false,
        }
        // The cone: gates back from the roots to known or free signals.
        let mut cone: Vec<usize> = Vec::new();
        let mut leaves: Vec<Net> = Vec::new();
        let mut seen: std::collections::HashSet<Net> = std::collections::HashSet::new();
        let mut stack: Vec<SigBit> = roots.clone();
        while let Some(b) = stack.pop() {
            let SigBit::Net(n) = b else { continue };
            if self.known(b).flatten().is_some() || !seen.insert(n) {
                continue;
            }
            match self.driver.get(&n) {
                Some(&k) => {
                    cone.push(k);
                    stack.extend(self.gates[k].inputs().iter().copied());
                }
                None => {
                    leaves.push(n);
                    if leaves.len() > EXACT_INPUTS {
                        return false;
                    }
                }
            }
        }
        cone.sort_by_key(|&k| self.order_pos[k]);
        let words = (1usize << leaves.len()).div_ceil(64);
        let mut sig: HashMap<Net, Vec<u64>> = HashMap::new();
        for (i, n) in leaves.iter().enumerate() {
            let v: Vec<u64> = (0..words)
                .map(|w| {
                    if i < 6 {
                        // Bit j of each word is assignment w * 64 + j.
                        let pat = [
                            0xaaaa_aaaa_aaaa_aaaa,
                            0xcccc_cccc_cccc_cccc,
                            0xf0f0_f0f0_f0f0_f0f0,
                            0xff00_ff00_ff00_ff00,
                            0xffff_0000_ffff_0000,
                            0xffff_ffff_0000_0000u64,
                        ];
                        pat[i]
                    } else if w >> (i - 6) & 1 == 1 {
                        !0
                    } else {
                        0
                    }
                })
                .collect();
            sig.insert(*n, v);
        }
        let get = |sig: &HashMap<Net, Vec<u64>>, b: SigBit| -> Option<Vec<u64>> {
            match self.known(b) {
                Some(Some(v)) => Some(vec![if v { !0 } else { 0 }; words]),
                Some(None) => None,
                None => sig.get(&b.net()?).cloned(),
            }
        };
        for &k in &cone {
            let g = &self.gates[k];
            let ins: Option<Vec<Vec<u64>>> = g.inputs().iter().map(|&b| get(&sig, b)).collect();
            let Some(ins) = ins else { return false };
            let out: Vec<u64> = (0..words)
                .map(|w| {
                    let i: Vec<u64> = ins.iter().map(|x| x[w]).collect();
                    match g.op() {
                        GateOp::Buf => i[0],
                        GateOp::Not => !i[0],
                        GateOp::And => i[0] & i[1],
                        GateOp::Nand => !(i[0] & i[1]),
                        GateOp::Or => i[0] | i[1],
                        GateOp::Nor => !(i[0] | i[1]),
                        GateOp::Xor => i[0] ^ i[1],
                        GateOp::Xnor => !(i[0] ^ i[1]),
                        GateOp::AndNot => i[0] & !i[1],
                        GateOp::OrNot => i[0] | !i[1],
                        GateOp::Mux => (i[0] & !i[2]) | (i[1] & i[2]),
                        GateOp::Nmux => !((i[0] & !i[2]) | (i[1] & i[2])),
                        GateOp::Aoi3 => !((i[0] & i[1]) | i[2]),
                        GateOp::Oai3 => !((i[0] | i[1]) & i[2]),
                        GateOp::Aoi4 => !((i[0] & i[1]) | (i[2] & i[3])),
                        GateOp::Oai4 => !((i[0] | i[1]) & (i[2] | i[3])),
                        GateOp::Tbuf => 0,
                    }
                })
                .collect();
            if let SigBit::Net(y) = g.y {
                sig.insert(y, out);
            }
        }
        let q = if value { !0u64 } else { 0 };
        let Some(dv) = get(&sig, *d) else {
            return false;
        };
        let ctl = |c: &chipsmith_netlist::Ctrl| -> Option<Vec<u64>> {
            let v = get(&sig, c.sig)?;
            Some(if c.polarity == Polarity::Pos {
                v
            } else {
                v.iter().map(|x| !x).collect()
            })
        };
        let k = |b: bool| if b { !0u64 } else { 0 };
        let next: Vec<u64> = match control {
            DffControl::Plain { en: None } => dv,
            DffControl::Plain { en: Some(e) } => {
                let Some(e) = ctl(e) else { return false };
                (0..words).map(|w| (e[w] & dv[w]) | (!e[w] & q)).collect()
            }
            DffControl::Async {
                arst,
                value: rv,
                en,
            } => {
                let Some(a) = ctl(arst) else { return false };
                let e = match en {
                    Some(e) => match ctl(e) {
                        Some(e) => e,
                        None => return false,
                    },
                    None => vec![!0; words],
                };
                (0..words)
                    .map(|w| (a[w] & k(*rv)) | (!a[w] & ((e[w] & dv[w]) | (!e[w] & q))))
                    .collect()
            }
            DffControl::Sync {
                srst,
                value: rv,
                en,
            } => {
                let Some(r) = ctl(srst) else { return false };
                let reset = |w: usize, v: u64| (r[w] & k(*rv)) | (!r[w] & v);
                match en {
                    SyncEnable::Always => (0..words).map(|w| reset(w, dv[w])).collect(),
                    SyncEnable::ResetOverEnable(e) => {
                        let Some(e) = ctl(e) else { return false };
                        (0..words)
                            .map(|w| reset(w, (e[w] & dv[w]) | (!e[w] & q)))
                            .collect()
                    }
                    SyncEnable::EnableOverReset(e) => {
                        let Some(e) = ctl(e) else { return false };
                        (0..words)
                            .map(|w| (e[w] & reset(w, dv[w])) | (!e[w] & q))
                            .collect()
                    }
                }
            }
            _ => return false,
        };
        let valid = 1usize << leaves.len();
        next.iter().enumerate().all(|(w, &x)| {
            let mask = if valid >= (w + 1) * 64 {
                !0u64
            } else {
                (1u64 << (valid - w * 64)) - 1
            };
            (x ^ q) & mask == 0
        })
    }
}

/// Replaces the constant registers' outputs by their values and drops
/// those bits. Returns how many bits.
pub fn apply(m: &mut Module, consts: &HashMap<Net, bool>) -> usize {
    if consts.is_empty() {
        return 0;
    }
    let mut subst = Subst::new(m);
    let mut count = 0;
    for c in &mut m.cells {
        match &mut c.kind {
            CellKind::Ff(ff) => {
                let drop: Vec<bool> =
                    ff.q.iter()
                        .map(|b| b.net().is_some_and(|n| consts.contains_key(&n)))
                        .collect();
                if !drop.iter().any(|&d| d) {
                    continue;
                }
                for b in &ff.q {
                    if let Some(&v) = b.net().and_then(|n| consts.get(&n)) {
                        subst.set(b.net().expect("checked"), SigBit::Const(State::from(v)));
                        count += 1;
                    }
                }
                crate::memory::drop_bits(ff, &drop);
            }
            CellKind::FfGate(ff) => {
                if let Some(&v) = ff.q.net().and_then(|n| consts.get(&n)) {
                    subst.set(ff.q.net().expect("checked"), SigBit::Const(State::from(v)));
                    count += 1;
                }
            }
            _ => {}
        }
    }
    m.cells.retain(|c| match &c.kind {
        CellKind::Ff(ff) => ff.width() > 0,
        CellKind::FfGate(ff) => !ff.q.net().is_some_and(|n| consts.contains_key(&n)),
        _ => true,
    });
    subst.apply(m);
    count
}
