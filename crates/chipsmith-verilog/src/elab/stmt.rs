//! Procedural code, run symbolically.
//!
//! An [`Env`] maps every variable a procedure has assigned so far to the
//! signal that holds its value at this point. An `if` or `case` runs each
//! branch on top of the current values (a frame each) and then merges:
//! every slot a branch changed gets a multiplexer, built the way yosys'
//! `proc_mux` builds it (a `$mux` per priority level, a `$pmux` for cases
//! that can't overlap), so the same netlist comes out. Loops unroll; their
//! conditions must fold to constants.
//!
//! Blocking and non-blocking assignments differ only in what a later read
//! sees: a variable assigned with `=` reads back its assigned value, one
//! assigned with `<=` reads the value it had when the block started. A
//! variable assigned both ways in one block is an error rather than one of
//! the tools' differing interpretations.

use chipsmith_netlist::{SigBit, SigSpec, State};
use rustc_hash::{FxHashMap, FxHashSet};

use super::{ConstVal, Dim, ModElab, ScopeId, Sym, Ty};
use crate::ast::{CaseKind, Expr, ExprKind, Stmt, StmtKind};
use crate::build::{as_const, konst, xs};
use crate::source::{bail, Diag, Result, Span};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum Slot {
    Var(usize),
    Local(usize),
    /// A value the lowering keeps for itself: a memory write port's
    /// enable, address or data.
    Hidden(usize),
    /// Which bits of a variable a combinational block assigned on the path
    /// taken: where not all, it's a latch with this as its enable.
    Assigned(usize),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Mode {
    /// A constant expression: only parameters can be read.
    Const,
    /// A continuous assignment: variables read as their nets.
    Cont,
    /// `always @*`
    Comb,
    /// `always @(posedge ...)`
    Clocked,
    /// `initial`: everything must fold to constants.
    Initial,
}

/// One memory write a clocked block made, its signals in hidden slots.
#[derive(Debug, Clone)]
pub(crate) struct MemWrite {
    pub mem: usize,
    pub en: usize,
    pub addr: usize,
    pub data: usize,
    pub span: Span,
}

pub(crate) struct Env {
    pub mode: Mode,
    frames: Vec<FxHashMap<Slot, SigSpec>>,
    /// Default values of hidden slots not yet assigned on a path.
    hidden_init: FxHashMap<usize, SigSpec>,
    pub blocking: FxHashSet<usize>,
    pub nonblocking: FxHashSet<usize>,
    /// Bits of each variable the block assigns somewhere.
    pub assigned: FxHashMap<usize, Vec<bool>>,
    /// Variables whose value from before the block was read: not
    /// temporaries of this block.
    pub reads_incoming: FxHashSet<usize>,
    pub mem_writes: Vec<MemWrite>,
    /// Loop iterations so far, to stop a runaway loop.
    pub iterations: usize,
    /// Per variable, which bits the block's assignments always write
    /// together (a class id per bit): yosys' "snippets", which it builds
    /// a multiplexer tree for each of.
    pub snippets: FxHashMap<usize, Vec<u32>>,
}

const MAX_ITERATIONS: usize = 1 << 20;

impl Env {
    pub fn new(mode: Mode) -> Env {
        Env {
            mode,
            frames: vec![FxHashMap::default()],
            hidden_init: FxHashMap::default(),
            blocking: FxHashSet::default(),
            nonblocking: FxHashSet::default(),
            assigned: FxHashMap::default(),
            reads_incoming: FxHashSet::default(),
            mem_writes: Vec::new(),
            iterations: 0,
            snippets: FxHashMap::default(),
        }
    }

    pub fn get(&self, slot: Slot) -> Option<&SigSpec> {
        self.frames.iter().rev().find_map(|f| f.get(&slot))
    }

    pub fn set(&mut self, slot: Slot, v: SigSpec) {
        self.frames
            .last_mut()
            .expect("an env has a frame")
            .insert(slot, v);
    }

    pub fn push(&mut self) {
        self.frames.push(FxHashMap::default());
    }

    pub fn pop(&mut self) -> FxHashMap<Slot, SigSpec> {
        self.frames.pop().expect("a pushed frame")
    }

    /// Everything a popped frame assigned, as if it had run here.
    pub fn absorb(&mut self, frame: FxHashMap<Slot, SigSpec>) {
        self.frames
            .last_mut()
            .expect("an env has a frame")
            .extend(frame);
    }

    pub fn new_hidden(&mut self, id: usize, init: SigSpec) {
        self.hidden_init.insert(id, init);
    }

    /// The final values: the bottom frame.
    pub fn values(&self) -> &FxHashMap<Slot, SigSpec> {
        &self.frames[0]
    }
}

/// One way out of an `if` or `case`.
struct Arm<'a> {
    /// Set when this arm is taken (given that no earlier group's was).
    ctrl: SigBit,
    /// Arms in one group can't overlap and share a `$pmux`; a new group
    /// is a new priority level (yosys' `pgroups`).
    group: usize,
    body: &'a Stmt,
}

/// What an assignment's left-hand side names.
enum Target {
    /// Bits of a variable or local, by position; `None` drops a bit that's
    /// out of range.
    Bits { slot: Slot, positions: Vec<Option<usize>> },
    /// A variable bit-select with a variable index.
    Dynamic {
        slot: Slot,
        dim: Dim,
        index: SigSpec,
        index_ty: Ty,
        width: usize,
        /// Index delta of the lowest selected index (for `-:`).
        delta: i64,
    },
    /// Bits of a memory word: the lowest and how many.
    Mem {
        mem: usize,
        addr: SigSpec,
        bits: (usize, usize),
    },
    Concat(Vec<Target>),
}

impl Target {
    fn width(&self) -> usize {
        match self {
            Target::Bits { positions, .. } => positions.len(),
            Target::Dynamic { width, .. } => *width,
            Target::Mem { bits: (_, w), .. } => *w,
            Target::Concat(parts) => parts.iter().map(Target::width).sum(),
        }
    }
}

