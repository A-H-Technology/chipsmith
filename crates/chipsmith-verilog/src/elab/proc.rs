//! `always` and `initial` blocks, and memories.
//!
//! An `always` block runs once symbolically ([`super::stmt`]); what it
//! assigned becomes registers, latches or plain logic, as yosys' `proc`
//! makes them:
//!
//! - `@(posedge c)`: a `$dff` per variable, D the value the block leaves.
//! - `@(posedge c or negedge r)`: the block must be `if (!r) ... else ...`
//!   (one test per extra edge, IEEE 1364.1 §5.2.2): constant reset values
//!   make an `$adff`, others an `$aldff`; several resets with constant
//!   values a `$dffsr`.
//! - `@*` or a level list: logic; a variable not assigned on every path is
//!   a `$dlatch` enabled where it is.
//!
//! A variable's outputs are recorded as [`ProcOut`]s and only become
//! cells when the module is finished, once every block is known: an
//! `integer` loop counter several blocks use as a temporary then doesn't
//! get several drivers.
//!
//! Memories become one `$mem_v2` with an asynchronous read port per read
//! and a write port per write a clocked block makes (in program order,
//! later ones winning); `memory_dff` and friends make the reads
//! synchronous later, as for yosys' own front end. A memory written from
//! combinational logic or with `=` becomes one variable per word instead
//! (yosys' `mem2reg`).

use chipsmith_netlist::{
    Ctrl, DffControl, Ff, FfKind, LatchReset, Mem, MemRd, MemWr, Polarity, SetClr, SigBit, SigSpec,
    State, Word,
};

use rustc_hash::FxHashMap;

use super::stmt::{Env, MemWrite, Mode, Slot};
use super::{ModElab, ScopeId, Sym};
use crate::ast::{BinOp, Edge, Event, Expr, ExprKind, Stmt, StmtKind, UnOp};
use crate::build::{as_const, konst, xs};
use crate::source::{bail, Diag, Result, Span};

#[derive(Debug)]
pub(crate) enum MemStorage {
    /// A `$mem_v2` and its ports so far.
    Ram { rd: Vec<MemRd>, wr: Vec<MemWr> },
    /// A variable per word, lowest index first.
    Regs(Vec<usize>),
}

/// What one block does to one variable.
#[derive(Debug)]
pub(crate) struct ProcOut {
    pub var: usize,
    /// Bit positions it assigns.
    pub bits: Vec<usize>,
    /// Assigned with `=` before ever being read in the block: a
    /// temporary, which needn't drive the variable if another block does.
    pub temp: bool,
    pub span: Span,
    pub kind: OutKind,
}

#[derive(Debug)]
pub(crate) enum OutKind {
    /// Logic; where `en` isn't 1 the bit is a latch enabled by it.
    Comb { value: SigSpec, en: SigSpec },
    Ff {
        clk: Ctrl,
        d: SigSpec,
        /// Asynchronous controls, highest priority first, each with the
        /// value it forces (Q where the reset branch leaves a bit alone).
        resets: Vec<(Ctrl, SigSpec)>,
    },
}

/// `x`, `!x`, `x == 0`, ... : which signal a reset test tests, and the
/// level that makes it true.
fn reset_test(cond: &Expr) -> Option<(&Expr, bool)> {
    let is_bit = |e: &Expr, v: u8| match &e.kind {
        ExprKind::Number(n) => {
            n.bits[0] == State::from(v == 1) && n.bits[1..].iter().all(|&b| b == State::Zero)
        }
        _ => false,
    };
    match &cond.kind {
        ExprKind::Ident(_) | ExprKind::Hier(_) => Some((cond, true)),
        ExprKind::Unary {
            op: UnOp::LogicNot | UnOp::BitNot,
            arg,
        } => reset_test(arg).map(|(e, l)| (e, !l)),
        ExprKind::Binary { op, lhs, rhs }
            if matches!(op, BinOp::Eq | BinOp::Ne | BinOp::CaseEq | BinOp::CaseNe) =>
        {
            let eq = matches!(op, BinOp::Eq | BinOp::CaseEq);
            let (sig, level) = if is_bit(rhs, 1) {
                (lhs, true)
            } else if is_bit(rhs, 0) {
                (lhs, false)
            } else if is_bit(lhs, 1) {
                (rhs, true)
            } else if is_bit(lhs, 0) {
                (rhs, false)
            } else {
                return None;
            };
            reset_test(sig).map(|(e, l)| (e, l == (level == eq)))
        }
        _ => None,
    }
}

