//! Expressions: their types by IEEE 1364-2005 §5.4 (bit lengths) and §5.5
//! (signedness), and their lowering to RTLIL cells.
//!
//! An expression is lowered *in a context*: [`ModElab::lower`] takes the
//! width and signedness the operands of the enclosing context-determined
//! expression agree on, and returns exactly that many bits. Operands of
//! context-determined operators are lowered in the same context, so every
//! cell's operands already have the result's width and the operator's
//! signedness; self-determined operands are lowered at their own type,
//! and a self-determined result is extended into the context (with the
//! context's signedness: an unsigned operand anywhere makes it unsigned,
//! §5.5.1, which is why `$signed(a) + 1'b1` zero-extends `a`).

use std::rc::Rc;

use chipsmith_netlist::{BinaryOp, SigBit, SigSpec, State, UnaryOp};

use super::stmt::{Mode, Slot};
use super::{ConstVal, Dim, Env, ModElab, ScopeId, Sym, Ty};
use crate::ast::{BinOp, Expr, ExprKind, Ident, UnOp};
use crate::build::{as_const, extend, konst, xs};
use crate::source::{bail, Result, Span};

/// Extends a value of type `from` into context `ctx`: sign extension only
/// if the context is signed (and so the value is).
fn into_ctx(v: SigSpec, ctx: Ty) -> SigSpec {
    extend(&v, ctx.width, ctx.signed)
}

/// What a select's base is.
pub(crate) enum Base {
    /// A vector: its current value and declared range.
    Vector { bits: SigSpec, dim: Dim },
    /// A memory word (`mem[addr]`), not yet read.
    Mem { mem: usize },
}

impl<'a, 'd> ModElab<'a, 'd> {
    // -----------------------------------------------------------------
    // Names

    pub fn lookup(&self, mut scope: ScopeId, name: &str) -> Option<(Sym<'a>, Span)> {
        loop {
            let s = &self.scopes[scope];
            if let Some(found) = s.names.get(name) {
                return Some(found.clone());
            }
            scope = s.parent?;
        }
    }