impl<'a, 'd> ModElab<'a, 'd> {
    pub fn exec(&mut self, s: &'a Stmt, scope: ScopeId, env: &mut Env) -> Result<()> {
        match &s.kind {
            StmtKind::Null => Ok(()),
            StmtKind::Block {
                name,
                decls,
                params,
                stmts,
            } => {
                let inner = if decls.is_empty() && params.is_empty() && name.is_none() {
                    scope
                } else {
                    let prefix = match name {
                        Some(n) => format!("{}{}.", self.scopes[scope].prefix, n.name),
                        None => self.scopes[scope].prefix.clone(),
                    };
                    let inner = self.new_scope(Some(scope), prefix);
                    if let Some(n) = name {
                        self.scopes[scope].blocks.insert(n.name.to_string(), inner);
                    }
                    for p in params {
                        self.declare_param(inner, p)?;
                    }
                    for d in decls {
                        self.declare_block_vars(inner, d, env)?;
                    }
                    inner
                };
                for st in stmts {
                    self.exec(st, inner, env)?;
                }
                Ok(())
            }
            StmtKind::Assign { lhs, rhs, blocking } => self.assign(lhs, rhs, *blocking, scope, env, s.span),
            StmtKind::If { cond, then, els } => {
                let c = self.lower_bool(cond, scope, env)?;
                let arms = vec![Arm {
                    ctrl: c,
                    group: 0,
                    body: then,
                }];
                self.switch(arms, els.as_deref(), scope, env, s.span)
            }
            StmtKind::Case { kind, expr, items } => {
                let parallel = has_attr(&s.attrs, "parallel_case");
                let full = has_attr(&s.attrs, "full_case");
                self.case(*kind, expr, items, parallel, full, scope, env, s.span)
            }
            StmtKind::For {
                init,
                cond,
                step,
                body,
            } => {
                self.exec(init, scope, env)?;
                loop {
                    if !self.loop_cond(cond, scope, env)? {
                        break;
                    }
                    self.exec(body, scope, env)?;
                    self.exec(step, scope, env)?;
                }
                // The loop variable unrolls away: like yosys, it ends up
                // a constant (its final value), not a register or an
                // initial value.
                if let StmtKind::Assign { lhs, .. } = &init.kind {
                    if let ExprKind::Ident(id) = &lhs.kind {
                        if let Some((Sym::Var(v), _)) = self.lookup(scope, &id.name) {
                            if let Some(c) = env.get(Slot::Var(v)).and_then(|x| as_const(x)) {
                                env.assigned.remove(&v);
                                self.loop_finals.insert(v, c);
                            }
                        }
                    }
                }
                Ok(())
            }
            StmtKind::While { cond, body } => loop {
                if !self.loop_cond(cond, scope, env)? {
                    return Ok(());
                }
                self.exec(body, scope, env)?;
            },
            StmtKind::Repeat { count, body } => {
                let (v, t) = self.lower_self(count, scope, env)?;
                let Some(n) = as_const(&v).and_then(|bits| {
                    ConstVal {
                        bits,
                        signed: t.signed,
                        string: false,
                    }
                    .as_i64()
                }) else {
                    bail!(count.span, "a repeat count must be a constant");
                };
                for _ in 0..n.max(0) {
                    self.bump_iterations(env, s.span)?;
                    self.exec(body, scope, env)?;
                }
                Ok(())
            }
            StmtKind::Event { .. } => bail!(
                s.span,
                "an event control is only supported at the start of an always block"
            ),
            StmtKind::TaskCall { name, args } => self.call_task(name, args, scope, env, s.span),
            StmtKind::SysTaskCall { name, args } => self.sys_task(name, args, scope, env, s.span),
            StmtKind::Unsupported(what) => bail!(s.span, "{what} is not supported"),
        }
    }

    fn bump_iterations(&mut self, env: &mut Env, span: Span) -> Result<()> {
        env.iterations += 1;
        if env.iterations > MAX_ITERATIONS {
            bail!(span, "loop runs more than {MAX_ITERATIONS} iterations");
        }
        Ok(())
    }

    fn loop_cond(&mut self, cond: &'a Expr, scope: ScopeId, env: &mut Env) -> Result<bool> {
        self.bump_iterations(env, cond.span)?;
        let c = self.lower_bool(cond, scope, env)?;
        match c {
            SigBit::ONE => Ok(true),
            SigBit::ZERO => Ok(false),
            _ => bail!(
                cond.span,
                "loop condition doesn't fold to a constant (loops must unroll)"
            ),
        }
    }

    fn sys_task(
        &mut self,
        name: &'a crate::ast::Ident,
        args: &'a [Expr],
        scope: ScopeId,
        env: &mut Env,
        span: Span,
    ) -> Result<()> {
        match &*name.name {
            // Simulation output has no hardware.
            "display" | "displayb" | "displayh" | "displayo" | "write" | "writeb" | "writeh"
            | "writeo" | "strobe" | "monitor" | "finish" | "stop" | "info" | "warning"
            | "error" | "fatal" | "dumpfile" | "dumpvars" | "dumpon" | "dumpoff" | "fflush" => {
                Ok(())
            }
            "readmemh" | "readmemb" => {
                if env.mode != Mode::Initial {
                    bail!(span, "${} is only supported in an initial block", name.name);
                }
                super::proc::readmem(self, &*name.name == "readmemh", args, scope, span)
            }
            other => bail!(name.span, "system task ${other} is not supported"),
        }
    }

