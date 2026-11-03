//! Multiplexer trees as one-hot selects (Quartus' "multiplexer
//! restructuring"): a tree of word-wide `$mux` and `$pmux` cells (an
//! `if`/`else if` chain, nested `case`s) becomes one `$pmux` whose selects
//! are each leaf's path condition, built once for the whole word; leaves
//! with the same data share a select, and a leaf that is the default (the
//! tree's all-false path) needs none. Lowered, each bit is then an AND-OR
//! over the distinct data it can take, where the tree put each condition
//! into every bit's chain of multiplexers. A `$pmux` node's words are its
//! selects' (an OR of them when several are high, as `techmap` lowers
//! one), its default on none.

use std::collections::HashMap;

use chipsmith_netlist::{CellKind, Module, Net, Pmux, SigBit, SigSpec};

use crate::rewrite::Gates;

/// Trees narrower than this aren't worth the shared selects.
const MIN_WIDTH: usize = 4;
/// Nor ones of fewer multiplexers.
const MIN_MUXES: usize = 3;

/// The literals (select, value) on the way from a tree's root to a leaf.
type Path = Vec<(SigBit, bool)>;

/// A tree: its root cell, its leaves (data word and path) and every cell
/// in it.
type Tree = (usize, Vec<(SigSpec, Path)>, Vec<usize>);

