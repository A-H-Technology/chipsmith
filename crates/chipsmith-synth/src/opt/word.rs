//! Word-level `opt_expr` and `opt_merge`: cells with constant inputs
//! folded, multiplexers with constant or redundant inputs reduced, and
//! identical cells merged.

use std::collections::HashMap;

use chipsmith_netlist::{BinaryOp, Bmux, Cell, CellKind, Mux, Pmux, SigBit, SigSpec, State};

use crate::rewrite::{inits, Gates, Subst};
use crate::techmap::{eval, lower_comb};

/// What folding one cell gave.
enum Fold {
    Keep,
    /// The cell goes; its outputs are these signals.
    Gone(Vec<(SigBit, SigBit)>),
    /// The cell is replaced by this one (and some outputs are fixed).
    Replace(CellKind, Vec<(SigBit, SigBit)>),
}

fn set(subst: &mut Subst, pairs: Vec<(SigBit, SigBit)>) {
    for (y, v) in pairs {
        if let SigBit::Net(n) = y {
            subst.set(n, v);
        }
    }
}

fn fold_mux(g: &mut Gates, mx: &Mux) -> Fold {
    let pairs = |y: &[SigBit], v: &[SigBit]| y.iter().copied().zip(v.iter().copied()).collect();
    match mx.s {
        SigBit::Const(State::Zero) => return Fold::Gone(pairs(&mx.y, &mx.a)),
        SigBit::Const(State::One) => return Fold::Gone(pairs(&mx.y, &mx.b)),
        _ => {}
    }
    let mut fixed = Vec::new();
    let mut keep = Vec::new();
    let mut not_s = None;
    for i in 0..mx.y.len() {
        let (a, b) = (mx.a[i], mx.b[i]);
        let v = if a == b || b == SigBit::X {
            Some(a)
        } else if a == SigBit::X {
            Some(b)
        } else if a == SigBit::ZERO && b == SigBit::ONE {
            Some(mx.s)
        } else if a == SigBit::ONE && b == SigBit::ZERO {
            Some(*not_s.get_or_insert_with(|| g.not(mx.s)))
        } else {
            None
        };
        match v {
            Some(v) => fixed.push((mx.y[i], v)),
            None => keep.push(i),
        }
    }
    if fixed.is_empty() {
        Fold::Keep
    } else if keep.is_empty() {
        Fold::Gone(fixed)
    } else {
        let pick = |s: &SigSpec| keep.iter().map(|&i| s[i]).collect();
        Fold::Replace(
            CellKind::Mux(Mux {
                a: pick(&mx.a),
                b: pick(&mx.b),
                s: mx.s,
                y: pick(&mx.y),
            }),
            fixed,
        )
    }
}

fn fold_pmux(g: &mut Gates, p: &Pmux) -> Fold {
    let w = p.a.len();
    let word = |j: usize| &p.b[j * w..(j + 1) * w];
    let mut cases: Vec<(SigBit, SigSpec)> = Vec::new();
    for (j, &s) in p.s.iter().enumerate() {
        let b = word(j);
        match s {
            SigBit::Const(State::Zero) | SigBit::Const(State::X) => continue,
            // Selected (and so, one-hot, the only one): the result.
            SigBit::Const(State::One) => {
                return Fold::Gone(p.y.iter().copied().zip(b.iter().copied()).collect())
            }
            _ => {}
        }
        // A case giving A, or x, changes nothing.
        if b == &p.a[..] || b.iter().all(|&x| x == SigBit::X) {
            continue;
        }
        // Cases with the same data: one case, any of their selects.
        if let Some(c) = cases.iter_mut().find(|c| c.1 == b) {
            c.0 = g.or(c.0, s);
        } else {
            cases.push((s, b.to_vec()));
        }
    }
    let mut a = p.a.clone();
    // An undefined default may as well be the last case.
    if !cases.is_empty() && a.iter().all(|&x| x == SigBit::X) {
        a = cases.pop().expect("nonempty").1;
    }
    if cases.len() == p.s.len() && a == p.a {
        return Fold::Keep;
    }
    let y = p.y.clone();
    match cases.len() {
        0 => Fold::Gone(y.into_iter().zip(a).collect()),
        1 => {
            let (s, b) = cases.pop().expect("one");
            Fold::Replace(CellKind::Mux(Mux { a, b, s, y }), Vec::new())
        }
        _ => Fold::Replace(
            CellKind::Pmux(Pmux {
                a,
                b: cases.iter().flat_map(|c| c.1.iter().copied()).collect(),
                s: cases.iter().map(|c| c.0).collect(),
                y,
            }),
            Vec::new(),
        ),
    }
}