    fn call_task(
        &mut self,
        name: &'a crate::ast::Ident,
        args: &'a [Expr],
        scope: ScopeId,
        env: &mut Env,
        span: Span,
    ) -> Result<()> {
        let Sym::Task(t, def) = self.resolve_callable(scope, name)? else {
            bail!(name.span, "`{}` is not a task", name.name);
        };
        if self.depth > 256 {
            bail!(span, "task {} recurses too deeply", name.name);
        }
        let tscope = self.new_scope(Some(def), String::new());
        for p in &t.params {
            self.declare_param(tscope, p)?;
        }
        let mut formals = Vec::new();
        for d in &t.decls {
            for n in &d.names {
                let (dim, signed) = self.decl_dim(&d.ty, tscope)?;
                let l = self.new_local(tscope, &n.name.name, dim, signed, n.name.span);
                if let Some(dir) = d.dir {
                    formals.push((l, dir));
                }
            }
        }
        if formals.len() != args.len() {
            bail!(
                span,
                "task {} takes {} arguments, given {}",
                name.name,
                formals.len(),
                args.len()
            );
        }
        let mut inputs = Vec::new();
        for (&(l, dir), a) in formals.iter().zip(args) {
            if dir != crate::ast::PortDir::Output {
                let lt = self.locals[l].ty();
                let at = self.self_ty(a, scope)?;
                let mut v = self.lower(
                    a,
                    Ty {
                        width: lt.width.max(at.width),
                        signed: at.signed,
                    },
                    scope,
                    env,
                )?;
                v.truncate(lt.width);
                inputs.push((l, v));
            }
        }
        env.push();
        for (l, v) in inputs {
            env.set(Slot::Local(l), v);
        }
        self.depth += 1;
        let r = self.exec(&t.body, tscope, env);
        self.depth -= 1;
        r?;
        let mut outs = Vec::new();
        for (&(l, dir), a) in formals.iter().zip(args) {
            if dir != crate::ast::PortDir::Input {
                let v = env
                    .get(Slot::Local(l))
                    .cloned()
                    .unwrap_or_else(|| xs(self.locals[l].dim.width()));
                outs.push((a, v, self.locals[l].signed));
            }
        }
        let frame = env.pop();
        env.absorb(frame);
        for (a, v, signed) in outs {
            let target = self.target(a, scope, env)?;
            let w = target.width();
            let v = crate::build::extend(&v, w, signed);
            self.write_target(target, v, true, env, span)?;
        }
        Ok(())
    }

    // -----------------------------------------------------------------
    // Assignments

    fn assign(
        &mut self,
        lhs: &'a Expr,
        rhs: &'a Expr,
        blocking: bool,
        scope: ScopeId,
        env: &mut Env,
        span: Span,
    ) -> Result<()> {
        let target = self.target(lhs, scope, env)?;
        let lw = target.width();
        let rt = self.self_ty(rhs, scope)?;
        // The right side is sized to the wider of the two, signed if it
        // is; the left side takes the low bits (IEEE 1364-2005 §5.4.1).
        let mut v = self.lower(
            rhs,
            Ty {
                width: lw.max(rt.width),
                signed: rt.signed,
            },
            scope,
            env,
        )?;
        v.truncate(lw);
        self.write_target(target, v, blocking, env, span)
    }

    /// Resolves an assignment's left-hand side.
    fn target(&mut self, lhs: &'a Expr, scope: ScopeId, env: &mut Env) -> Result<Target> {
        match &lhs.kind {
            ExprKind::Concat(parts) => {
                let mut ts = Vec::new();
                for p in parts {
                    ts.push(self.target(p, scope, env)?);
                }
                Ok(Target::Concat(ts))
            }
            ExprKind::Ident(_) | ExprKind::Hier(_) => {
                let sym = match &lhs.kind {
                    ExprKind::Ident(id) => self.resolve(scope, id)?,
                    ExprKind::Hier(p) => self.resolve_hier(scope, p, lhs.span)?,
                    _ => unreachable!(),
                };
                let (slot, dim) = self.slot_of(&sym, lhs.span)?;
                Ok(Target::Bits {
                    slot,
                    positions: (0..dim.width()).map(Some).collect(),
                })
            }
            ExprKind::Index { base, index } => {
                if let Some(mem) = self.mem_of(base, scope)? {
                    let (addr, _) = self.lower_self(index, scope, env)?;
                    let w = self.mems[mem].width();
                    return Ok(Target::Mem {
                        mem,
                        addr,
                        bits: (0, w),
                    });
                }
                if let ExprKind::Index { base: inner, index: addr_e } = &base.kind {
                    if let Some(mem) = self.mem_of(inner, scope)? {
                        // mem[a][b] = ...
                        let (addr, _) = self.lower_self(addr_e, scope, env)?;
                        let i = self.eval_i64(index, scope).map_err(|_| {
                            Diag::new(index.span, "a memory word's bit-select must be constant when written")
                        })?;
                        let Some(p) = self.mems[mem].word.pos(i) else {
                            bail!(index.span, "bit {i} is outside the memory word");
                        };
                        return Ok(Target::Mem {
                            mem,
                            addr,
                            bits: (p, 1),
                        });
                    }
                }
                let (slot, dim) = self.base_slot(base, scope)?;
                let (idx, t) = self.lower_self(index, scope, env)?;
                if let Some(c) = as_const(&idx) {
                    let i = ConstVal {
                        bits: c,
                        signed: t.signed,
                        string: false,
                    }
                    .as_i64();
                    return Ok(Target::Bits {
                        slot,
                        positions: vec![i.and_then(|i| dim.pos(i))],
                    });
                }
                Ok(Target::Dynamic {
                    slot,
                    dim,
                    index: idx,
                    index_ty: t,
                    width: 1,
                    delta: 0,
                })
            }
            ExprKind::Slice { base, msb, lsb } => {
                let m = self.eval_i64(msb, scope)?;
                let l = self.eval_i64(lsb, scope)?;
                if let ExprKind::Index { base: inner, index: addr_e } = &base.kind {
                    if let Some(mem) = self.mem_of(inner, scope)? {
                        let (addr, _) = self.lower_self(addr_e, scope, env)?;
                        let word = self.mems[mem].word;
                        let (Some(a), Some(b)) = (word.pos(m), word.pos(l)) else {
                            bail!(lhs.span, "part-select [{m}:{l}] is outside the memory word");
                        };
                        return Ok(Target::Mem {
                            mem,
                            addr,
                            bits: (a.min(b), a.abs_diff(b) + 1),
                        });
                    }
                }
                let (slot, dim) = self.base_slot(base, scope)?;
                let (a, b) = (dim.pos_unchecked(l), dim.pos_unchecked(m));
                let positions = (a.min(b)..=a.max(b))
                    .map(|p| (0..dim.width() as i64).contains(&p).then_some(p as usize))
                    .collect();
                Ok(Target::Bits { slot, positions })
            }
            ExprKind::IndexedSlice {
                base,
                start,
                width,
                up,
            } => {
                let w = self.eval_i64(width, scope)?;
                if w <= 0 {
                    bail!(width.span, "an indexed part-select must be at least one bit wide");
                }
                let w = w as usize;
                let (slot, dim) = self.base_slot(base, scope)?;
                let (idx, t) = self.lower_self(start, scope, env)?;
                let lo_delta = if *up { 0 } else { -(w as i64 - 1) };
                let lsb_delta = if dim.upto() { lo_delta + w as i64 - 1 } else { lo_delta };
                if let Some(c) = as_const(&idx) {
                    let Some(i) = (ConstVal {
                        bits: c,
                        signed: t.signed,
                        string: false,
                    })
                    .as_i64() else {
                        return Ok(Target::Bits {
                            slot,
                            positions: vec![None; w],
                        });
                    };
                    let p0 = dim.pos_unchecked(i + lsb_delta);
                    let positions = (0..w as i64)
                        .map(|k| {
                            let p = p0 + k;
                            (0..dim.width() as i64).contains(&p).then_some(p as usize)
                        })
                        .collect();
                    return Ok(Target::Bits { slot, positions });
                }
                Ok(Target::Dynamic {
                    slot,
                    dim,
                    index: idx,
                    index_ty: t,
                    width: w,
                    delta: lsb_delta,
                })
            }
            _ => bail!(lhs.span, "not something that can be assigned to"),
        }
    }