    pub fn resolve(&self, scope: ScopeId, id: &Ident) -> Result<Sym<'a>> {
        match self.lookup(scope, &id.name) {
            Some((sym, _)) => Ok(sym),
            None => bail!(id.span, "`{}` is not declared", id.name),
        }
    }

    /// A function or task by name: inside a function its own name is its
    /// result variable, which a recursive call looks past.
    pub fn resolve_callable(&self, mut scope: ScopeId, id: &Ident) -> Result<Sym<'a>> {
        loop {
            let s = &self.scopes[scope];
            if let Some((sym @ (Sym::Func(..) | Sym::Task(..)), _)) = s.names.get(&id.name) {
                return Ok(sym.clone());
            }
            match s.parent {
                Some(p) => scope = p,
                None => bail!(id.span, "`{}` is not a function or task", id.name),
            }
        }
    }

    /// `blk[2].x`: a name inside generate blocks.
    pub fn resolve_hier(
        &mut self,
        scope: ScopeId,
        path: &'a [(Ident, Option<Expr>)],
        span: Span,
    ) -> Result<Sym<'a>> {
        let (last, prefix) = path.split_last().expect("a path has a name");
        if last.1.is_some() {
            bail!(span, "a hierarchical name must end in a name");
        }
        // The first block is looked up through enclosing scopes.
        let mut s = scope;
        let mut first = true;
        for (id, idx) in prefix {
            let key = match idx {
                Some(e) => format!("{}[{}]", id.name, self.eval_i64(e, scope)?),
                None => id.name.to_string(),
            };
            let mut probe = Some(s);
            let mut found = None;
            while let Some(p) = probe {
                if let Some(&b) = self.scopes[p].blocks.get(&key) {
                    found = Some(b);
                    break;
                }
                if !first {
                    break;
                }
                probe = self.scopes[p].parent;
            }
            let Some(b) = found else {
                bail!(
                    id.span,
                    "`{key}` is not a generate block here (references into other modules are not supported)"
                );
            };
            s = b;
            first = false;
        }
        match self.scopes[s].names.get(&*last.0.name) {
            Some((sym, _)) => Ok(sym.clone()),
            None => bail!(
                last.0.span,
                "`{}` is not declared in that block",
                last.0.name
            ),
        }
    }

    fn expr_sym(&mut self, e: &'a Expr, scope: ScopeId) -> Result<Option<Sym<'a>>> {
        Ok(match &e.kind {
            ExprKind::Ident(id) => Some(self.resolve(scope, id)?),
            ExprKind::Hier(path) => Some(self.resolve_hier(scope, path, e.span)?),
            _ => None,
        })
    }

    // -----------------------------------------------------------------
    // Constants

    pub fn eval_const(&mut self, e: &'a Expr, scope: ScopeId) -> Result<ConstVal> {
        let ty = self.self_ty(e, scope)?;
        let mut env = Env::new(Mode::Const);
        let v = self.lower(e, ty, scope, &mut env)?;
        let Some(bits) = as_const(&v) else {
            bail!(e.span, "not a constant expression");
        };
        Ok(ConstVal {
            bits,
            signed: ty.signed,
            string: matches!(e.kind, ExprKind::Str(_)),
        })
    }

    pub fn eval_i64(&mut self, e: &'a Expr, scope: ScopeId) -> Result<i64> {
        let v = self.eval_const(e, scope)?;
        match v.as_i64() {
            Some(i) => Ok(i),
            None => bail!(
                e.span,
                "expected an integer, the value is {}",
                chipsmith_netlist::Const(v.bits)
            ),
        }
    }

    pub fn eval_dim(&mut self, r: &'a crate::ast::Range, scope: ScopeId) -> Result<Dim> {
        let msb = self.eval_i64(&r.msb, scope)?;
        let lsb = self.eval_i64(&r.lsb, scope)?;
        if (msb - lsb).unsigned_abs() >= 1 << 24 {
            bail!(r.span, "range [{msb}:{lsb}] is too wide");
        }
        Ok(Dim { msb, lsb })
    }

    // -----------------------------------------------------------------
    // Types

    /// An expression's self-determined type, IEEE 1364-2005 Table 5-22.
    pub fn self_ty(&mut self, e: &'a Expr, scope: ScopeId) -> Result<Ty> {
        Ok(match &e.kind {
            ExprKind::Number(n) => Ty {
                width: n.bits.len(),
                signed: n.signed,
            },
            ExprKind::Fill(_) => Ty::BIT,
            ExprKind::Real => bail!(e.span, "real numbers are not supported"),
            ExprKind::Str(s) => Ty {
                width: 8 * s.len().max(1),
                signed: false,
            },
            ExprKind::Ident(_) | ExprKind::Hier(_) => {
                match self.expr_sym(e, scope)?.expect("a name") {
                    Sym::Var(v) => self.vars[v].ty(),
                    Sym::Param(c) => c.ty(),
                    Sym::Local(l) => self.locals[l].ty(),
                    Sym::Mem(_) => bail!(e.span, "a memory can only be used with an index"),
                    Sym::Func(..) | Sym::Task(..) => {
                        bail!(e.span, "a function or task is not a value")
                    }
                }
            }
            ExprKind::Index { base, .. } => {
                if let Some(Sym::Mem(m)) = self.expr_sym(base, scope)? {
                    Ty {
                        width: self.mems[m].width(),
                        signed: self.mems[m].signed,
                    }
                } else {
                    Ty::BIT
                }
            }
            ExprKind::Slice { msb, lsb, .. } => {
                let m = self.eval_i64(msb, scope)?;
                let l = self.eval_i64(lsb, scope)?;
                Ty {
                    width: (m - l).unsigned_abs() as usize + 1,
                    signed: false,
                }
            }
            ExprKind::IndexedSlice { width, .. } => {
                let w = self.eval_i64(width, scope)?;
                if w <= 0 {
                    bail!(
                        width.span,
                        "an indexed part-select must be at least one bit wide"
                    );
                }
                Ty {
                    width: w as usize,
                    signed: false,
                }
            }
            ExprKind::Unary { op, arg } => match op {
                UnOp::Plus | UnOp::Minus | UnOp::BitNot => self.self_ty(arg, scope)?,
                _ => Ty::BIT,
            },
            ExprKind::Binary { op, lhs, rhs } => match op {
                BinOp::Add
                | BinOp::Sub
                | BinOp::Mul
                | BinOp::Div
                | BinOp::Mod
                | BinOp::And
                | BinOp::Or
                | BinOp::Xor
                | BinOp::Xnor => {
                    let a = self.self_ty(lhs, scope)?;
                    let b = self.self_ty(rhs, scope)?;
                    Ty {
                        width: a.width.max(b.width),
                        signed: a.signed && b.signed,
                    }
                }
                BinOp::Pow | BinOp::Shl | BinOp::Shr | BinOp::AShl | BinOp::AShr => {
                    self.self_ty(lhs, scope)?
                }
                _ => Ty::BIT,
            },
            ExprKind::Cond { cond, then, els } => {
                // A constant condition picks one side (as the other may not
                // even elaborate), but the type is both sides'.
                let _ = cond;
                let a = self.self_ty(then, scope)?;
                let b = self.self_ty(els, scope)?;
                Ty {
                    width: a.width.max(b.width),
                    signed: a.signed && b.signed,
                }
            }
            ExprKind::Concat(parts) => {
                let mut w = 0;
                for p in parts {
                    w += self.self_ty(p, scope)?.width;
                }
                Ty {
                    width: w,
                    signed: false,
                }
            }
            ExprKind::Repeat { count, parts } => {
                let n = self.eval_i64(count, scope)?;
                if n < 0 {
                    bail!(count.span, "negative replication count {n}");
                }
                let mut w = 0;
                for p in parts {
                    w += self.self_ty(p, scope)?.width;
                }
                Ty {
                    width: w * n as usize,
                    signed: false,
                }
            }
            ExprKind::Call { name, .. } => {
                let Sym::Func(f, def) = self.resolve_callable(scope, name)? else {
                    bail!(name.span, "`{}` is not a function", name.name);
                };
                self.function_ty(f, def)?
            }
            ExprKind::SysCall { name, args } => match &*name.name {
                "signed" | "unsigned" => {
                    let [a] = &args[..] else {
                        bail!(e.span, "${} takes one argument", name.name);
                    };
                    Ty {
                        width: self.self_ty(a, scope)?.width,
                        signed: &*name.name == "signed",
                    }
                }
                "clog2" | "bits" | "size" => Ty::INTEGER,
                other => bail!(name.span, "system function ${other} is not supported"),
            },
        })
    }

    pub fn function_ty(&mut self, f: &'a crate::ast::Subroutine, def: ScopeId) -> Result<Ty> {
        let ret = f.ret.as_ref().expect("a function has a return type");
        Ok(match ret.kind {
            Some(crate::ast::NetKind::Integer) => Ty::INTEGER,
            _ => Ty {
                width: match &ret.range {
                    Some(r) => self.eval_dim(r, def)?.width(),
                    None => 1,
                },
                signed: ret.signed,
            },
        })
    }

    // -----------------------------------------------------------------
    // Lowering

    pub fn lower_self(
        &mut self,
        e: &'a Expr,
        scope: ScopeId,
        env: &mut Env,
    ) -> Result<(SigSpec, Ty)> {
        let ty = self.self_ty(e, scope)?;
        Ok((self.lower(e, ty, scope, env)?, ty))
    }

    /// A condition: the expression, self-determined, reduced to one bit.
    pub fn lower_bool(&mut self, e: &'a Expr, scope: ScopeId, env: &mut Env) -> Result<SigBit> {
        let (v, _) = self.lower_self(e, scope, env)?;
        Ok(self.b.reduce_bool(v, e.span))
    }

    /// `e` evaluated in a context of type `ctx`: exactly `ctx.width` bits.
    pub fn lower(
        &mut self,
        e: &'a Expr,
        ctx: Ty,
        scope: ScopeId,
        env: &mut Env,
    ) -> Result<SigSpec> {
        let span = e.span;
        Ok(match &e.kind {
            ExprKind::Number(n) => {
                // An unsized constant whose top bit is x or z extends with
                // it (IEEE 1364-2005 §3.5.1).
                let top = n.bits.last().copied().unwrap_or(State::Zero);
                if !n.sized && matches!(top, State::X | State::Z) {
                    let mut v = n.bits.clone();
                    v.resize(ctx.width.max(v.len()), top);
                    v.truncate(ctx.width);
                    konst(&v)
                } else {
                    into_ctx(konst(&n.bits), ctx)
                }
            }
            ExprKind::Fill(s) => konst(&vec![*s; ctx.width]),
            ExprKind::Real => bail!(span, "real numbers are not supported"),
            ExprKind::Str(s) => {
                let mut bits = Vec::with_capacity(8 * s.len());
                for &byte in s.iter().rev() {
                    bits.extend((0..8).map(|i| State::from(byte >> i & 1 == 1)));
                }
                if bits.is_empty() {
                    bits = vec![State::Zero; 8];
                }
                into_ctx(konst(&bits), ctx)
            }
            ExprKind::Ident(_) | ExprKind::Hier(_) => {
                let sym = self.expr_sym(e, scope)?.expect("a name");
                let v = self.read_sym(&sym, env, span)?;
                into_ctx(v, ctx)
            }
            ExprKind::Index { base, index } => {
                let v = match self.select_base(base, scope, env)? {
                    Base::Mem { mem } => {
                        let (addr, _) = self.lower_self(index, scope, env)?;
                        self.mem_read(mem, addr, env, span)?
                    }
                    Base::Vector { bits, dim } => self.bit_select(&bits, dim, index, scope, env)?,
                };
                into_ctx(v, ctx)
            }
            ExprKind::Slice { base, msb, lsb } => {
                let Base::Vector { bits, dim } = self.select_base(base, scope, env)? else {
                    bail!(span, "a part-select of a memory needs a word index first");
                };
                let m = self.eval_i64(msb, scope)?;
                let l = self.eval_i64(lsb, scope)?;
                if (m < l) != dim.upto() && m != l {
                    bail!(
                        span,
                        "part-select [{m}:{l}] is reversed for a vector declared [{}:{}]",
                        dim.msb,
                        dim.lsb
                    );
                }
                into_ctx(const_part(&bits, dim, m, l), ctx)
            }
            ExprKind::IndexedSlice {
                base,
                start,
                width,
                up,
            } => {
                let Base::Vector { bits, dim } = self.select_base(base, scope, env)? else {
                    bail!(span, "a part-select of a memory needs a word index first");
                };
                let w = self.eval_i64(width, scope)? as usize;
                let v = self.indexed_select(&bits, dim, start, w, *up, scope, env, span)?;
                into_ctx(v, ctx)
            }
            ExprKind::Unary { op, arg } => match op {
                UnOp::Plus => self.lower(arg, ctx, scope, env)?,
                UnOp::Minus => {
                    let a = self.lower(arg, ctx, scope, env)?;
                    self.b.unary(UnaryOp::Neg, a, ctx.signed, ctx.width, span)
                }
                UnOp::BitNot => {
                    let a = self.lower(arg, ctx, scope, env)?;
                    self.b.unary(UnaryOp::Not, a, ctx.signed, ctx.width, span)
                }
                UnOp::LogicNot => {
                    let (a, t) = self.lower_self(arg, scope, env)?;
                    let r = self.b.unary(UnaryOp::LogicNot, a, t.signed, 1, span);
                    into_ctx(r, ctx)
                }
                _ => {
                    let (a, t) = self.lower_self(arg, scope, env)?;
                    let (uop, invert) = match op {
                        UnOp::RedAnd => (UnaryOp::ReduceAnd, false),
                        UnOp::RedNand => (UnaryOp::ReduceAnd, true),
                        UnOp::RedOr => (UnaryOp::ReduceOr, false),
                        UnOp::RedNor => (UnaryOp::ReduceOr, true),
                        UnOp::RedXor => (UnaryOp::ReduceXor, false),
                        _ => (UnaryOp::ReduceXnor, false),
                    };
                    let mut r = self.b.unary(uop, a, t.signed, 1, span);
                    if invert {
                        r = self.b.unary(UnaryOp::Not, r, false, 1, span);
                    }
                    into_ctx(r, ctx)
                }
            },
            ExprKind::Binary { op, lhs, rhs } => {
                self.lower_binary(*op, lhs, rhs, ctx, scope, env, span)?
            }
            ExprKind::Cond { cond, then, els } => {
                let c = self.lower_bool(cond, scope, env)?;
                match c {
                    SigBit::ONE => self.lower(then, ctx, scope, env)?,
                    SigBit::ZERO => self.lower(els, ctx, scope, env)?,
                    _ => {
                        let a = self.lower(then, ctx, scope, env)?;
                        let b = self.lower(els, ctx, scope, env)?;
                        self.b.mux(c, b, a, span)
                    }
                }
            }
            ExprKind::Concat(parts) => {
                let v = self.concat(parts, scope, env)?;
                into_ctx(v, ctx)
            }
            ExprKind::Repeat { count, parts } => {
                let n = self.eval_i64(count, scope)?;
                let one = self.concat(parts, scope, env)?;
                let v: SigSpec = (0..n).flat_map(|_| one.iter().copied()).collect();
                into_ctx(v, ctx)
            }
            ExprKind::Call { name, args } => {
                let Sym::Func(f, def) = self.resolve_callable(scope, name)? else {
                    bail!(name.span, "`{}` is not a function", name.name);
                };
                let v = self.call_function(f, def, args, scope, env, span)?;
                into_ctx(v, ctx)
            }
            ExprKind::SysCall { name, args } => match &*name.name {
                "signed" | "unsigned" => {
                    let (v, _) = self.lower_self(&args[0], scope, env)?;
                    into_ctx(v, ctx)
                }
                "clog2" => {
                    let [a] = &args[..] else {
                        bail!(span, "$clog2 takes one argument");
                    };
                    let v = self.eval_const(a, scope)?;
                    if v.bits
                        .iter()
                        .any(|b| !matches!(b, State::Zero | State::One))
                    {
                        bail!(a.span, "$clog2 of an undefined value");
                    }
                    // ceil(log2(v)): the bits of v - 1.
                    let n = v.bits.iter().rposition(|&b| b == State::One);
                    let pow2 = n.is_some_and(|n| v.bits[..n].iter().all(|&b| b == State::Zero));
                    let r = match n {
                        None => 0,
                        Some(n) if pow2 => n as i64,
                        Some(n) => n as i64 + 1,
                    };
                    into_ctx(konst(&ConstVal::from_i64(r).bits), ctx)
                }
                "bits" | "size" => {
                    let [a] = &args[..] else {
                        bail!(span, "${} takes one argument", name.name);
                    };
                    let w = self.self_ty(a, scope)?.width;
                    into_ctx(konst(&ConstVal::from_i64(w as i64).bits), ctx)
                }
                other => bail!(name.span, "system function ${other} is not supported"),
            },
        })
    }

    #[allow(clippy::too_many_arguments)]
    fn lower_binary(
        &mut self,
        op: BinOp,
        lhs: &'a Expr,
        rhs: &'a Expr,
        ctx: Ty,
        scope: ScopeId,
        env: &mut Env,
        span: Span,
    ) -> Result<SigSpec> {
        let arith = |op| match op {
            BinOp::Add => Some(BinaryOp::Add),
            BinOp::Sub => Some(BinaryOp::Sub),
            BinOp::Mul => Some(BinaryOp::Mul),
            BinOp::Div => Some(BinaryOp::Div),
            BinOp::Mod => Some(BinaryOp::Mod),
            BinOp::And => Some(BinaryOp::And),
            BinOp::Or => Some(BinaryOp::Or),
            BinOp::Xor => Some(BinaryOp::Xor),
            BinOp::Xnor => Some(BinaryOp::Xnor),
            _ => None,
        };
        if let Some(cell) = arith(op) {
            let a = self.lower(lhs, ctx, scope, env)?;
            let b = self.lower(rhs, ctx, scope, env)?;
            return Ok(self
                .b
                .binary(cell, a, ctx.signed, b, ctx.signed, ctx.width, span));
        }
        match op {
            BinOp::Pow => {
                let a = self.lower(lhs, ctx, scope, env)?;
                let (b, bt) = self.lower_self(rhs, scope, env)?;
                Ok(self
                    .b
                    .binary(BinaryOp::Pow, a, ctx.signed, b, bt.signed, ctx.width, span))
            }
            BinOp::Shl | BinOp::AShl | BinOp::Shr | BinOp::AShr => {
                // The shifted operand is context-determined, the amount
                // self-determined and always unsigned (§5.1.12).
                let a = self.lower(lhs, ctx, scope, env)?;
                let (b, _) = self.lower_self(rhs, scope, env)?;
                let cell = match op {
                    BinOp::Shl | BinOp::AShl => BinaryOp::Shl,
                    BinOp::AShr if ctx.signed => BinaryOp::Sshr,
                    _ => BinaryOp::Shr,
                };
                Ok(self
                    .b
                    .binary(cell, a, ctx.signed, b, false, ctx.width, span))
            }
            BinOp::Lt
            | BinOp::Le
            | BinOp::Gt
            | BinOp::Ge
            | BinOp::Eq
            | BinOp::Ne
            | BinOp::CaseEq
            | BinOp::CaseNe => {
                // Operands sized to each other, signed only if both are.
                let ta = self.self_ty(lhs, scope)?;
                let tb = self.self_ty(rhs, scope)?;
                let t = Ty {
                    width: ta.width.max(tb.width),
                    signed: ta.signed && tb.signed,
                };
                let a = self.lower(lhs, t, scope, env)?;
                let b = self.lower(rhs, t, scope, env)?;
                let cell = match op {
                    BinOp::Lt => BinaryOp::Lt,
                    BinOp::Le => BinaryOp::Le,
                    BinOp::Gt => BinaryOp::Gt,
                    BinOp::Ge => BinaryOp::Ge,
                    BinOp::Eq => BinaryOp::Eq,
                    BinOp::Ne => BinaryOp::Ne,
                    BinOp::CaseEq => BinaryOp::Eqx,
                    _ => BinaryOp::Nex,
                };
                let r = self.b.binary(cell, a, t.signed, b, t.signed, 1, span);
                Ok(into_ctx(r, ctx))
            }
            BinOp::LogicAnd | BinOp::LogicOr => {
                let (a, ta) = self.lower_self(lhs, scope, env)?;
                // Short-circuit a constant left operand, so `P && x[P-1]`
                // needn't elaborate the right side when P is 0.
                if let Some(c) = as_const(&a) {
                    let t = crate::fold::unary(UnaryOp::ReduceBool, &c, false, 1)[0];
                    match (op, t) {
                        (BinOp::LogicAnd, State::Zero) => {
                            return Ok(into_ctx(vec![SigBit::ZERO], ctx))
                        }
                        (BinOp::LogicOr, State::One) => {
                            return Ok(into_ctx(vec![SigBit::ONE], ctx))
                        }
                        _ => {}
                    }
                }
                let (b, tb) = self.lower_self(rhs, scope, env)?;
                let cell = if op == BinOp::LogicAnd {
                    BinaryOp::LogicAnd
                } else {
                    BinaryOp::LogicOr
                };
                let r = self.b.binary(cell, a, ta.signed, b, tb.signed, 1, span);
                Ok(into_ctx(r, ctx))
            }
            _ => unreachable!("arithmetic handled above"),
        }
    }

    fn concat(&mut self, parts: &'a [Expr], scope: ScopeId, env: &mut Env) -> Result<SigSpec> {
        let mut v = SigSpec::new();
        // The first part is the most significant.
        for p in parts.iter().rev() {
            let (bits, _) = self.lower_self(p, scope, env)?;
            v.extend(bits);
        }
        Ok(v)
    }

    /// The value of a name.
    pub fn read_sym(&mut self, sym: &Sym<'a>, env: &mut Env, span: Span) -> Result<SigSpec> {
        Ok(match sym {
            Sym::Param(c) => konst(&c.bits),
            Sym::Var(v) => self.read_var(*v, env, span)?,
            Sym::Local(l) => match env.get(Slot::Local(*l)) {
                Some(v) => v.clone(),
                None => xs(self.locals[*l].dim.width()),
            },
            Sym::Mem(_) => bail!(span, "a memory can only be used with an index"),
            Sym::Func(..) | Sym::Task(..) => bail!(span, "a function or task is not a value"),
        })
    }

    pub fn read_var(&mut self, v: usize, env: &mut Env, span: Span) -> Result<SigSpec> {
        match env.mode {
            Mode::Const => bail!(
                span,
                "`{}` is not a constant (only parameters can be used here)",
                self.vars[v].name
            ),
            Mode::Cont => Ok(self.vars[v].nets.clone()),
            Mode::Initial => Ok(match env.get(Slot::Var(v)) {
                Some(val) => val.clone(),
                None => konst(
                    &self.vars[v]
                        .init
                        .clone()
                        .unwrap_or_else(|| vec![State::X; self.vars[v].dim.width()]),
                ),
            }),
            Mode::Comb | Mode::Clocked => {
                let val = if env.blocking.contains(&v) {
                    env.get(Slot::Var(v))
                        .cloned()
                        .unwrap_or_else(|| self.vars[v].nets.clone())
                } else {
                    self.vars[v].nets.clone()
                };
                if !env.reads_incoming.contains(&v) {
                    let nets = &self.vars[v].nets;
                    if val.iter().any(|b| nets.contains(b)) {
                        env.reads_incoming.insert(v);
                    }
                }
                Ok(val)
            }
        }
    }

    /// The thing a select applies to: a vector's bits, or a memory.
    pub fn select_base(&mut self, base: &'a Expr, scope: ScopeId, env: &mut Env) -> Result<Base> {
        match &base.kind {
            ExprKind::Ident(_) | ExprKind::Hier(_) => {
                let sym = self.expr_sym(base, scope)?.expect("a name");
                match sym {
                    Sym::Mem(mem) => Ok(Base::Mem { mem }),
                    Sym::Var(v) => {
                        let dim = self.vars[v].dim;
                        Ok(Base::Vector {
                            bits: self.read_var(v, env, base.span)?,
                            dim,
                        })
                    }
                    Sym::Local(l) => {
                        let dim = self.locals[l].dim;
                        Ok(Base::Vector {
                            bits: self.read_sym(&sym, env, base.span)?,
                            dim,
                        })
                    }
                    Sym::Param(c) => Ok(Base::Vector {
                        dim: Dim::of_width(c.bits.len()),
                        bits: konst(&c.bits),
                    }),
                    Sym::Func(..) | Sym::Task(..) => {
                        bail!(base.span, "a function or task is not a value")
                    }
                }
            }
            // mem[a][b]: a word, then a select in it.
            ExprKind::Index { base: inner, index } => {
                if let Some(Sym::Mem(m)) = self.expr_sym(inner, scope)? {
                    let (addr, _) = self.lower_self(index, scope, env)?;
                    let word = self.mem_read(m, addr, env, base.span)?;
                    return Ok(Base::Vector {
                        bits: word,
                        dim: self.mems[m].word,
                    });
                }
                let (bits, t) = self.lower_self(base, scope, env)?;
                Ok(Base::Vector {
                    bits,
                    dim: Dim::of_width(t.width),
                })
            }
            _ => {
                let (bits, t) = self.lower_self(base, scope, env)?;
                Ok(Base::Vector {
                    bits,
                    dim: Dim::of_width(t.width),
                })
            }
        }
    }

    /// `v[i]`: a constant index takes the bit (x if out of range); a
    /// variable one shifts with `$shiftx`, as yosys does.
    fn bit_select(
        &mut self,
        bits: &SigSpec,
        dim: Dim,
        index: &'a Expr,
        scope: ScopeId,
        env: &mut Env,
    ) -> Result<SigSpec> {
        let (idx, t) = self.lower_self(index, scope, env)?;
        if let Some(c) = as_const(&idx) {
            let cv = ConstVal {
                bits: c,
                signed: t.signed,
                string: false,
            };
            return Ok(vec![match cv.as_i64().and_then(|i| dim.pos(i)) {
                Some(p) => bits[p],
                None => SigBit::X,
            }]);
        }
        let amount = self.position(idx, t, dim, 0, index.span);
        Ok(self.b.binary(
            BinaryOp::Shiftx,
            bits.clone(),
            false,
            amount.0,
            amount.1,
            1,
            index.span,
        ))
    }

    /// The bit position of index `idx + delta` in a vector declared `dim`,
    /// as a signal and whether it's signed.
    pub fn position(
        &mut self,
        idx: SigSpec,
        t: Ty,
        dim: Dim,
        delta: i64,
        span: Span,
    ) -> (SigSpec, bool) {
        // pos = idx - lsb (descending) or lsb - idx (ascending).
        let k = if dim.upto() {
            dim.lsb - delta
        } else {
            delta - dim.lsb
        };
        if !dim.upto() && k == 0 {
            return (idx, t.signed);
        }
        let kbits = 64 - k.unsigned_abs().leading_zeros() as usize + 1;
        let w = t.width.max(kbits) + 1;
        let a = extend(&idx, w, t.signed);
        let kc = konst(&chipsmith_netlist::Const::from_int(k, w).0);
        let v = if dim.upto() {
            self.b.binary(BinaryOp::Sub, kc, true, a, true, w, span)
        } else {
            self.b.binary(BinaryOp::Add, a, true, kc, true, w, span)
        };
        (v, true)
    }

    /// `v[start +: width]` / `v[start -: width]`.
    #[allow(clippy::too_many_arguments)]
    fn indexed_select(
        &mut self,
        bits: &SigSpec,
        dim: Dim,
        start: &'a Expr,
        w: usize,
        up: bool,
        scope: ScopeId,
        env: &mut Env,
        span: Span,
    ) -> Result<SigSpec> {
        let (idx, t) = self.lower_self(start, scope, env)?;
        // The index range is [lo, lo + w - 1]; the result's LSB is the bit
        // at the lower position.
        let lo_delta = if up { 0 } else { -(w as i64 - 1) };
        let lsb_delta = if dim.upto() {
            lo_delta + w as i64 - 1
        } else {
            lo_delta
        };
        if let Some(c) = as_const(&idx) {
            let cv = ConstVal {
                bits: c,
                signed: t.signed,
                string: false,
            };
            let Some(i) = cv.as_i64() else {
                return Ok(xs(w));
            };
            let p0 = dim.pos_unchecked(i + lsb_delta);
            return Ok((0..w as i64)
                .map(|k| {
                    let p = p0 + k;
                    if (0..bits.len() as i64).contains(&p) {
                        bits[p as usize]
                    } else {
                        SigBit::X
                    }
                })
                .collect());
        }
        let (amount, signed) = self.position(idx, t, dim, lsb_delta, span);
        Ok(self.b.binary(
            BinaryOp::Shiftx,
            bits.clone(),
            false,
            amount,
            signed,
            w,
            span,
        ))
    }

    /// Reads a memory word.
    pub fn mem_read(
        &mut self,
        m: usize,
        addr: SigSpec,
        env: &mut Env,
        span: Span,
    ) -> Result<SigSpec> {
        super::proc::mem_read(self, m, addr, env, span)
    }

    /// Calls a function: its body runs in `env` with fresh locals for its
    /// arguments and result.
    pub fn call_function(
        &mut self,
        f: &'a crate::ast::Subroutine,
        def: ScopeId,
        args: &'a [Expr],
        scope: ScopeId,
        env: &mut Env,
        span: Span,
    ) -> Result<SigSpec> {
        if self.depth > 256 {
            bail!(span, "function {} recurses too deeply", f.name.name);
        }
        let ret_ty = self.function_ty(f, def)?;
        let fscope = self.new_scope(Some(def), String::new());
        let ret_dim = match &f.ret.as_ref().and_then(|r| r.range.as_ref()) {
            Some(r) => self.eval_dim(r, def)?,
            None => Dim::of_width(ret_ty.width),
        };
        let ret = self.new_local(fscope, &f.name.name, ret_dim, ret_ty.signed, f.name.span);
        for p in &f.params {
            self.declare_param(fscope, p)?;
        }
        let mut inputs = Vec::new();
        for d in &f.decls {
            for n in &d.names {
                let (dim, signed) = self.decl_dim(&d.ty, fscope)?;
                if !n.dims.is_empty() {
                    bail!(n.name.span, "arrays in functions are not supported");
                }
                let l = self.new_local(fscope, &n.name.name, dim, signed, n.name.span);
                match d.dir {
                    Some(crate::ast::PortDir::Input) => inputs.push(l),
                    Some(_) => bail!(d.span, "a function's arguments are inputs"),
                    None => {}
                }
            }
        }
        if inputs.len() != args.len() {
            bail!(
                span,
                "function {} takes {} arguments, given {}",
                f.name.name,
                inputs.len(),
                args.len()
            );
        }
        // Arguments are evaluated in the caller's scope and env first.
        let mut values = Vec::new();
        for (&l, a) in inputs.iter().zip(args) {
            let lt = self.locals[l].ty();
            let at = self.self_ty(a, scope)?;
            let t = Ty {
                width: lt.width.max(at.width),
                signed: at.signed,
            };
            let mut v = self.lower(a, t, scope, env)?;
            v.truncate(lt.width);
            values.push((l, v));
        }
        env.push();
        for (l, v) in values {
            env.set(Slot::Local(l), v);
        }
        self.depth += 1;
        let r = self.exec(&f.body, fscope, env);
        self.depth -= 1;
        let frame = env.pop();
        r?;
        if let Some(v) = frame.keys().find_map(|s| match s {
            Slot::Var(v) => Some(*v),
            _ => None,
        }) {
            bail!(
                span,
                "function {} assigns `{}`, which isn't one of its own variables",
                f.name.name,
                self.vars[v].name
            );
        }
        Ok(frame
            .get(&Slot::Local(ret))
            .cloned()
            .unwrap_or_else(|| xs(ret_ty.width)))
    }

    pub fn new_local(
        &mut self,
        scope: ScopeId,
        name: &Rc<str>,
        dim: Dim,
        signed: bool,
        span: Span,
    ) -> usize {
        self.locals.push(super::Local { dim, signed });
        let id = self.locals.len() - 1;
        self.scopes[scope]
            .names
            .insert(name.clone(), (Sym::Local(id), span));
        id
    }
}

/// `v[m:l]` with constant bounds; bits outside the vector are x.
pub fn const_part(bits: &[SigBit], dim: Dim, m: i64, l: i64) -> SigSpec {
    let (p_lo, p_hi) = {
        let a = dim.pos_unchecked(l);
        let b = dim.pos_unchecked(m);
        (a.min(b), a.max(b))
    };
    (p_lo..=p_hi)
        .map(|p| {
            if (0..bits.len() as i64).contains(&p) {
                bits[p as usize]
            } else {
                SigBit::X
            }
        })
        .collect()
}