/// Rewrites the trees; returns how many.
pub fn restructure(m: &mut Module) -> usize {
    // Each net's readers, and the `$mux` driving it.
    let mut readers: HashMap<Net, usize> = HashMap::new();
    let mut mux_of: HashMap<Net, usize> = HashMap::new();
    for (i, c) in m.cells.iter().enumerate() {
        c.kind.for_each_port(|p| {
            if p.dir == Some(chipsmith_netlist::Dir::Out) {
                return;
            }
            for b in p.bits.iter().filter_map(|b| b.net()) {
                *readers.entry(b).or_default() += 1;
            }
        });
        let y = match &c.kind {
            CellKind::Mux(mx) => mx.y.first(),
            CellKind::Pmux(p) => p.y.first(),
            _ => None,
        };
        if let Some(n) = y.and_then(|b| b.net()) {
            mux_of.insert(n, i);
        }
    }
    for w in &m.wires {
        if w.port.is_some_and(|d| d.drives()) {
            for b in w.bits.iter().filter_map(|b| b.net()) {
                *readers.entry(b).or_default() += 1;
            }
        }
    }
    // An inner node: a `$mux` as wide as its parent whose every output
    // bit only the parent reads.
    let out = |j: usize| -> &SigSpec {
        match &m.cells[j].kind {
            CellKind::Mux(c) => &c.y,
            CellKind::Pmux(c) => &c.y,
            _ => unreachable!("a mux"),
        }
    };
    let inner = |width: usize, word: &SigSpec| -> Option<usize> {
        let first = word.first()?.net()?;
        let &j = mux_of.get(&first)?;
        let y = out(j);
        (y == word
            && y.len() == width
            && y.iter()
                .all(|b| b.net().is_some_and(|n| readers.get(&n) == Some(&1))))
        .then_some(j)
    };
    // A node's data words, each with the literals that choose it: a
    // `$mux`'s A on !S and B on S; a `$pmux`'s B words on their select
    // and A on none of them.
    let choices = |j: usize| -> Vec<(SigSpec, Vec<(SigBit, bool)>)> {
        match &m.cells[j].kind {
            CellKind::Mux(mx) => vec![
                (mx.a.clone(), vec![(mx.s, false)]),
                (mx.b.clone(), vec![(mx.s, true)]),
            ],
            CellKind::Pmux(p) => {
                let w = p.a.len();
                let mut v: Vec<_> =
                    p.s.iter()
                        .enumerate()
                        .map(|(k, &s)| (p.b[k * w..(k + 1) * w].to_vec(), vec![(s, true)]))
                        .collect();
                v.push((p.a.clone(), p.s.iter().map(|&s| (s, false)).collect()));
                v
            }
            _ => unreachable!("a mux"),
        }
    };
    // Roots: muxes no other tree takes in.
    let mut taken = vec![false; m.cells.len()];
    for (i, c) in m.cells.iter().enumerate() {
        if matches!(c.kind, CellKind::Mux(_) | CellKind::Pmux(_)) {
            let width = out(i).len();
            for (word, _) in choices(i) {
                if let Some(j) = inner(width, &word) {
                    taken[j] = true;
                }
            }
        }
    }
    let mut trees: Vec<Tree> = Vec::new();
    for (i, c) in m.cells.iter().enumerate() {
        if !matches!(c.kind, CellKind::Mux(_) | CellKind::Pmux(_)) {
            continue;
        }
        let width = out(i).len();
        if taken[i] || width < MIN_WIDTH {
            continue;
        }
        let mut leaves = Vec::new();
        let mut members = vec![i];
        let mut stack: Vec<(usize, Path)> = vec![(i, Vec::new())];
        while let Some((j, path)) = stack.pop() {
            for (word, lits) in choices(j) {
                let mut p = path.clone();
                p.extend(lits);
                match inner(width, &word) {
                    Some(k) => {
                        members.push(k);
                        stack.push((k, p));
                    }
                    None => leaves.push((word, p)),
                }
            }
        }
        // Worth it where the leaves repeat themselves: a tree whose leaves
        // are mostly distinct words costs more as one-hot selects (picorv32's
        // operand muxes: 6 words of 7 leaves, 32 LUTs more), one that
        // picks one word or the default far less (sdram-selftest's:
        // 112 LUTs fewer).
        let default = leaves
            .iter()
            .find(|(_, p)| p.iter().all(|&(_, v)| !v))
            .map(|(d, _)| d);
        let mut distinct: Vec<&SigSpec> = Vec::new();
        for (d, _) in &leaves {
            if Some(d) != default && !distinct.contains(&d) {
                distinct.push(d);
            }
        }
        if members.len() >= MIN_MUXES && 2 * distinct.len() <= leaves.len() {
            trees.push((i, leaves, members));
        }
    }
    let n = trees.len();
    let mut remove = vec![false; m.cells.len()];
    let mut g = Gates::new(m, "restructure");
    let mut built = Vec::new();
    for (root, leaves, members) in trees {
        let y = match &g.m.cells[root].kind {
            CellKind::Mux(r) => r.y.clone(),
            CellKind::Pmux(r) => r.y.clone(),
            _ => unreachable!("a mux"),
        };
        // The default: the all-false path's leaf.
        let default = leaves
            .iter()
            .find(|(_, p)| p.iter().all(|&(_, v)| !v))
            .map(|(d, _)| d.clone())
            .expect("a tree has an all-false path");
        // Each distinct other data word, with the OR of its paths.
        let mut words: Vec<(SigSpec, SigBit)> = Vec::new();
        for (data, path) in leaves {
            if data == default {
                continue;
            }
            let mut cond = SigBit::ONE;
            for (s, v) in path {
                let lit = if v { s } else { g.not(s) };
                cond = g.and(cond, lit);
            }
            match words.iter_mut().find(|(d, _)| *d == data) {
                Some((_, c)) => *c = g.or(*c, cond),
                None => words.push((data, cond)),
            }
        }
        for &j in &members {
            remove[j] = true;
        }
        built.push(Pmux {
            a: default,
            b: words.iter().flat_map(|(d, _)| d.iter().copied()).collect(),
            s: words.iter().map(|&(_, c)| c).collect(),
            y,
        });
    }
    let kept = remove.len();
    let mut it = remove.into_iter();
    // Cells added by the gates come after the original ones.
    g.m.cells.retain(|_| it.next().is_none_or(|r| !r));
    debug_assert!(it.next().is_none() || kept > 0);
    for p in built {
        g.add(CellKind::Pmux(p));
    }
    n
}

#[cfg(test)]
mod tests {
    use super::*;
    use chipsmith_netlist::{Dir, GateOp, Mux, Wire};