    fn mem_of(&mut self, e: &'a Expr, scope: ScopeId) -> Result<Option<usize>> {
        Ok(match &e.kind {
            ExprKind::Ident(id) => match self.resolve(scope, id)? {
                Sym::Mem(m) => Some(m),
                _ => None,
            },
            ExprKind::Hier(p) => match self.resolve_hier(scope, p, e.span)? {
                Sym::Mem(m) => Some(m),
                _ => None,
            },
            _ => None,
        })
    }

    fn base_slot(&mut self, base: &'a Expr, scope: ScopeId) -> Result<(Slot, Dim)> {
        let sym = match &base.kind {
            ExprKind::Ident(id) => self.resolve(scope, id)?,
            ExprKind::Hier(p) => self.resolve_hier(scope, p, base.span)?,
            _ => bail!(base.span, "only a simple name can be selected from on the left of an assignment"),
        };
        self.slot_of(&sym, base.span)
    }

    fn slot_of(&self, sym: &Sym<'a>, span: Span) -> Result<(Slot, Dim)> {
        match sym {
            Sym::Var(v) => Ok((Slot::Var(*v), self.vars[*v].dim)),
            Sym::Local(l) => Ok((Slot::Local(*l), self.locals[*l].dim)),
            Sym::Mem(_) => bail!(span, "a memory can only be assigned a word at a time"),
            Sym::Param(_) => bail!(span, "a parameter can't be assigned"),
            Sym::Func(..) | Sym::Task(..) => bail!(span, "a function or task can't be assigned"),
        }
    }

    /// The value of a slot as an assignment sees it (the value it will
    /// have, for splicing in new bits).
    fn current(&self, slot: Slot, env: &Env) -> SigSpec {
        if let Some(v) = env.get(slot) {
            return v.clone();
        }
        match slot {
            Slot::Var(v) => match env.mode {
                Mode::Initial => konst(
                    &self.vars[v]
                        .init
                        .clone()
                        .unwrap_or_else(|| vec![State::X; self.vars[v].dim.width()]),
                ),
                _ => self.vars[v].nets.clone(),
            },
            Slot::Local(l) => xs(self.locals[l].dim.width()),
            Slot::Hidden(h) => env.hidden_init[&h].clone(),
            Slot::Assigned(v) => vec![SigBit::ZERO; self.vars[v].dim.width()],
        }
    }

    fn write_target(&mut self, t: Target, v: SigSpec, blocking: bool, env: &mut Env, span: Span) -> Result<()> {
        debug_assert_eq!(t.width(), v.len());
        match t {
            Target::Concat(parts) => {
                // The last part takes the low bits.
                let mut at = 0;
                let mut pieces = Vec::new();
                for p in parts.into_iter().rev() {
                    let w = p.width();
                    pieces.push((p, v[at..at + w].to_vec()));
                    at += w;
                }
                for (p, pv) in pieces {
                    self.write_target(p, pv, blocking, env, span)?;
                }
                Ok(())
            }
            Target::Bits { slot, positions } => {
                self.check_assignable(slot, blocking, env, span)?;
                let mut cur = self.current(slot, env);
                for (p, b) in positions.iter().zip(&v) {
                    if let Some(p) = *p {
                        cur[p] = *b;
                    }
                }
                env.set(slot, cur);
                self.mark_assigned(slot, positions.iter().flatten().copied(), env);
                Ok(())
            }
            Target::Dynamic {
                slot,
                dim,
                index,
                index_ty,
                width,
                delta,
            } => {
                self.check_assignable(slot, blocking, env, span)?;
                // One comparison per position the select can start at: the
                // bits there take the value when the index matches.
                let mut cur = self.current(slot, env);
                let n = dim.width();
                let (pos, signed) = self.position(index, index_ty, dim, delta, span);
                // Starts below 0 or near the top write the bits that are
                // in range.
                let first = if signed { 1 - width as i64 } else { 0 };
                for p0 in first..n as i64 {
                    let lo = p0.max(0) as usize;
                    let hi = ((p0 + width as i64) as usize).min(n);
                    if lo >= hi {
                        continue;
                    }
                    let pw = pos.len().max(64 - p0.unsigned_abs().leading_zeros() as usize + 1);
                    let pc = konst(&chipsmith_netlist::Const::from_int(p0, pw).0);
                    let pos_w = crate::build::extend(&pos, pw, signed);
                    let hit = self.b.binary(
                        chipsmith_netlist::BinaryOp::Eq,
                        pos_w,
                        signed,
                        pc,
                        signed,
                        1,
                        span,
                    )[0];
                    let old = cur[lo..hi].to_vec();
                    let k0 = (lo as i64 - p0) as usize;
                    let new = self.b.mux(hit, old, v[k0..k0 + (hi - lo)].to_vec(), span);
                    cur[lo..hi].copy_from_slice(&new);
                }
                env.set(slot, cur);
                self.mark_assigned(slot, 0..n, env);
                Ok(())
            }
            Target::Mem { mem, addr, bits } => {
                if !blocking && env.mode == Mode::Initial {
                    // Fine: initial contents either way.
                }
                super::proc::mem_write(self, mem, addr, v, bits, blocking, env, span)
            }
        }
    }