fn unwrap_block(s: &Stmt) -> &Stmt {
    match &s.kind {
        StmtKind::Block {
            name: None,
            decls,
            params,
            stmts,
        } if decls.is_empty() && params.is_empty() && stmts.len() == 1 => unwrap_block(&stmts[0]),
        _ => s,
    }
}

impl<'a, 'd> ModElab<'a, 'd> {
    pub fn always(&mut self, body: &'a Stmt, scope: ScopeId, span: Span) -> Result<()> {
        let StmtKind::Event { event, body } = &body.kind else {
            bail!(
                span,
                "an always block without an event control (`@(...)`) is not supported"
            );
        };
        let items = match event {
            Event::Star => Vec::new(),
            Event::List(items) => items.iter().filter(|i| i.edge.is_some()).collect(),
        };
        if let Event::List(all) = event {
            if !items.is_empty() && items.len() != all.len() {
                bail!(span, "an event list can't mix edges and levels");
            }
        }
        if items.is_empty() {
            return self.comb_block(body, scope, span);
        }
        // Edges: every one but the clock must be tested, in order, by the
        // leading if/else-if chain.
        let mut edges = Vec::new();
        for it in &items {
            let sig = self.lower_edge_signal(&it.expr, scope)?;
            edges.push((sig, it.edge.expect("filtered to edges"), &it.expr));
        }
        let mut stmt = unwrap_block(body);
        // `begin : name integer i; if (!rst) ...`: the reset test inside a
        // block of its own, which then scopes everything below.
        let mut scope = scope;
        if let StmtKind::Block {
            name,
            decls,
            params,
            stmts,
        } = &stmt.kind
        {
            if let [only] = &stmts[..] {
                scope = self.block_scope(name.as_ref(), decls, params, scope)?;
                stmt = unwrap_block(only);
            }
        }
        let mut resets = Vec::new();
        let n_async = edges.len() - 1;
        while resets.len() < n_async {
            let StmtKind::If { cond, then, els } = &stmt.kind else {
                bail!(
                    stmt.span,
                    "with {} edges in the event list, the block must start with `if` testing the asynchronous ones",
                    n_async + 1
                );
            };
            let Some((sig_e, level)) = reset_test(cond) else {
                bail!(
                    cond.span,
                    "expected a test of an asynchronous reset signal here"
                );
            };
            let sig = self.lower_edge_signal(sig_e, scope)?;
            let Some(k) = edges.iter().position(|(s, _, _)| *s == sig) else {
                bail!(cond.span, "this signal isn't in the block's event list");
            };
            let (_, edge, _) = edges.remove(k);
            if (edge == Edge::Pos) != level {
                bail!(
                    cond.span,
                    "a {} reset must be tested {} (the test doesn't match the edge)",
                    if edge == Edge::Pos {
                        "posedge"
                    } else {
                        "negedge"
                    },
                    if edge == Edge::Pos { "high" } else { "low" }
                );
            }
            let Some(els) = els else {
                bail!(
                    stmt.span,
                    "the reset test needs an `else` with the clocked logic"
                );
            };
            resets.push((
                Ctrl {
                    sig,
                    polarity: Polarity::from_bool(level),
                },
                &**then,
            ));
            stmt = unwrap_block(els);
        }
        let (clk_sig, clk_edge, _) = edges[0];
        let clk = Ctrl {
            sig: clk_sig,
            polarity: if clk_edge == Edge::Pos {
                Polarity::Pos
            } else {
                Polarity::Neg
            },
        };
        let mut reset_envs = Vec::new();
        for (ctrl, body) in resets {
            let mut env = Env::new(Mode::Clocked);
            self.prescan_snippets(body, scope, &mut env);
            self.exec(body, scope, &mut env)?;
            if let Some(w) = env.mem_writes.first() {
                bail!(w.span, "a memory can't be written by an asynchronous reset");
            }
            reset_envs.push((ctrl, env));
        }
        let mut env = Env::new(Mode::Clocked);
        self.prescan_snippets(stmt, scope, &mut env);
        self.exec(stmt, scope, &mut env)?;
        self.finish_mem_writes(&env, clk);
        let mut vars: Vec<usize> = env.assigned.keys().copied().collect();
        for (_, e) in &reset_envs {
            vars.extend(e.assigned.keys().copied());
        }
        vars.sort_unstable();
        vars.dedup();
        for v in vars {
            let w = self.vars[v].dim.width();
            let mut mask = vec![false; w];
            for e in std::iter::once(&env).chain(reset_envs.iter().map(|(_, e)| e)) {
                if let Some(m) = e.assigned.get(&v) {
                    for (i, &a) in m.iter().enumerate() {
                        mask[i] |= a;
                    }
                }
            }
            let bits: Vec<usize> = (0..w).filter(|&i| mask[i]).collect();
            let final_value = |e: &Env| -> SigSpec {
                let full = e
                    .values()
                    .get(&Slot::Var(v))
                    .cloned()
                    .unwrap_or_else(|| self.vars[v].nets.clone());
                bits.iter().map(|&i| full[i]).collect()
            };
            let d = final_value(&env);
            let nets = &self.vars[v].nets;
            let temp = env.is_blocking(v)
                && !env.reads_incoming.contains(&v)
                && !d.iter().any(|b| nets.contains(b))
                && reset_envs.is_empty();
            let resets = reset_envs
                .iter()
                .map(|(c, e)| (*c, final_value(e)))
                .collect();
            self.outs.push(ProcOut {
                var: v,
                bits,
                temp,
                span,
                kind: OutKind::Ff { clk, d, resets },
            });
        }
        Ok(())
    }

