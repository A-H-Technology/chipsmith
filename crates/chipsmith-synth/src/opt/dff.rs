//! `opt_dff`: registers take over the multiplexers in front of them as a
//! synchronous reset (an input that is a constant) or a clock enable (an
//! input that is the register's own output, through a multiplexer tree);
//! controls that are constant go; bits whose every source is one constant
//! become that constant. As yosys' `opt_dff` (`simple.cc`, `constbits.cc`)
//! without `-sat`; [`super::seqconst`] does more than its SAT half.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use chipsmith_netlist::{
    Cell, CellKind, Const, Ctrl, DffControl, Ff, FfKind, GateOp, Module, Net, Polarity, SigBit,
    SigSpec, State, SyncEnable, Word,
};

use crate::rewrite::{inits, Gates, Subst};

/// An edge-triggered register with at most an enable and one reset: the
/// registers this pass rewrites.
#[derive(Clone)]
struct Reg {
    clk: Ctrl,
    d: SigSpec,
    q: SigSpec,
    en: Option<Ctrl>,
    arst: Option<(Ctrl, Const)>,
    srst: Option<(Ctrl, Const)>,
    /// With both an enable and a sync reset: the reset only acts on
    /// enabled edges.
    ce_over_srst: bool,
}

impl Reg {
    fn from_ff(ff: &Ff<Word>) -> Option<Reg> {
        let FfKind::Dff { clk, d, control } = &ff.kind else {
            return None;
        };
        let mut r = Reg {
            clk: *clk,
            d: d.clone(),
            q: ff.q.clone(),
            en: None,
            arst: None,
            srst: None,
            ce_over_srst: false,
        };
        match control {
            DffControl::Plain { en } => r.en = *en,
            DffControl::Async { arst, value, en } => {
                r.arst = Some((*arst, value.clone()));
                r.en = *en;
            }
            DffControl::Sync { srst, value, en } => {
                r.srst = Some((*srst, value.clone()));
                match en {
                    SyncEnable::Always => {}
                    SyncEnable::ResetOverEnable(e) => r.en = Some(*e),
                    SyncEnable::EnableOverReset(e) => {
                        r.en = Some(*e);
                        r.ce_over_srst = true;
                    }
                }
            }
            DffControl::Load { .. } | DffControl::SetClr { .. } => return None,
        }
        Some(r)
    }

    fn to_ff(&self) -> Ff<Word> {
        let control = match (&self.arst, &self.srst) {
            (Some((arst, value)), _) => DffControl::Async {
                arst: *arst,
                value: value.clone(),
                en: self.en,
            },
            (None, Some((srst, value))) => DffControl::Sync {
                srst: *srst,
                value: value.clone(),
                en: match self.en {
                    None => SyncEnable::Always,
                    Some(e) if self.ce_over_srst => SyncEnable::EnableOverReset(e),
                    Some(e) => SyncEnable::ResetOverEnable(e),
                },
            },
            (None, None) => DffControl::Plain { en: self.en },
        };
        Ff {
            q: self.q.clone(),
            kind: FfKind::Dff {
                clk: self.clk,
                d: self.d.clone(),
                control,
            },
        }
    }

    fn slice(&self, bits: &[usize]) -> Reg {
        let pick = |s: &SigSpec| bits.iter().map(|&i| s[i]).collect::<SigSpec>();
        let pickc = |c: &Const| Const(bits.iter().map(|&i| c.0[i]).collect());
        Reg {
            clk: self.clk,
            d: pick(&self.d),
            q: pick(&self.q),
            en: self.en,
            arst: self.arst.as_ref().map(|(c, v)| (*c, pickc(v))),
            srst: self.srst.as_ref().map(|(c, v)| (*c, pickc(v))),
            ce_over_srst: self.ce_over_srst,
        }
    }
}

/// Whether a control is constant: `Some(true)` if always active.
fn const_ctrl(c: Ctrl) -> Option<bool> {
    match c.sig {
        SigBit::Const(State::Zero) => Some(!c.polarity.is_pos()),
        SigBit::Const(State::One) => Some(c.polarity.is_pos()),
        _ => None,
    }
}

/// A one-bit condition `sig == value`, as yosys' `ctrl_t`.
type Cond = (SigBit, bool);