    /// Writes bits of a variable (a memory word kept as a register).
    pub fn write_word(
        &mut self,
        v: usize,
        positions: Vec<Option<usize>>,
        value: SigSpec,
        blocking: bool,
        env: &mut Env,
        span: Span,
    ) -> Result<()> {
        self.write_target(
            Target::Bits {
                slot: Slot::Var(v),
                positions,
            },
            value,
            blocking,
            env,
            span,
        )
    }

    fn check_assignable(&mut self, slot: Slot, blocking: bool, env: &mut Env, span: Span) -> Result<()> {
        let Slot::Var(v) = slot else {
            return Ok(());
        };
        match env.mode {
            Mode::Const => bail!(span, "`{}` can't be assigned in a constant function", self.vars[v].name),
            Mode::Cont => bail!(span, "`{}` can't be assigned here", self.vars[v].name),
            Mode::Initial => return Ok(()),
            Mode::Comb | Mode::Clocked => {}
        }
        if !self.vars[v].variable {
            bail!(
                span,
                "`{}` is a net; procedural code can only assign variables (declare it `reg`)",
                self.vars[v].name
            );
        }
        let (mine, other) = if blocking {
            (&mut env.blocking, &env.nonblocking)
        } else {
            (&mut env.nonblocking, &env.blocking)
        };
        if other.contains(&v) {
            bail!(
                span,
                "`{}` is assigned with both `=` and `<=` in one always block",
                self.vars[v].name
            );
        }
        mine.insert(v);
        Ok(())
    }

    fn mark_assigned(&mut self, slot: Slot, positions: impl Iterator<Item = usize>, env: &mut Env) {
        let Slot::Var(v) = slot else { return };
        if !matches!(env.mode, Mode::Comb | Mode::Clocked | Mode::Initial) {
            return;
        }
        let w = self.vars[v].dim.width();
        let mask = env.assigned.entry(v).or_insert_with(|| vec![false; w]);
        let mut changed = Vec::new();
        for p in positions {
            mask[p] = true;
            changed.push(p);
        }
        if env.mode == Mode::Comb {
            let mut a = self.current(Slot::Assigned(v), env);
            for p in changed {
                a[p] = SigBit::ONE;
            }
            env.set(Slot::Assigned(v), a);
        }
    }

    // -----------------------------------------------------------------
    // if / case

    #[allow(clippy::too_many_arguments)]
    fn case(
        &mut self,
        kind: CaseKind,
        expr: &'a Expr,
        items: &'a [crate::ast::CaseItem],
        parallel: bool,
        full: bool,
        scope: ScopeId,
        env: &mut Env,
        span: Span,
    ) -> Result<()> {
        // The case expression and every item are sized to the widest of
        // them, signed only if all are (IEEE 1364-2005 §9.5).
        let mut ty = self.self_ty(expr, scope)?;
        for it in items {
            for e in &it.exprs {
                let t = self.self_ty(e, scope)?;
                ty.width = ty.width.max(t.width);
                ty.signed &= t.signed;
            }
        }
        let sel = self.lower(expr, ty, scope, env)?;
        // Patterns per item; a constant pattern's don't-care bits are
        // None (z/? in casez, x/z/? in casex).
        let mut patterns: Vec<Vec<Vec<Option<SigBit>>>> = Vec::new();
        let mut default = None;
        let mut bodies = Vec::new();
        for it in items {
            if it.exprs.is_empty() {
                if default.is_some() {
                    bail!(it.body.span, "a case can only have one default");
                }
                default = Some(&it.body);
                continue;
            }
            let mut pats = Vec::new();
            for e in &it.exprs {
                let v = self.lower(e, ty, scope, env)?;
                pats.push(
                    v.into_iter()
                        .map(|b| match (kind, b) {
                            (CaseKind::Casez, SigBit::Const(State::Z)) => None,
                            (CaseKind::Casex, SigBit::Const(State::Z | State::X)) => None,
                            _ => Some(b),
                        })
                        .collect(),
                );
            }
            patterns.push(pats);
            bodies.push(&it.body);
        }
        // `(* full_case *)` without a default: where no item matches, the
        // variables the case assigns with `=` are x (so no latch), the
        // ones assigned with `<=` keep their value. That is yosys' reading
        // (its front end adds that default case).
        let full_x = if full && default.is_none() {
            let mut lv = Vec::new();
            for it in items {
                self.blocking_lvalues(&it.body, scope, &mut lv);
            }
            Some(lv)
        } else {
            None
        };
        // Items covering every value the case expression can take leave
        // nothing for a missing default: no latch, as yosys' proc_dlatch
        // finds.
        let complete = default.is_none() && covers_all(&sel, patterns.iter().flatten());
        let groups = pgroups(&patterns, parallel);
        let mut arms = Vec::new();
        for ((pats, body), group) in patterns.iter().zip(bodies).zip(groups) {
            let mut hits = Vec::new();
            for p in pats {
                hits.push(self.pattern_match(&sel, p, span));
            }
            let ctrl = self.any(hits, span);
            arms.push(Arm { ctrl, group, body });
        }
        self.switch_full(arms, default, full_x, complete, scope, env, span)
    }

    /// One case pattern against the case expression.
    fn pattern_match(&mut self, sel: &[SigBit], pat: &[Option<SigBit>], span: Span) -> SigBit {
        let (s, p): (SigSpec, SigSpec) = sel
            .iter()
            .zip(pat)
            .filter_map(|(&s, &p)| p.map(|p| (s, p)))
            .unzip();
        if s.is_empty() {
            return SigBit::ONE;
        }
        if s.len() == 1 && p[0] == SigBit::ONE {
            return s[0];
        }
        self.b
            .binary(chipsmith_netlist::BinaryOp::Eq, s, false, p, false, 1, span)[0]
    }

    /// OR of several one-bit signals (`$reduce_or` when more than one).
    fn any(&mut self, mut bits: Vec<SigBit>, span: Span) -> SigBit {
        if bits.contains(&SigBit::ONE) {
            return SigBit::ONE;
        }
        bits.retain(|&b| b != SigBit::ZERO);
        match bits.len() {
            0 => SigBit::ZERO,
            1 => bits[0],
            _ => self.b.unary(
                chipsmith_netlist::UnaryOp::ReduceOr,
                bits,
                false,
                1,
                span,
            )[0],
        }
    }