    /// A clock or reset in an event list: one bit.
    fn lower_edge_signal(&mut self, e: &'a Expr, scope: ScopeId) -> Result<SigBit> {
        let mut env = Env::new(Mode::Cont);
        let (v, _) = self.lower_self(e, scope, &mut env)?;
        if v.len() != 1 {
            bail!(e.span, "an edge must be on a one-bit signal");
        }
        Ok(v[0])
    }

    fn comb_block(&mut self, body: &'a Stmt, scope: ScopeId, span: Span) -> Result<()> {
        let mut env = Env::new(Mode::Comb);
        self.prescan_snippets(body, scope, &mut env);
        self.exec(body, scope, &mut env)?;
        // mem_write turns these into register writes before they get
        // here; a write port can only come from a clocked block.
        debug_assert!(env.mem_writes.is_empty());
        let mut vars: Vec<usize> = env.assigned.keys().copied().collect();
        vars.sort_unstable();
        for v in vars {
            let mask = &env.assigned[&v];
            let bits: Vec<usize> = (0..mask.len()).filter(|&i| mask[i]).collect();
            let full = env
                .values()
                .get(&Slot::Var(v))
                .cloned()
                .unwrap_or_else(|| self.vars[v].nets.clone());
            let en_full = env
                .values()
                .get(&Slot::Assigned(v))
                .cloned()
                .unwrap_or_else(|| vec![SigBit::ZERO; mask.len()]);
            let value: SigSpec = bits.iter().map(|&i| full[i]).collect();
            let en: SigSpec = bits.iter().map(|&i| en_full[i]).collect();
            let nets = &self.vars[v].nets;
            let temp = env.is_blocking(v)
                && !env.reads_incoming.contains(&v)
                && en.iter().all(|&b| b == SigBit::ONE)
                && !value.iter().any(|b| nets.contains(b));
            self.outs.push(ProcOut {
                var: v,
                bits,
                temp,
                span,
                kind: OutKind::Comb { value, en },
            });
        }
        Ok(())
    }

    pub fn initial(&mut self, body: &'a Stmt, scope: ScopeId) -> Result<()> {
        let mut env = Env::new(Mode::Initial);
        self.exec(body, scope, &mut env)?;
        let mut vars: Vec<usize> = env.assigned.keys().copied().collect();
        vars.sort_unstable();
        for v in vars {
            let val = env.values()[&Slot::Var(v)].clone();
            let Some(bits) = as_const(&val) else {
                bail!(
                    body.span,
                    "the initial value of `{}` is not a constant",
                    self.vars[v].name
                );
            };
            self.vars[v].init = Some(bits);
        }
        Ok(())
    }

    /// A declaration's initializer: `reg x = 0;`.
    pub fn declare_init(&mut self, v: usize, init: &'a Expr, scope: ScopeId) -> Result<()> {
        let c = self.eval_const(init, scope).map_err(|d| {
            Diag::new(
                d.span.unwrap_or(init.span),
                format!(
                    "the initial value of `{}`: {}",
                    self.vars[v].name, d.message
                ),
            )
        })?;
        let w = self.vars[v].dim.width();
        let bits = crate::fold::extend(&c.bits, w, c.signed);
        self.vars[v].init = Some(bits);
        Ok(())
    }

