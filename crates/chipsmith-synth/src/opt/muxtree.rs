//! `opt_muxtree`: in a tree of multiplexers (each feeding only the next),
//! an input whose select condition contradicts what the path to it already
//! decided can never be chosen; it becomes x, which [`super::opt_word`]
//! then folds away.

use std::collections::HashMap;

use chipsmith_netlist::{CellKind, Module, Net, SigBit, SigSpec, State};

/// A multiplexer's inputs as words: A, then each B word.
fn ports(kind: &CellKind) -> Option<(Vec<SigSpec>, Vec<SigBit>)> {
    match kind {
        CellKind::Mux(m) => Some((vec![m.a.clone(), m.b.clone()], vec![m.s])),
        CellKind::Pmux(p) => {
            let w = p.a.len();
            let mut v = vec![p.a.clone()];
            v.extend((0..p.s.len()).map(|j| p.b[j * w..(j + 1) * w].to_vec()));
            Some((v, p.s.clone()))
        }
        _ => None,
    }
}

fn y_of(kind: &CellKind) -> &[SigBit] {
    match kind {
        CellKind::Mux(m) => &m.y,
        CellKind::Pmux(p) => &p.y,
        _ => &[],
    }
}

type Knowledge = HashMap<SigBit, bool>;

struct Tree {
    /// Per multiplexer cell: its input words and selects.
    muxes: HashMap<usize, (Vec<SigSpec>, Vec<SigBit>)>,
    /// The multiplexer whose whole output an input word is, if that output
    /// feeds nothing else.
    child: HashMap<SigSpec, usize>,
    /// Per multiplexer, which inputs some path reaches.
    live: HashMap<usize, Vec<bool>>,
}

impl Tree {
    fn visit(&mut self, cell: usize, know: &mut Knowledge) {
        let (words, sels) = self.muxes[&cell].clone();
        let state = |s: SigBit, know: &Knowledge| match s {
            SigBit::Const(State::Zero) => Some(false),
            SigBit::Const(State::One) => Some(true),
            _ => know.get(&s).copied(),
        };
        // Which inputs this visit can choose, and what each choice implies.
        let mut picks: Vec<(usize, Vec<(SigBit, bool)>)> = Vec::new();
        if sels.len() == 1 && words.len() == 2 {
            match state(sels[0], know) {
                Some(false) => picks.push((0, vec![])),
                Some(true) => picks.push((1, vec![])),
                None => {
                    picks.push((0, vec![(sels[0], false)]));
                    picks.push((1, vec![(sels[0], true)]));
                }
            }
        } else {
            let active = sels.iter().position(|&s| state(s, know) == Some(true));
            match active {
                Some(j) => picks.push((j + 1, vec![])),
                None => {
                    for (j, &s) in sels.iter().enumerate() {
                        if state(s, know) == Some(false) {
                            continue;
                        }
                        // One-hot: this case means every other select is 0.
                        let mut imp = vec![(s, true)];
                        imp.extend(
                            sels.iter()
                                .enumerate()
                                .filter(|&(k, _)| k != j)
                                .map(|(_, &o)| (o, false)),
                        );
                        picks.push((j + 1, imp));
                    }
                    picks.push((0, sels.iter().map(|&s| (s, false)).collect()));
                }
            }
        }
        for (port, implied) in picks {
            self.live.get_mut(&cell).expect("visited muxes are indexed")[port] = true;
            let Some(&next) = self.child.get(&words[port]) else {
                continue;
            };
            let mut added = Vec::new();
            for (s, v) in implied {
                if let SigBit::Net(_) = s {
                    if let std::collections::hash_map::Entry::Vacant(e) = know.entry(s) {
                        e.insert(v);
                        added.push(s);
                    }
                }
            }
            self.visit(next, know);
            for s in added {
                know.remove(&s);
            }
        }
    }
}

/// Returns whether any input became x.
pub fn opt_muxtree(m: &mut Module) -> bool {
    let mut users = vec![0u32; m.net_count()];
    for c in &m.cells {
        c.kind.for_each_port(|p| {
            if p.dir.is_none_or(|d| d.reads()) {
                for b in p.bits {
                    if let SigBit::Net(n) = b {
                        users[n.index()] += 1;
                    }
                }
            }
        });
    }
    for (_, w, dir) in m.ports() {
        if dir.drives() {
            for b in &w.bits {
                if let SigBit::Net(n) = b {
                    users[n.index()] += 1;
                }
            }
        }
    }
    let mut tree = Tree {
        muxes: HashMap::new(),
        child: HashMap::new(),
        live: HashMap::new(),
    };
    let mut by_y: HashMap<SigSpec, usize> = HashMap::new();
    for (i, c) in m.cells.iter().enumerate() {
        if let Some((words, sels)) = ports(&c.kind) {
            tree.live.insert(i, vec![false; words.len()]);
            tree.muxes.insert(i, (words, sels));
            by_y.insert(y_of(&c.kind).to_vec(), i);
        }
    }
    let single = |y: &SigSpec| {
        y.iter()
            .all(|b| b.net().is_some_and(|n: Net| users[n.index()] == 1))
    };
    let mut has_parent = vec![false; m.cells.len()];
    for (words, _) in tree.muxes.values() {
        for w in words {
            if let Some(&c) = by_y.get(w) {
                if single(w) {
                    tree.child.insert(w.clone(), c);
                    has_parent[c] = true;
                }
            }
        }
    }
    let mut roots: Vec<usize> = tree
        .muxes
        .keys()
        .copied()
        .filter(|&i| !has_parent[i])
        .collect();
    roots.sort();
    for r in roots {
        tree.visit(r, &mut Knowledge::new());
    }
    let mut changed = false;
    for (i, live) in &tree.live {
        if live.iter().all(|&l| l) {
            continue;
        }
        let dead = |port: usize| !live[port];
        match &mut m.cells[*i].kind {
            CellKind::Mux(x) => {
                if dead(0) && x.a.iter().any(|&b| b != SigBit::X) {
                    x.a.fill(SigBit::X);
                    changed = true;
                }
                if dead(1) && x.b.iter().any(|&b| b != SigBit::X) {
                    x.b.fill(SigBit::X);
                    changed = true;
                }
            }
            CellKind::Pmux(p) => {
                let w = p.a.len();
                if dead(0) && p.a.iter().any(|&b| b != SigBit::X) {
                    p.a.fill(SigBit::X);
                    changed = true;
                }
                for j in 0..p.s.len() {
                    let word = &mut p.b[j * w..(j + 1) * w];
                    if dead(j + 1) && word.iter().any(|&b| b != SigBit::X) {
                        word.fill(SigBit::X);
                        changed = true;
                    }
                }
            }
            _ => {}
        }
    }
    changed
}