    fn switch(
        &mut self,
        arms: Vec<Arm<'a>>,
        default: Option<&'a Stmt>,
        scope: ScopeId,
        env: &mut Env,
        span: Span,
    ) -> Result<()> {
        self.switch_full(arms, default, None, false, scope, env, span)
    }

    /// Runs the arms and the default, then merges what they assigned.
    /// `complete`: some arm is always taken, so without a default the
    /// value before the switch is never what comes out.
    #[allow(clippy::too_many_arguments)]
    fn switch_full(
        &mut self,
        arms: Vec<Arm<'a>>,
        default: Option<&'a Stmt>,
        full_x: Option<Vec<(Slot, Vec<Option<usize>>)>>,
        complete: bool,
        scope: ScopeId,
        env: &mut Env,
        span: Span,
    ) -> Result<()> {
        let mut taken: Vec<(SigBit, usize, FxHashMap<Slot, SigSpec>)> = Vec::new();
        let mut default_body = default;
        let mut prev_group = None;
        for arm in arms {
            if arm.ctrl == SigBit::ZERO {
                continue;
            }
            if arm.ctrl == SigBit::ONE && prev_group != Some(arm.group) {
                // Always taken when reached: the arms after it are dead,
                // and it is the default.
                default_body = Some(arm.body);
                break;
            }
            prev_group = Some(arm.group);
            env.push();
            let r = self.exec(arm.body, scope, env);
            let frame = env.pop();
            r?;
            taken.push((arm.ctrl, arm.group, frame));
        }
        let default_frame = match (default_body, full_x) {
            (Some(body), _) => {
                env.push();
                let r = self.exec(body, scope, env);
                let f = env.pop();
                r?;
                f
            }
            (None, Some(lvalues)) => {
                env.push();
                for (slot, positions) in lvalues {
                    let n = positions.len();
                    let r = self.write_target(Target::Bits { slot, positions }, xs(n), true, env, span);
                    if let Err(e) = r {
                        env.pop();
                        return Err(e);
                    }
                }
                env.pop()
            }
            (None, None) => FxHashMap::default(),
        };
        if taken.is_empty() {
            env.absorb(default_frame);
            return Ok(());
        }
        let mut keys: Vec<Slot> = default_frame.keys().copied().collect();
        for (_, _, f) in &taken {
            keys.extend(f.keys().copied());
        }
        keys.sort_by_key(slot_order);
        keys.dedup();
        let unreachable_default = complete && default_body.is_none();
        for key in keys {
            let base = self.current(key, env);
            let dval = match default_frame.get(&key) {
                Some(v) => v.clone(),
                None if unreachable_default => match key {
                    Slot::Assigned(_) => vec![SigBit::ONE; base.len()],
                    Slot::Var(_) if env.mode == Mode::Comb => xs(base.len()),
                    _ => base.clone(),
                },
                None => base.clone(),
            };
            let vals: Vec<SigSpec> = taken
                .iter()
                .map(|(_, _, f)| f.get(&key).cloned().unwrap_or_else(|| base.clone()))
                .collect();
            let classes = match key {
                Slot::Var(v) => env.snippets.get(&v).cloned(),
                _ => None,
            };
            let merged = self.merge(&taken, &vals, dval, classes.as_deref(), span);
            env.set(key, merged);
        }
        Ok(())
    }

    /// The multiplexer tree for one slot, built the way yosys' proc_mux
    /// builds it, so that the two agree even in states the design never
    /// reaches (which induction proofs consider): arms from last to
    /// first, a group of arms that can't overlap one `$pmux` whose later
    /// selects win (yosys' `append_pmux` order), each new group a `$mux`
    /// level above the previous result. An arm leaving the value as the
    /// `$pmux`'s default is left out of it, and a leading arm that changes
    /// nothing makes no cell. Those decisions are per snippet (`classes`,
    /// see [`Env::snippets`]), as yosys makes them.
    fn merge(
        &mut self,
        arms: &[(SigBit, usize, FxHashMap<Slot, SigSpec>)],
        vals: &[SigSpec],
        dval: SigSpec,
        classes: Option<&[u32]>,
        span: Span,
    ) -> SigSpec {
        let w = dval.len();
        let mut groups: Vec<(u32, Vec<usize>)> = Vec::new();
        for i in 0..w {
            if vals.iter().all(|v| v[i] == dval[i]) {
                continue;
            }
            let class = classes.map_or(0, |c| c[i]);
            match groups.iter_mut().find(|(c, _)| *c == class) {
                Some((_, bits)) => bits.push(i),
                None => groups.push((class, vec![i])),
            }
        }
        let mut out = dval.clone();
        for (_, bits) in groups {
            let pick = |v: &SigSpec| -> SigSpec { bits.iter().map(|&i| v[i]).collect() };
            let base = pick(&dval);
            let mut result = base.clone();
            // The open `$pmux`: its default and cases so far.
            let mut open: Option<(SigSpec, Vec<(SigBit, SigSpec)>)> = None;
            for k in (0..arms.len()).rev() {
                let v = pick(&vals[k]);
                let same_group = k + 1 < arms.len() && arms[k].1 == arms[k + 1].1;
                match &mut open {
                    Some((a, cases)) if same_group => {
                        if v != *a {
                            cases.push((arms[k].0, v));
                        }
                    }
                    _ => {
                        if v == result {
                            continue;
                        }
                        if let Some((a, cases)) = open.take() {
                            result = self.b.pmux(a, cases, span);
                        }
                        open = Some((result.clone(), vec![(arms[k].0, v)]));
                    }
                }
            }
            if let Some((a, cases)) = open {
                result = self.b.pmux(a, cases, span);
            }
            for (k, &i) in bits.iter().enumerate() {
                out[i] = result[k];
            }
        }
        out
    }

    /// Declares a named block's `reg`s and `integer`s: module variables
    /// (they are static, and reachable as `block.name`), or locals inside
    /// a function or task.
    fn declare_block_vars(&mut self, scope: ScopeId, d: &'a crate::ast::NetDecl, env: &mut Env) -> Result<()> {
        let _ = env;
        self.declare_net(scope, d, None)
    }
}