    fn finish_mem_writes(&mut self, env: &Env, clk: Ctrl) {
        let first = self.mem_port_base(env);
        for (k, w) in env.mem_writes.iter().enumerate() {
            let get = |id: usize| env.values().get(&Slot::Hidden(id)).cloned();
            let width = self.mems[w.mem].width();
            let en = get(w.en).unwrap_or_else(|| vec![SigBit::ZERO; width]);
            let addr = get(w.addr).expect("a write port's address is set where it's enabled");
            let data = get(w.data).unwrap_or_else(|| xs(width));
            // Later writes in this block win over earlier ones to the same
            // memory; writes from other blocks have no order.
            let earlier: Vec<usize> = env.mem_writes[..k]
                .iter()
                .enumerate()
                .filter(|(_, o)| o.mem == w.mem)
                .map(|(j, _)| first[&w.mem] + j)
                .collect();
            let MemStorage::Ram { wr, .. } = &mut self.mems[w.mem].storage else {
                unreachable!("only a RAM gets write ports");
            };
            let index = wr.len();
            let mut priority = vec![false; index + 1];
            for j in earlier {
                if j < index {
                    priority[j] = true;
                }
            }
            wr.push(MemWr {
                clk: Some(clk),
                en,
                addr,
                data,
                wide_continuation: false,
                priority,
            });
        }
    }

    /// The index each memory's next write port gets, before this block's.
    fn mem_port_base(&self, env: &Env) -> rustc_hash::FxHashMap<usize, usize> {
        let mut m = rustc_hash::FxHashMap::default();
        for w in &env.mem_writes {
            m.entry(w.mem)
                .or_insert_with(|| match &self.mems[w.mem].storage {
                    MemStorage::Ram { wr, .. } => wr.len(),
                    MemStorage::Regs(_) => 0,
                });
        }
        m
    }