fn fold_bmux(b: &Bmux) -> Fold {
    // Constant select bits pick half the words each.
    let w = b.y.len();
    let mut a = b.a.clone();
    let mut s = Vec::new();
    for (k, &sel) in b.s.iter().enumerate() {
        let _ = k;
        match sel {
            SigBit::Const(State::Zero) | SigBit::Const(State::One) => {
                let hi = sel == SigBit::ONE;
                // Keep the words whose index has this bit (at its current
                // position, which is 0 after the earlier bits were taken
                // out: bit position = s.len()).
                let pos = s.len();
                let n = a.len() / w;
                let mut next = Vec::with_capacity(a.len() / 2);
                for i in 0..n {
                    if (i >> pos & 1 == 1) == hi {
                        next.extend_from_slice(&a[i * w..(i + 1) * w]);
                    }
                }
                a = next;
            }
            _ => s.push(sel),
        }
    }
    if s.len() == b.s.len() {
        return Fold::Keep;
    }
    if s.is_empty() {
        return Fold::Gone(b.y.iter().copied().zip(a).collect());
    }
    Fold::Replace(
        CellKind::Bmux(Bmux {
            a,
            s,
            y: b.y.clone(),
        }),
        Vec::new(),
    )
}

fn fold(g: &mut Gates, kind: &CellKind) -> Fold {
    if let Some(vals) = eval(kind) {
        return Fold::Gone(
            vals.into_iter()
                .map(|(n, s)| (SigBit::Net(n), SigBit::Const(s)))
                .collect(),
        );
    }
    match kind {
        CellKind::Mux(mx) => fold_mux(g, mx),
        CellKind::Pmux(p) => fold_pmux(g, p),
        CellKind::Bmux(b) => fold_bmux(b),
        // A shift by a constant is wiring.
        CellKind::Binary(b)
            if matches!(
                b.op,
                BinaryOp::Shl
                    | BinaryOp::Shr
                    | BinaryOp::Sshl
                    | BinaryOp::Sshr
                    | BinaryOp::Shift
                    | BinaryOp::Shiftx
            ) && b.b.sig.iter().all(|x| matches!(x, SigBit::Const(_))) =>
        {
            match lower_comb(g, kind) {
                Ok(Some(outs)) => Fold::Gone(outs),
                _ => Fold::Keep,
            }
        }
        _ => Fold::Keep,
    }
}

/// Folds every word-level cell once; returns whether anything changed.
pub fn opt_word(m: &mut Module) -> bool {
    let mut g = Gates::new(m, "optw");
    let mut subst = Subst::new(g.m);
    let cells = std::mem::take(&mut g.m.cells);
    let mut out = Vec::with_capacity(cells.len());
    let mut changed = false;
    for c in cells {
        if matches!(
            c.kind,
            CellKind::Gate(_)
                | CellKind::Other(_)
                | CellKind::Ff(_)
                | CellKind::FfGate(_)
                | CellKind::Mem(_)
        ) {
            out.push(c);
            continue;
        }
        match fold(&mut g, &c.kind) {
            Fold::Keep => out.push(c),
            Fold::Gone(pairs) => {
                set(&mut subst, pairs);
                changed = true;
            }
            Fold::Replace(kind, pairs) => {
                set(&mut subst, pairs);
                out.push(Cell { kind, ..c });
                changed = true;
            }
        }
    }
    // The gates folding made are in `g.m.cells` already.
    g.m.cells.extend(out);
    subst.apply(m);
    changed
}

use chipsmith_netlist::Module;