/// Whether constant patterns match every value of `sel` (its bits that
/// aren't constant: up to 12 of them are enumerated).
fn covers_all<'p>(sel: &[SigBit], patterns: impl Iterator<Item = &'p Vec<Option<SigBit>>>) -> bool {
    let free: Vec<usize> = (0..sel.len()).filter(|&i| sel[i].net().is_some()).collect();
    if free.len() > 12 {
        return false;
    }
    let mut covered = vec![false; 1 << free.len()];
    for p in patterns {
        let mut fixed = Vec::new();
        let mut possible = true;
        for (i, (&s, &b)) in sel.iter().zip(p).enumerate() {
            match (s, b) {
                (_, None) => {}
                (_, Some(SigBit::Net(_))) => {
                    possible = false;
                    break;
                }
                (_, Some(SigBit::Const(State::X | State::Z))) => {
                    possible = false;
                    break;
                }
                (SigBit::Const(c), Some(SigBit::Const(d))) => {
                    if c != d {
                        possible = false;
                        break;
                    }
                }
                (SigBit::Net(_), Some(SigBit::Const(d))) => {
                    let k = free.iter().position(|&f| f == i).expect("a free bit");
                    fixed.push((k, d == State::One));
                }
            }
        }
        if !possible {
            continue;
        }
        for (v, c) in covered.iter_mut().enumerate() {
            if fixed.iter().all(|&(k, one)| (v >> k & 1 == 1) == one) {
                *c = true;
            }
        }
    }
    covered.iter().all(|&c| c)
}

impl<'a, 'd> ModElab<'a, 'd> {
    /// The variable bits a statement assigns with `=` (yosys'
    /// `collect_lvalues`), by constant selects; a variable select takes
    /// the whole variable.
    fn blocking_lvalues(&mut self, s: &'a Stmt, scope: ScopeId, out: &mut Vec<(Slot, Vec<Option<usize>>)>) {
        match &s.kind {
            StmtKind::Assign { lhs, blocking: true, .. } => self.lvalue_bits(lhs, scope, out),
            StmtKind::Block { stmts, .. } => stmts.iter().for_each(|x| self.blocking_lvalues(x, scope, out)),
            StmtKind::If { then, els, .. } => {
                self.blocking_lvalues(then, scope, out);
                if let Some(e) = els {
                    self.blocking_lvalues(e, scope, out);
                }
            }
            StmtKind::Case { items, .. } => items.iter().for_each(|i| self.blocking_lvalues(&i.body, scope, out)),
            StmtKind::For { body, .. } | StmtKind::While { body, .. } | StmtKind::Repeat { body, .. } => {
                self.blocking_lvalues(body, scope, out)
            }
            _ => {}
        }
    }

    fn lvalue_bits(&mut self, e: &'a Expr, scope: ScopeId, out: &mut Vec<(Slot, Vec<Option<usize>>)>) {
        let slot_dim = |m: &Self, b: &'a Expr| -> Option<(Slot, Dim)> {
            let ExprKind::Ident(id) = &b.kind else { return None };
            match m.lookup(scope, &id.name)? {
                (Sym::Var(v), _) => Some((Slot::Var(v), m.vars[v].dim)),
                (Sym::Local(l), _) => Some((Slot::Local(l), m.locals[l].dim)),
                _ => None,
            }
        };
        let all = |d: Dim| (0..d.width()).map(Some).collect::<Vec<_>>();
        match &e.kind {
            ExprKind::Concat(parts) => parts.iter().for_each(|p| self.lvalue_bits(p, scope, out)),
            ExprKind::Ident(_) => {
                if let Some((slot, d)) = slot_dim(self, e) {
                    out.push((slot, all(d)));
                }
            }
            ExprKind::Index { base, index } => {
                if let Some((slot, d)) = slot_dim(self, base) {
                    match self.eval_i64(index, scope).ok().and_then(|i| d.pos(i)) {
                        Some(p) => out.push((slot, vec![Some(p)])),
                        None => out.push((slot, all(d))),
                    }
                }
            }
            ExprKind::Slice { base, msb, lsb } => {
                if let Some((slot, d)) = slot_dim(self, base) {
                    match (self.eval_i64(msb, scope), self.eval_i64(lsb, scope)) {
                        (Ok(m), Ok(l)) => {
                            let (a, b) = (d.pos_unchecked(l), d.pos_unchecked(m));
                            let ps = (a.min(b)..=a.max(b))
                                .filter(|&p| (0..d.width() as i64).contains(&p))
                                .map(|p| Some(p as usize))
                                .collect();
                            out.push((slot, ps));
                        }
                        _ => out.push((slot, all(d))),
                    }
                }
            }
            ExprKind::IndexedSlice { base, .. } => {
                if let Some((slot, d)) = slot_dim(self, base) {
                    out.push((slot, all(d)));
                }
            }
            _ => {}
        }
    }

    /// Fills [`Env::snippets`] for a block: every assignment's target
    /// splits its variable's bits, a variable index into single bits (a
    /// loop over `x[i]` writes them one at a time once unrolled).
    pub fn prescan_snippets(&mut self, body: &'a Stmt, scope: ScopeId, env: &mut Env) {
        let mut masks: FxHashMap<usize, Vec<Vec<bool>>> = FxHashMap::default();
        self.scan_stmt(body, scope, &mut masks);
        for (v, ms) in masks {
            let w = self.vars[v].dim.width();
            let mut sigs: Vec<Vec<bool>> = vec![Vec::new(); w];
            for m in &ms {
                for (i, s) in sigs.iter_mut().enumerate() {
                    s.push(m[i]);
                }
            }
            let mut ids: Vec<Vec<bool>> = Vec::new();
            let classes = sigs
                .into_iter()
                .map(|s| match ids.iter().position(|x| *x == s) {
                    Some(k) => k as u32,
                    None => {
                        ids.push(s);
                        ids.len() as u32 - 1
                    }
                })
                .collect();
            env.snippets.insert(v, classes);
        }
    }

    fn scan_stmt(&mut self, s: &'a Stmt, scope: ScopeId, out: &mut FxHashMap<usize, Vec<Vec<bool>>>) {
        match &s.kind {
            StmtKind::Assign { lhs, .. } => self.scan_lhs(lhs, scope, out),
            StmtKind::Block { stmts, .. } => stmts.iter().for_each(|x| self.scan_stmt(x, scope, out)),
            StmtKind::If { then, els, .. } => {
                self.scan_stmt(then, scope, out);
                if let Some(e) = els {
                    self.scan_stmt(e, scope, out);
                }
            }
            StmtKind::Case { items, .. } => items.iter().for_each(|i| self.scan_stmt(&i.body, scope, out)),
            StmtKind::For { init, step, body, .. } => {
                self.scan_stmt(init, scope, out);
                self.scan_stmt(step, scope, out);
                self.scan_stmt(body, scope, out);
            }
            StmtKind::While { body, .. } | StmtKind::Repeat { body, .. } | StmtKind::Event { body, .. } => {
                self.scan_stmt(body, scope, out)
            }
            _ => {}
        }
    }

    fn scan_lhs(&mut self, e: &'a Expr, scope: ScopeId, out: &mut FxHashMap<usize, Vec<Vec<bool>>>) {
        let var = |m: &mut Self, b: &'a Expr| -> Option<usize> {
            match &b.kind {
                ExprKind::Ident(id) => match m.lookup(scope, &id.name) {
                    Some((Sym::Var(v), _)) => Some(v),
                    _ => None,
                },
                _ => None,
            }
        };
        let mut push = |v: usize, mask: Vec<bool>| out.entry(v).or_default().push(mask);
        match &e.kind {
            ExprKind::Concat(parts) => {
                for p in parts {
                    self.scan_lhs(p, scope, out);
                }
            }
            ExprKind::Ident(_) => {
                if let Some(v) = var(self, e) {
                    let w = self.vars[v].dim.width();
                    push(v, vec![true; w]);
                }
            }
            ExprKind::Index { base, index } => {
                let Some(v) = var(self, base) else { return };
                let dim = self.vars[v].dim;
                match self.eval_i64(index, scope).ok().and_then(|i| dim.pos(i)) {
                    Some(p) => {
                        let mut m = vec![false; dim.width()];
                        m[p] = true;
                        push(v, m);
                    }
                    None => {
                        for p in 0..dim.width() {
                            let mut m = vec![false; dim.width()];
                            m[p] = true;
                            push(v, m);
                        }
                    }
                }
            }
            ExprKind::Slice { base, msb, lsb } => {
                let Some(v) = var(self, base) else { return };
                let dim = self.vars[v].dim;
                if let (Ok(m), Ok(l)) = (self.eval_i64(msb, scope), self.eval_i64(lsb, scope)) {
                    let (a, b) = (dim.pos_unchecked(l), dim.pos_unchecked(m));
                    let mask = (0..dim.width() as i64).map(|p| p >= a.min(b) && p <= a.max(b)).collect();
                    push(v, mask);
                }
            }
            ExprKind::IndexedSlice { base, .. } => {
                // The start is usually a variable: one bit at a time is as
                // fine as it gets.
                let Some(v) = var(self, base) else { return };
                let w = self.vars[v].dim.width();
                for p in 0..w {
                    let mut m = vec![false; w];
                    m[p] = true;
                    push(v, m);
                }
            }
            _ => {}
        }
    }
}