    /// Turns the recorded block outputs into cells and connections.
    pub fn finish_processes(&mut self) -> Result<()> {
        let outs = std::mem::take(&mut self.outs);
        for (v, value) in std::mem::take(&mut self.loop_finals) {
            if outs.iter().all(|o| o.var != v) {
                let nets = self.vars[v].nets.clone();
                let span = self.vars[v].span;
                self.b.join_sig(&nets, &konst(&value), span);
            }
        }
        // Which outputs drive each bit: a temporary yields to anything
        // else, several non-temporaries are an error.
        let mut by_var: FxHashMap<usize, Vec<usize>> = Default::default();
        for (i, o) in outs.iter().enumerate() {
            by_var.entry(o.var).or_default().push(i);
        }
        let mut skip = vec![false; outs.len()];
        for (&v, list) in &by_var {
            if list.len() < 2 {
                continue;
            }
            let real: Vec<usize> = list.iter().copied().filter(|&i| !outs[i].temp).collect();
            for &i in list {
                if outs[i].temp {
                    skip[i] = true;
                }
            }
            if real.len() > 1 {
                // Different bits from different blocks are fine.
                let mut seen = vec![false; self.vars[v].dim.width()];
                for &i in &real {
                    for &b in &outs[i].bits {
                        if seen[b] {
                            bail!(
                                self.vars[v].span,
                                "`{}` is assigned in more than one always block",
                                self.vars[v].name
                            );
                        }
                        seen[b] = true;
                    }
                }
            }
        }
        // What drives each variable bit, for its initial value: a register
        // keeps it as `init`, logic drops it, and a variable nothing
        // drives is that constant (yosys 0.70's proc does the same).
        let mut by_ff: FxHashMap<usize, Vec<bool>> = FxHashMap::default();
        let mut driven: FxHashMap<usize, Vec<bool>> = FxHashMap::default();
        for (o, &skip) in outs.iter().zip(&skip) {
            if skip {
                continue;
            }
            let w = self.vars[o.var].dim.width();
            let d = driven.entry(o.var).or_insert_with(|| vec![false; w]);
            for &b in &o.bits {
                d[b] = true;
            }
            if let OutKind::Ff { .. } = o.kind {
                let f = by_ff.entry(o.var).or_insert_with(|| vec![false; w]);
                for &b in &o.bits {
                    f[b] = true;
                }
            }
        }
        for v in 0..self.vars.len() {
            let Some(init) = self.vars[v].init.clone() else {
                continue;
            };
            if self.vars[v].cont_driven || self.vars[v].port == Some(chipsmith_netlist::Dir::In) {
                continue;
            }
            let w = init.len();
            let ff = by_ff.get(&v).cloned().unwrap_or_else(|| vec![false; w]);
            let dr = driven.get(&v).cloned().unwrap_or_else(|| vec![false; w]);
            let mut kept = vec![State::X; w];
            for i in 0..w {
                if ff[i] {
                    kept[i] = init[i];
                } else if !dr[i] && init[i] != State::X {
                    let n = self.vars[v].nets[i];
                    let span = self.vars[v].span;
                    self.b.join(n, SigBit::Const(init[i]), span);
                }
            }
            self.vars[v].init = kept.iter().any(|&b| b != State::X).then_some(kept);
        }
        for (o, skip) in outs.into_iter().zip(skip) {
            if skip {
                continue;
            }
            let q: SigSpec = o.bits.iter().map(|&i| self.vars[o.var].nets[i]).collect();
            match o.kind {
                OutKind::Comb { value, en } => {
                    let mut latch: Vec<(SigBit, Vec<usize>)> = Vec::new();
                    for (k, &e) in en.iter().enumerate() {
                        if e == SigBit::ONE {
                            self.b.join(q[k], value[k], o.span);
                        } else {
                            match latch.iter_mut().find(|(s, _)| *s == e) {
                                Some((_, ks)) => ks.push(k),
                                None => latch.push((e, vec![k])),
                            }
                        }
                    }
                    for (e, ks) in latch {
                        let ff = Ff::<Word> {
                            q: ks.iter().map(|&k| q[k]).collect(),
                            kind: FfKind::Dlatch {
                                en: Ctrl::pos(e),
                                d: ks.iter().map(|&k| value[k]).collect(),
                                reset: LatchReset::None,
                            },
                        };
                        self.b
                            .add("$dlatch", chipsmith_netlist::CellKind::Ff(ff), o.span);
                    }
                }
                OutKind::Ff { clk, d, resets } => self.build_ff(q, clk, d, resets, o.span)?,
            }
        }
        Ok(())
    }

    fn build_ff(
        &mut self,
        q: SigSpec,
        clk: Ctrl,
        d: SigSpec,
        resets: Vec<(Ctrl, SigSpec)>,
        span: Span,
    ) -> Result<()> {
        let ff = |q: SigSpec, d: SigSpec, control| Ff::<Word> {
            q,
            kind: FfKind::Dff { clk, d, control },
        };
        let cell = |b: &mut crate::build::Builder, name: &str, f: Ff<Word>| {
            b.add(name, chipsmith_netlist::CellKind::Ff(f), span);
        };
        match resets.len() {
            0 => cell(
                &mut self.b,
                "$dff",
                ff(q, d, DffControl::Plain { en: None }),
            ),
            1 => {
                let (arst, value) = &resets[0];
                // Constant reset bits in an $adff, the rest loaded
                // asynchronously from their (signal) reset value.
                let (konst_bits, load_bits): (Vec<usize>, Vec<usize>) =
                    (0..q.len()).partition(|&k| matches!(value[k], SigBit::Const(_)));
                if !konst_bits.is_empty() {
                    let pick = |s: &SigSpec| konst_bits.iter().map(|&k| s[k]).collect::<SigSpec>();
                    let val = as_const(&pick(value)).expect("constant bits");
                    cell(
                        &mut self.b,
                        "$adff",
                        ff(
                            pick(&q),
                            pick(&d),
                            DffControl::Async {
                                arst: *arst,
                                value: chipsmith_netlist::Const(val),
                                en: None,
                            },
                        ),
                    );
                }
                if !load_bits.is_empty() {
                    let pick = |s: &SigSpec| load_bits.iter().map(|&k| s[k]).collect::<SigSpec>();
                    cell(
                        &mut self.b,
                        "$aldff",
                        ff(
                            pick(&q),
                            pick(&d),
                            DffControl::Load {
                                aload: *arst,
                                ad: pick(value),
                                en: None,
                            },
                        ),
                    );
                }
            }
            _ => {
                // Per bit: set where the first active reset forces a 1,
                // clear where it forces a 0.
                let mut set = Vec::new();
                let mut clr = Vec::new();
                for k in 0..q.len() {
                    let mut s = SigBit::ZERO;
                    let mut c = SigBit::ZERO;
                    for (ctrl, value) in resets.iter().rev() {
                        let active = if ctrl.polarity.is_pos() {
                            ctrl.sig
                        } else {
                            self.b.not1(ctrl.sig, span)
                        };
                        let inactive = self.b.not1(active, span);
                        let (vs, vc) = match value[k] {
                            SigBit::ONE => (SigBit::ONE, SigBit::ZERO),
                            SigBit::ZERO => (SigBit::ZERO, SigBit::ONE),
                            b if b == q[k] => (SigBit::ZERO, SigBit::ZERO),
                            _ => bail!(
                                span,
                                "with more than one asynchronous reset, reset values must be constants"
                            ),
                        };
                        // This reset, or (if inactive) the lower ones.
                        let s_hold = self.b.and1(inactive, s, span);
                        let s_here = self.b.and1(active, vs, span);
                        s = self.b.or1(s_here, s_hold, span);
                        let c_hold = self.b.and1(inactive, c, span);
                        let c_here = self.b.and1(active, vc, span);
                        c = self.b.or1(c_here, c_hold, span);
                    }
                    set.push(s);
                    clr.push(c);
                }
                cell(
                    &mut self.b,
                    "$dffsr",
                    ff(
                        q,
                        d,
                        DffControl::SetClr {
                            sr: SetClr {
                                set,
                                set_polarity: Polarity::Pos,
                                clr,
                                clr_polarity: Polarity::Pos,
                            },
                            en: None,
                        },
                    ),
                );
            }
        }
        Ok(())
    }