/// `opt_merge`: cells of the same type and parameters with the same inputs
/// compute the same outputs; the later ones go. Registers merge only with
/// the same power-up value.
pub fn opt_merge(m: &mut Module) -> bool {
    let init = inits(m);
    let mut seen: HashMap<String, usize> = HashMap::new();
    let mut subst = Subst::new(m);
    let mut remove = vec![false; m.cells.len()];
    for (i, c) in m.cells.iter().enumerate() {
        let mergeable = matches!(
            c.kind,
            CellKind::Unary(_)
                | CellKind::Binary(_)
                | CellKind::Mux(_)
                | CellKind::Pmux(_)
                | CellKind::Bmux(_)
                | CellKind::Demux(_)
                | CellKind::Alu(_)
                | CellKind::Lut(_)
                | CellKind::Ff(_)
                | CellKind::FfGate(_)
        );
        if !mergeable {
            continue;
        }
        let gen = c.kind.to_generic();
        let mut key = format!("{}|{:?}", gen.ty, gen.params);
        for p in &gen.ports {
            if p.dir != Some(chipsmith_netlist::Dir::Out) {
                key += &format!("|{}={:?}", p.name, p.sig);
            }
        }
        if matches!(c.kind, CellKind::Ff(_) | CellKind::FfGate(_)) {
            for p in gen.ports.iter().filter(|p| p.name == "Q") {
                let iv: Vec<State> = p
                    .sig
                    .iter()
                    .map(|b| {
                        b.net()
                            .and_then(|n| init.get(&n))
                            .copied()
                            .unwrap_or(State::X)
                    })
                    .collect();
                key += &format!("|init={iv:?}");
            }
        }
        match seen.get(&key) {
            Some(&first) => {
                let mut outs_first: Vec<SigBit> = Vec::new();
                m.cells[first].kind.for_each_port(|p| {
                    if p.dir == Some(chipsmith_netlist::Dir::Out) {
                        outs_first.extend_from_slice(p.bits);
                    }
                });
                let mut k = 0;
                c.kind.for_each_port(|p| {
                    if p.dir == Some(chipsmith_netlist::Dir::Out) {
                        for b in p.bits {
                            if let SigBit::Net(n) = b {
                                subst.set(*n, outs_first[k]);
                            }
                            k += 1;
                        }
                    }
                });
                remove[i] = true;
            }
            None => {
                seen.insert(key, i);
            }
        }
    }
    let changed = remove.iter().any(|&r| r);
    let mut it = remove.into_iter();
    m.cells.retain(|_| !it.next().expect("one flag per cell"));
    subst.apply(m);
    changed
}

/// Register bits with the same input, controls, reset value and power-up
/// value are the same register, wherever they sit in their words (`opt_merge`
/// compares whole cells; yosys finds these in `opt_dff -sat`). A word's
/// bits that a ROM or decoder computes alike become one.
pub fn merge_ff_bits(m: &mut Module) -> bool {
    use chipsmith_netlist::{DffControl, FfKind};
    let init = inits(m);
    let mut seen: HashMap<String, SigBit> = HashMap::new();
    let mut subst = Subst::new(m);
    let mut changed = false;
    for c in &mut m.cells {
        let CellKind::Ff(ff) = &mut c.kind else {
            continue;
        };
        let FfKind::Dff { clk, d, control } = &ff.kind else {
            continue;
        };
        // The controls as text, the per-bit parts added below.
        let ctrl = match control {
            DffControl::Plain { en } => format!("plain {en:?}"),
            DffControl::Async { arst, en, .. } => format!("async {arst:?} {en:?}"),
            DffControl::Sync { srst, en, .. } => format!("sync {srst:?} {en:?}"),
            DffControl::Load { .. } | DffControl::SetClr { .. } => continue,
        };
        let value = |i: usize| match control {
            DffControl::Async { value, .. } | DffControl::Sync { value, .. } => value.0[i],
            _ => State::X,
        };
        let mut drop = vec![false; ff.q.len()];
        for i in 0..ff.q.len() {
            let SigBit::Net(q) = ff.q[i] else { continue };
            let key = format!(
                "{clk:?} {ctrl} d={:?} v={:?} init={:?}",
                d[i],
                value(i),
                init.get(&q)
            );
            match seen.get(&key) {
                Some(&first) => {
                    subst.set(q, first);
                    drop[i] = true;
                    changed = true;
                }
                None => {
                    seen.insert(key, ff.q[i]);
                }
            }
        }
        if drop.iter().any(|&d| d) {
            crate::memory::drop_bits(ff, &drop);
        }
    }
    m.cells
        .retain(|c| !matches!(&c.kind, CellKind::Ff(ff) if ff.width() == 0));
    subst.apply(m);
    changed
}