/// A deterministic order for merging slots (so cell names are stable).
fn slot_order(s: &Slot) -> (u8, usize) {
    match *s {
        Slot::Var(v) => (0, v),
        Slot::Local(l) => (1, l),
        Slot::Hidden(h) => (2, h),
        Slot::Assigned(v) => (3, v),
    }
}

fn has_attr(attrs: &[crate::ast::Attr], name: &str) -> bool {
    attrs.iter().any(|a| &*a.name.name == name)
}

/// Which arms can share a `$pmux`: yosys' `proc_mux` grouping. With
/// `parallel_case`, or when every pattern is a distinct defined constant,
/// all of them; otherwise consecutive arms stay in a group while their
/// constant patterns can't match anything an earlier arm in the group
/// does.
fn pgroups(patterns: &[Vec<Vec<Option<SigBit>>>], parallel: bool) -> Vec<usize> {
    let n = patterns.len();
    let is_const = |p: &Vec<Option<SigBit>>| p.iter().all(|b| matches!(b, None | Some(SigBit::Const(_))));
    let is_defined = |p: &Vec<Option<SigBit>>| {
        p.iter()
            .all(|b| matches!(b, Some(SigBit::Const(State::Zero | State::One))))
    };
    if parallel {
        return vec![0; n];
    }
    let mut seen = FxHashSet::default();
    let simple = patterns
        .iter()
        .flatten()
        .all(|p| is_defined(p) && seen.insert(p.clone()));
    if simple {
        return vec![0; n];
    }
    // Two constant patterns overlap unless some bit both care about
    // differs.
    let overlap = |a: &Vec<Option<SigBit>>, b: &Vec<Option<SigBit>>| {
        !a.iter().zip(b).any(|(x, y)| match (x, y) {
            (Some(SigBit::Const(s)), Some(SigBit::Const(t))) => {
                matches!(s, State::Zero | State::One) && matches!(t, State::Zero | State::One) && s != t
            }
            _ => false,
        })
    };
    let mut groups = vec![0; n];
    let mut taken: Vec<&Vec<Option<SigBit>>> = Vec::new();
    let mut extra_next = false;
    for i in 0..n {
        if i > 0 {
            groups[i] = groups[i - 1];
            let mut new_group = extra_next;
            extra_next = false;
            for p in &patterns[i] {
                if !is_const(p) || taken.iter().any(|t| overlap(t, p)) {
                    new_group = true;
                }
            }
            if new_group {
                groups[i] += 1;
                taken.clear();
            }
        }
        for p in &patterns[i] {
            if is_const(p) {
                taken.push(p);
            } else {
                extra_next = true;
            }
        }
    }
    groups
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pat(s: &str) -> Vec<Option<SigBit>> {
        s.chars()
            .rev()
            .map(|c| match c {
                '-' => None,
                c => Some(SigBit::Const(State::parse(c).unwrap())),
            })
            .collect()
    }

    #[test]
    fn grouping() {
        // Distinct constants: one group.
        assert_eq!(pgroups(&[vec![pat("00")], vec![pat("01")]], false), [0, 0]);
        // 1- overlaps 11 after it: a new priority level.
        assert_eq!(
            pgroups(&[vec![pat("1-")], vec![pat("01")], vec![pat("11")]], false),
            [0, 0, 1]
        );
        assert_eq!(pgroups(&[vec![pat("1-")], vec![pat("11")]], true), [0, 0]);
    }
}