    /// The `$mem_v2` cells, at the end of the module.
    pub fn finish_memories(&mut self) {
        for m in 0..self.mems.len() {
            let mem = &mut self.mems[m];
            let MemStorage::Ram { rd, wr } = &mut mem.storage else {
                continue;
            };
            let (mut rd, mut wr) = (std::mem::take(rd), std::mem::take(wr));
            if rd.is_empty() && wr.is_empty() {
                continue;
            }
            let size = mem.size();
            let min_abits = (usize::BITS - (size.max(2) - 1).leading_zeros()) as usize;
            let abits = rd
                .iter()
                .map(|r| r.addr.len())
                .chain(wr.iter().map(|w| w.addr.len()))
                .max()
                .unwrap_or(0)
                .max(min_abits);
            for r in &mut rd {
                r.addr.resize(abits, SigBit::ZERO);
            }
            for w in &mut wr {
                w.addr.resize(abits, SigBit::ZERO);
            }
            let n = wr.len();
            for w in &mut wr {
                w.priority.resize(n, false);
            }
            let cell = Mem {
                memid: format!("\\{}", mem.name),
                size,
                offset: mem.lo,
                width: mem.width(),
                abits,
                init: chipsmith_netlist::Const(mem.init.clone()),
                rd,
                wr,
            };
            let span = mem.span;
            let attrs = mem.attrs.clone();
            let name = mem.name.clone();
            let c = self
                .b
                .add_named(name, chipsmith_netlist::CellKind::Mem(cell), span);
            c.attributes.extend(attrs);
        }
    }
}

pub(crate) fn mem_read(
    m: &mut ModElab<'_, '_>,
    mem: usize,
    addr: SigSpec,
    env: &mut Env,
    span: Span,
) -> Result<SigSpec> {
    let width = m.mems[mem].width();
    match env.mode {
        Mode::Const => bail!(span, "memory `{}` is not a constant", m.mems[mem].name),
        Mode::Initial => {
            let Some(a) = as_const(&addr).and_then(|c| addr_value(&c)) else {
                bail!(
                    span,
                    "a memory read in an initial block needs a constant address"
                );
            };
            let mv = &m.mems[mem];
            if a < mv.lo || a > mv.hi {
                return Ok(xs(width));
            }
            let i = (a - mv.lo) as usize;
            return Ok(konst(&mv.init[i * width..(i + 1) * width]));
        }
        _ => {}
    }
    match &m.mems[mem].storage {
        MemStorage::Ram { .. } => {
            let data = m.b.new_sig(width);
            let MemStorage::Ram { rd, .. } = &mut m.mems[mem].storage else {
                unreachable!()
            };
            rd.push(MemRd {
                addr,
                data: data.clone(),
                wide_continuation: false,
                sync: None,
            });
            Ok(data)
        }
        MemStorage::Regs(words) => {
            let words = words.clone();
            let lo = m.mems[mem].lo;
            let mut cases = Vec::new();
            if let Some(a) = as_const(&addr).and_then(|c| addr_value(&c)) {
                return match usize::try_from(a - lo).ok().and_then(|i| words.get(i)) {
                    Some(&w) => m.read_var(w, env, span),
                    None => Ok(xs(width)),
                };
            }
            for (i, &w) in words.iter().enumerate() {
                let hit = word_hit(m, &addr, lo + i as i64, span);
                let v = m.read_var(w, env, span)?;
                cases.push((hit, v));
            }
            Ok(m.b.pmux(xs(width), cases, span))
        }
    }
}