fn bit_key(b: SigBit) -> (u8, usize) {
    match b {
        SigBit::Const(s) => (0, s as usize),
        SigBit::Net(n) => (1, n.index()),
    }
}

/// A feedback path through a multiplexer tree: the select values on it.
type Pattern = BTreeMap<(u8, usize), (SigBit, bool)>;

struct Ctx {
    /// The multiplexer (cell, bit) driving each net.
    mux_of: HashMap<Net, (usize, usize)>,
    /// How many cell inputs and module outputs read each net.
    users: Vec<u32>,
}

impl Ctx {
    fn new(m: &Module) -> Ctx {
        let mut mux_of = HashMap::new();
        let mut users = vec![0u32; m.net_count()];
        for (i, c) in m.cells.iter().enumerate() {
            let y: &[SigBit] = match &c.kind {
                CellKind::Mux(x) => &x.y,
                CellKind::Pmux(x) => &x.y,
                CellKind::Gate(g) if g.op() == GateOp::Mux => std::slice::from_ref(&g.y),
                _ => &[],
            };
            for (k, b) in y.iter().enumerate() {
                if let SigBit::Net(n) = b {
                    mux_of.insert(*n, (i, k));
                }
            }
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
        Ctx { mux_of, users }
    }

    fn single_user_mux(&self, d: SigBit) -> Option<(usize, usize)> {
        let n = d.net()?;
        if self.users[n.index()] != 1 {
            return None;
        }
        self.mux_of.get(&n).copied()
    }
}

/// A one-bit-select multiplexer's (A, B, S) for bit `k`.
fn mux2(cells: &[Cell], (ci, k): (usize, usize)) -> Option<(SigBit, SigBit, SigBit)> {
    match &cells[ci].kind {
        CellKind::Mux(x) => Some((x.a[k], x.b[k], x.s)),
        CellKind::Gate(g) if g.op() == GateOp::Mux => {
            let i = g.inputs();
            Some((i[0], i[1], i[2]))
        }
        CellKind::Pmux(x) if x.s.len() == 1 => Some((x.a[k], x.b[k], x.s[0])),
        _ => None,
    }
}

fn is_const01(b: SigBit) -> bool {
    matches!(b, SigBit::Const(State::Zero | State::One))
}

/// Sets bit `k` of a multiplexer's input `port` (0 = A, else B word
/// `port - 1`) to x: the feedback a clock enable now does.
fn break_feedback(cells: &mut [Cell], (ci, k): (usize, usize), port: usize) {
    match &mut cells[ci].kind {
        CellKind::Mux(x) => {
            if port == 0 {
                x.a[k] = SigBit::X
            } else {
                x.b[k] = SigBit::X
            }
        }
        CellKind::Pmux(x) => {
            let w = x.a.len();
            if port == 0 {
                x.a[k] = SigBit::X
            } else {
                x.b[(port - 1) * w + k] = SigBit::X
            }
        }
        CellKind::Gate(g) => g.inputs_mut()[if port == 0 { 0 } else { 1 }] = SigBit::X,
        _ => {}
    }
}

/// yosys' `find_muxtree_feedback_patterns`: the select values under which
/// `d` is `q` through a tree of single-reader multiplexers; those inputs
/// become x.
fn feedback(ctx: &Ctx, cells: &mut [Cell], d: SigBit, q: SigBit, path: &Pattern) -> Vec<Pattern> {
    if d == q {
        return vec![path.clone()];
    }
    let Some(at) = ctx.single_user_mux(d) else {
        return Vec::new();
    };
    let (ci, k) = at;
    let (a, bs, ss): (SigBit, Vec<SigBit>, Vec<SigBit>) = match &cells[ci].kind {
        CellKind::Mux(x) => (x.a[k], vec![x.b[k]], vec![x.s]),
        CellKind::Pmux(x) => {
            let w = x.a.len();
            (
                x.a[k],
                (0..x.s.len()).map(|j| x.b[j * w + k]).collect(),
                x.s.clone(),
            )
        }
        CellKind::Gate(g) => {
            let i = g.inputs();
            (i[0], vec![i[1]], vec![i[2]])
        }
        _ => return Vec::new(),
    };
    let mut out = Vec::new();
    for (j, &s) in ss.iter().enumerate() {
        if path.get(&bit_key(s)).is_some_and(|v| v.1) {
            out = feedback(ctx, cells, bs[j], q, path);
            if bs[j] == q {
                break_feedback(cells, at, j + 1);
            }
            return out;
        }
    }
    let mut path_else = path.clone();
    for (j, &s) in ss.iter().enumerate() {
        if path.contains_key(&bit_key(s)) {
            continue;
        }
        let mut path_this = path.clone();
        path_else.insert(bit_key(s), (s, false));
        path_this.insert(bit_key(s), (s, true));
        out.extend(feedback(ctx, cells, bs[j], q, &path_this));
        if bs[j] == q {
            break_feedback(cells, at, j + 1);
        }
    }
    out.extend(feedback(ctx, cells, a, q, &path_else));
    if a == q {
        break_feedback(cells, at, 0);
    }
    out
}

fn join(a: State, b: State) -> Option<State> {
    match (a, b) {
        (State::X, v) | (v, State::X) => Some(v),
        _ if a == b => Some(a),
        _ => None,
    }
}

/// Runs over every register once; returns whether anything changed.
pub fn opt_dff(m: &mut Module) -> bool {
    let init = inits(m);
    let ctx = Ctx::new(m);
    let mut g = Gates::new(m, "dff");
    let mut subst = Subst::new(g.m);
    let mut changed = false;
    let mut out_regs: Vec<Reg> = Vec::new();
    let n = g.m.cells.len();
    let mut remove = vec![false; n];
    for ci in 0..n {
        let CellKind::Ff(ff) = &g.m.cells[ci].kind else {
            continue;
        };
        let Some(mut r) = Reg::from_ff(ff) else {
            continue;
        };
        let init_of = |q: SigBit| q.net().and_then(|n| init.get(&n)).copied().unwrap_or(State::X);
        let before = r.to_ff();
        // Constant controls.
        if let Some((arst, value)) = &r.arst {
            match const_ctrl(*arst) {
                Some(false) => r.arst = None,
                Some(true) => {
                    for (q, v) in r.q.iter().zip(&value.0) {
                        if let SigBit::Net(n) = q {
                            subst.set(*n, SigBit::Const(*v));
                        }
                    }
                    remove[ci] = true;
                    changed = true;
                    continue;
                }
                None => {}
            }
        }
        if let Some((srst, value)) = r.srst.clone() {
            match const_ctrl(srst) {
                Some(false) => r.srst = None,
                Some(true) => {
                    r.srst = None;
                    if !r.ce_over_srst {
                        r.en = None;
                    }
                    r.d = value.0.iter().map(|&s| SigBit::Const(s)).collect();
                }
                None => {}
            }
        }
        let mut frozen = matches!(r.clk.sig, SigBit::Const(_));
        if let Some(en) = r.en {
            match const_ctrl(en) {
                Some(true) => r.en = None,
                Some(false) => {
                    if let (Some((srst, value)), false) = (r.srst.clone(), r.ce_over_srst) {
                        r.en = Some(srst);
                        r.srst = None;
                        r.d = value.0.iter().map(|&s| SigBit::Const(s)).collect();
                    } else {
                        frozen = true;
                    }
                }
                None => {}
            }
        }
        if r.d == r.q && r.arst.is_none() {
            if let Some((srst, value)) = r.srst.clone() {
                // Only the reset changes it: the reset is the enable.
                let s = active_high(&mut g, srst);
                let en = match r.en {
                    Some(e) if r.ce_over_srst => {
                        let e = active_high(&mut g, e);
                        g.and(e, s)
                    }
                    _ => s,
                };
                r.en = Some(Ctrl::pos(en));
                r.srst = None;
                r.d = value.0.iter().map(|&s| SigBit::Const(s)).collect();
            } else {
                frozen = true;
            }
        }
        if frozen && r.arst.is_none() {
            // Never clocked, or never changes: it keeps its power-up value.
            for q in &r.q {
                if let SigBit::Net(n) = q {
                    subst.set(*n, SigBit::Const(init_of(*q)));
                }
            }
            remove[ci] = true;
            changed = true;
            continue;
        }
        // Bits whose every source is the same constant.
        let mut keep = Vec::new();
        for i in 0..r.q.len() {
            let mut v = Some(init_of(r.q[i]));
            if let Some((_, val)) = &r.arst {
                v = v.and_then(|v| join(v, val.0[i]));
            }
            if let Some((_, val)) = &r.srst {
                v = v.and_then(|v| join(v, val.0[i]));
            }
            v = match r.d[i] {
                SigBit::Const(s) => v.and_then(|v| join(v, s)),
                SigBit::Net(_) => None,
            };
            match (v, r.q[i]) {
                (Some(s), SigBit::Net(n)) => {
                    subst.set(n, SigBit::Const(s));
                    changed = true;
                }
                _ => keep.push(i),
            }
        }
        if keep.is_empty() {
            remove[ci] = true;
            continue;
        }
        if keep.len() != r.q.len() {
            r = r.slice(&keep);
        }
        // Multiplexers in front become a sync reset, then an enable.
        let mut regs = vec![r];
        let can_srst = |r: &Reg| r.arst.is_none() && (r.srst.is_none() || r.en.is_none() || r.ce_over_srst);
        if can_srst(&regs[0]) && regs[0].d != regs[0].q {
            regs = merge_srst(&ctx, &mut g, regs.pop().expect("one"));
        }
        let mut done = Vec::new();
        for r in regs {
            let can_ce = r.srst.is_none() || r.en.is_none() || !r.ce_over_srst;
            if can_ce && r.d != r.q {
                done.extend(merge_ce(&ctx, &mut g, r));
            } else {
                done.push(r);
            }
        }
        if done.len() == 1 && done[0].to_ff() == before {
            continue;
        }
        changed = true;
        remove[ci] = true;
        out_regs.extend(done);
    }
    let mut it = remove.into_iter();
    g.m.cells.retain(|_| !it.next().unwrap_or(false));
    for r in out_regs {
        g.add(CellKind::Ff(r.to_ff()));
    }
    subst.apply(m);
    changed
}

fn active_high(g: &mut Gates, c: Ctrl) -> SigBit {
    match c.polarity {
        Polarity::Pos => c.sig,
        Polarity::Neg => g.not(c.sig),
    }
}

/// yosys' `try_merge_srst`: multiplexers with a constant input in front of
/// D are a synchronous reset, grouped by the resets each bit has.
fn merge_srst(ctx: &Ctx, g: &mut Gates, mut r: Reg) -> Vec<Reg> {
    let mut groups: BTreeMap<BTreeSet<((u8, usize), bool)>, (Vec<Cond>, Vec<usize>)> =
        BTreeMap::new();
    let mut rest = Vec::new();
    let mut vals = Vec::with_capacity(r.q.len());
    for i in 0..r.q.len() {
        let mut resets: Vec<Cond> = Vec::new();
        let mut val = r.srst.as_ref().map_or(State::X, |(_, v)| v.0[i]);
        while let Some(at) = ctx.single_user_mux(r.d[i]) {
            let Some((a, b, s)) = mux2(&g.m.cells, at) else {
                break;
            };
            if is_const01(a) && is_const01(b) {
                break;
            }
            let ok = |k: SigBit| {
                is_const01(k)
                    && (val == State::X || SigBit::Const(val) == k)
            };
            if ok(b) && a != r.q[i] {
                val = match b {
                    SigBit::Const(s) => s,
                    _ => unreachable!(),
                };
                resets.push((s, true));
                r.d[i] = a;
            } else if ok(a) && b != r.q[i] {
                val = match a {
                    SigBit::Const(s) => s,
                    _ => unreachable!(),
                };
                resets.push((s, false));
                r.d[i] = b;
            } else {
                break;
            }
        }
        vals.push(val);
        if resets.is_empty() {
            rest.push(i);
        } else {
            if let Some((srst, _)) = &r.srst {
                resets.push((srst.sig, srst.polarity.is_pos()));
            }
            let key = resets.iter().map(|&(s, p)| (bit_key(s), p)).collect();
            groups.entry(key).or_insert_with(|| (resets, Vec::new())).1.push(i);
        }
    }
    let mut out = Vec::new();
    for (_, (resets, bits)) in groups {
        let mut nr = r.slice(&bits);
        let mut uniq: Vec<Cond> = Vec::new();
        for c in resets {
            if !uniq.contains(&c) {
                uniq.push(c);
            }
        }
        let srst = combine_resets(g, &uniq);
        nr.srst = Some((srst, Const(bits.iter().map(|&i| vals[i]).collect())));
        if nr.en.is_some() {
            nr.ce_over_srst = true;
        }
        out.push(nr);
    }
    if !rest.is_empty() {
        out.push(r.slice(&rest));
    }
    out
}

/// Any of the resets: an OR (or, all active low, an AND).
fn combine_resets(g: &mut Gates, ctrls: &[Cond]) -> Ctrl {
    if let [(s, p)] = ctrls {
        return Ctrl {
            sig: *s,
            polarity: Polarity::from_bool(*p),
        };
    }
    let pol = ctrls.iter().any(|c| c.1);
    let bits: Vec<SigBit> = ctrls
        .iter()
        .map(|&(s, p)| if p == pol { s } else { g.not(s) })
        .collect();
    let sig = if pol {
        g.reduce_or(&bits)
    } else {
        g.reduce_and(&bits)
    };
    Ctrl {
        sig,
        polarity: Polarity::from_bool(pol),
    }
}

/// yosys' `try_merge_ce`: multiplexers that feed Q back are an enable.
fn merge_ce(ctx: &Ctx, g: &mut Gates, mut r: Reg) -> Vec<Reg> {
    type Key = (Vec<Vec<((u8, usize), bool)>>, Vec<((u8, usize), bool)>);
    let mut groups: BTreeMap<Key, (Vec<Pattern>, Vec<Cond>, Vec<usize>)> = BTreeMap::new();
    let mut rest = Vec::new();
    for i in 0..r.q.len() {
        let mut enables: Vec<Cond> = Vec::new();
        while let Some(at) = ctx.single_user_mux(r.d[i]) {
            let Some((a, b, s)) = mux2(&g.m.cells, at) else {
                break;
            };
            if a == r.q[i] {
                enables.push((s, true));
                r.d[i] = b;
            } else if b == r.q[i] {
                enables.push((s, false));
                r.d[i] = a;
            } else {
                break;
            }
        }
        let mut patterns = feedback(ctx, &mut g.m.cells, r.d[i], r.q[i], &Pattern::new());
        if patterns.is_empty() && enables.is_empty() {
            rest.push(i);
            continue;
        }
        if let Some(en) = r.en {
            enables.push((en.sig, en.polarity.is_pos()));
        }
        let pkey = |p: &Pattern| p.iter().map(|(k, v)| (*k, v.1)).collect::<Vec<_>>();
        patterns.sort_by_key(pkey);
        patterns.dedup_by(|a, b| pkey(a) == pkey(b));
        let mut ekeys: Vec<((u8, usize), bool)> =
            enables.iter().map(|&(s, p)| (bit_key(s), p)).collect();
        ekeys.sort();
        ekeys.dedup();
        let pkeys = patterns.iter().map(pkey).collect();
        groups
            .entry((pkeys, ekeys))
            .or_insert_with(|| (patterns, enables, Vec::new()))
            .2
            .push(i);
    }
    let mut out = Vec::new();
    for (_, (patterns, enables, bits)) in groups {
        let mut nr = r.slice(&bits);
        // Enabled unless on a feedback path, and every enable active.
        let mut terms: Vec<SigBit> = Vec::new();
        for p in &patterns {
            let ne: Vec<SigBit> = p
                .values()
                .map(|&(s, v)| if v { g.not(s) } else { s })
                .collect();
            terms.push(g.reduce_or(&ne));
        }
        let mut seen = Vec::new();
        for &(s, pol) in &enables {
            if seen.contains(&(s, pol)) {
                continue;
            }
            seen.push((s, pol));
            terms.push(if pol { s } else { g.not(s) });
        }
        let en = g.reduce_and(&terms);
        nr.en = Some(Ctrl::pos(en));
        nr.ce_over_srst = false;
        out.push(nr);
    }
    if !rest.is_empty() {
        out.push(r.slice(&rest));
    }
    out
}