    /// Evaluates the module's muxes and gates for given input values.
    fn eval(m: &Module, inputs: &HashMap<SigBit, bool>, y: &SigSpec) -> Vec<bool> {
        let mut val = inputs.clone();
        val.insert(SigBit::ZERO, false);
        val.insert(SigBit::ONE, true);
        loop {
            let mut out: Vec<(SigBit, bool)> = Vec::new();
            for c in &m.cells {
                match &c.kind {
                    CellKind::Mux(mx) => {
                        if let Some(&s) = val.get(&mx.s) {
                            let src = if s { &mx.b } else { &mx.a };
                            for (o, i) in mx.y.iter().zip(src) {
                                if let Some(&v) = val.get(i) {
                                    out.push((*o, v));
                                }
                            }
                        }
                    }
                    CellKind::Pmux(p) => {
                        let w = p.a.len();
                        let s: Option<Vec<bool>> =
                            p.s.iter().map(|b| val.get(b).copied()).collect();
                        let Some(s) = s else { continue };
                        for k in 0..w {
                            let v = if s.iter().any(|&x| x) {
                                s.iter()
                                    .enumerate()
                                    .any(|(j, &x)| x && val.get(&p.b[j * w + k]) == Some(&true))
                            } else {
                                val.get(&p.a[k]) == Some(&true)
                            };
                            out.push((p.y[k], v));
                        }
                    }
                    CellKind::Gate(g) => {
                        let ins: Option<Vec<bool>> =
                            g.inputs().iter().map(|b| val.get(b).copied()).collect();
                        if let Some(i) = ins {
                            let v = match g.op() {
                                GateOp::Not => !i[0],
                                GateOp::And => i[0] & i[1],
                                GateOp::Or => i[0] | i[1],
                                op => panic!("{op:?}"),
                            };
                            out.push((g.y, v));
                        }
                    }
                    _ => {}
                }
            }
            let before = val.len();
            for (b, v) in out {
                val.entry(b).or_insert(v);
            }
            if val.len() == before {
                break;
            }
        }
        y.iter().map(|b| val[b]).collect()
    }

    /// `c3 ? x : (c2 ? d : (c1 ? x : d))` on 4 bits: one word or the
    /// default, so one shared select; the same function for every select.
    #[test]
    fn a_tree_of_one_word_becomes_one_select() {
        let mut m = Module::new("top");
        let (d, x, y) = (m.new_sig(4), m.new_sig(4), m.new_sig(4));
        let c: Vec<SigBit> = (0..3).map(|_| m.new_sig(1)[0]).collect();
        let (t1, t2) = (m.new_sig(4), m.new_sig(4));
        let mux = |a: &SigSpec, b: &SigSpec, s: SigBit, y: &SigSpec| {
            CellKind::Mux(Mux {
                a: a.clone(),
                b: b.clone(),
                s,
                y: y.clone(),
            })
        };
        m.add_cell("m1", mux(&d, &x, c[0], &t1));
        m.add_cell("m2", mux(&t1, &d, c[1], &t2));
        m.add_cell("m3", mux(&t2, &x, c[2], &y));
        let mut w = Wire::new("y", y.clone());
        w.port = Some(Dir::Out);
        m.wires.push(w);
        let before = m.clone();
        assert_eq!(restructure(&mut m), 1);
        let pmux: Vec<&Pmux> = m
            .cells
            .iter()
            .filter_map(|c| match &c.kind {
                CellKind::Pmux(p) => Some(p),
                _ => None,
            })
            .collect();
        assert_eq!(pmux.len(), 1);
        assert_eq!(pmux[0].s.len(), 1, "one select for the one word");
        for sel in 0..8usize {
            let mut inputs = HashMap::new();
            for (k, &b) in c.iter().enumerate() {
                inputs.insert(b, sel >> k & 1 == 1);
            }
            for k in 0..4 {
                inputs.insert(d[k], k % 2 == 0);
                inputs.insert(x[k], k < 2);
            }
            assert_eq!(
                eval(&m, &inputs, &y),
                eval(&before, &inputs, &y),
                "selects {sel:03b}"
            );
        }
    }
}