fn addr_value(c: &[State]) -> Option<i64> {
    super::ConstVal {
        bits: c.to_vec(),
        signed: false,
        string: false,
    }
    .as_i64()
}

fn word_hit(m: &mut ModElab<'_, '_>, addr: &SigSpec, index: i64, span: Span) -> SigBit {
    let w = addr
        .len()
        .max(64 - index.unsigned_abs().leading_zeros() as usize + 1);
    let a = crate::build::extend(addr, w, false);
    let k = konst(&chipsmith_netlist::Const::from_int(index, w).0);
    m.b.binary(chipsmith_netlist::BinaryOp::Eq, a, false, k, false, 1, span)[0]
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn mem_write(
    m: &mut ModElab<'_, '_>,
    mem: usize,
    addr: SigSpec,
    v: SigSpec,
    (lo_bit, n): (usize, usize),
    blocking: bool,
    env: &mut Env,
    span: Span,
) -> Result<()> {
    let width = m.mems[mem].width();
    match env.mode {
        Mode::Initial => {
            let Some(a) = as_const(&addr).and_then(|c| addr_value(&c)) else {
                bail!(
                    span,
                    "a memory write in an initial block needs a constant address"
                );
            };
            let Some(vals) = as_const(&v) else {
                bail!(span, "initial memory contents must be constants");
            };
            let mv = &mut m.mems[mem];
            if a < mv.lo || a > mv.hi {
                return Ok(());
            }
            let i = (a - mv.lo) as usize;
            mv.init[i * width + lo_bit..i * width + lo_bit + n].copy_from_slice(&vals);
            return Ok(());
        }
        Mode::Const | Mode::Cont => {
            bail!(span, "memory `{}` can't be written here", m.mems[mem].name)
        }
        Mode::Comb | Mode::Clocked => {}
    }
    match &m.mems[mem].storage {
        MemStorage::Regs(words) => {
            let words = words.clone();
            let lo = m.mems[mem].lo;
            let positions: Vec<Option<usize>> = (lo_bit..lo_bit + n).map(Some).collect();
            let direct = as_const(&addr).and_then(|c| addr_value(&c));
            for (i, &w) in words.iter().enumerate() {
                let hit = match direct {
                    Some(a) if a == lo + i as i64 => SigBit::ONE,
                    Some(_) => SigBit::ZERO,
                    None => word_hit(m, &addr, lo + i as i64, span),
                };
                if hit == SigBit::ZERO {
                    continue;
                }
                let cur = m.target_value(w, env);
                let old: SigSpec = cur[lo_bit..lo_bit + n].to_vec();
                let new = m.b.mux(hit, old, v.clone(), span);
                m.write_word(w, positions.clone(), new, blocking, env, span)?;
            }
            Ok(())
        }
        MemStorage::Ram { .. } => {
            // Memories written like this are found by name before
            // elaboration and kept as registers; a write the scan can't
            // see (from inside a task) isn't supported.
            if env.mode == Mode::Comb || blocking {
                bail!(
                    span,
                    "memory `{}` is written from combinational logic or with `=` where that can't be seen before elaboration (inside a task?); mark it (* mem2reg *)",
                    m.mems[mem].name
                );
            }
            let id = m.hidden;
            m.hidden += 3;
            env.new_hidden(id, vec![SigBit::ZERO; width]);
            env.new_hidden(id + 1, xs(addr.len()));
            env.new_hidden(id + 2, xs(width));
            let mut en = vec![SigBit::ZERO; width];
            let mut data = xs(width);
            for k in 0..n {
                en[lo_bit + k] = SigBit::ONE;
                data[lo_bit + k] = v[k];
            }
            env.set(Slot::Hidden(id), en);
            env.set(Slot::Hidden(id + 1), addr);
            env.set(Slot::Hidden(id + 2), data);
            env.mem_writes.push(MemWrite {
                mem,
                en: id,
                addr: id + 1,
                data: id + 2,
                span,
            });
            Ok(())
        }
    }
}

/// `$readmemh("file", mem [, start [, end]])` in an initial block.
pub(crate) fn readmem<'a>(
    m: &mut ModElab<'a, '_>,
    hex: bool,
    args: &'a [Expr],
    scope: ScopeId,
    span: Span,
) -> Result<()> {
    let (Some(file), Some(target)) = (args.first(), args.get(1)) else {
        bail!(span, "$readmem needs a file name and a memory");
    };
    let ExprKind::Str(name) = &file.kind else {
        bail!(file.span, "$readmem's file name must be a string literal");
    };
    let name = String::from_utf8_lossy(name).into_owned();
    let ExprKind::Ident(id) = &target.kind else {
        bail!(target.span, "$readmem's second argument must be a memory");
    };
    let Sym::Mem(mem) = m.resolve(scope, id)? else {
        bail!(target.span, "`{}` is not a memory", id.name);
    };
    // Relative to the working directory, else to the source file (yosys
    // tries both).
    let src = m.b.sources.path(span).to_owned();
    let path = [
        std::path::PathBuf::from(&name),
        crate::source::sibling(&src, &name),
    ]
    .into_iter()
    .find(|p| p.is_file());
    let Some(path) = path else {
        bail!(file.span, "$readmem: file {name} not found");
    };
    let text = std::fs::read_to_string(&path).map_err(|e| {
        Diag::new(
            file.span,
            format!("$readmem: reading {}: {e}", path.display()),
        )
    })?;
    let (lo, hi) = (m.mems[mem].lo, m.mems[mem].hi);
    let start = match args.get(2) {
        Some(e) => m.eval_i64(e, scope)?,
        None => lo,
    };
    let end = match args.get(3) {
        Some(e) => Some(m.eval_i64(e, scope)?),
        None => None,
    };
    let width = m.mems[mem].width();
    let mut addr = start;
    let mut cleaned = String::new();
    let mut in_block = false;
    for line in text.lines() {
        let mut l = line;
        if in_block {
            match l.find("*/") {
                Some(i) => {
                    l = &l[i + 2..];
                    in_block = false;
                }
                None => continue,
            }
        }
        let l = l.split("//").next().unwrap_or("");
        let l = match l.find("/*") {
            Some(i) => {
                in_block = !l[i..].contains("*/");
                &l[..i]
            }
            None => l,
        };
        cleaned.push_str(l);
        cleaned.push('\n');
    }
    for tok in cleaned.split_whitespace() {
        if let Some(a) = tok.strip_prefix('@') {
            addr = i64::from_str_radix(a, 16)
                .map_err(|_| Diag::new(file.span, format!("$readmem: bad address {tok}")))?;
            continue;
        }
        if end.is_some_and(|e| addr > e) || addr > hi {
            break;
        }
        let mut bits = Vec::new();
        for c in tok.chars().rev().filter(|&c| c != '_') {
            let n = if hex { 4 } else { 1 };
            let st: Vec<State> = match c.to_ascii_lowercase() {
                'x' => vec![State::X; n],
                'z' => vec![State::Z; n],
                c => {
                    let d = c.to_digit(if hex { 16 } else { 2 }).ok_or_else(|| {
                        Diag::new(file.span, format!("$readmem: bad digit in {tok}"))
                    })?;
                    (0..n).map(|i| State::from(d >> i & 1 == 1)).collect()
                }
            };
            bits.extend(st);
        }
        bits.resize(width, State::Zero);
        if addr >= lo {
            let i = (addr - lo) as usize;
            m.mems[mem].init[i * width..(i + 1) * width].copy_from_slice(&bits);
        }
        addr += 1;
    }
    Ok(())
}

impl<'a, 'd> ModElab<'a, 'd> {
    fn target_value(&self, v: usize, env: &Env) -> SigSpec {
        env.get(Slot::Var(v))
            .cloned()
            .unwrap_or_else(|| self.vars[v].nets.clone())
    }
}
