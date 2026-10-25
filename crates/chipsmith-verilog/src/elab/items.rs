//! A module's items: declarations, continuous assignments, instances,
//! gate primitives and generate constructs, and finishing the netlist
//! module.
//!
//! Each scope (the module, each generate block) is done in two passes, as
//! names must be declared before their uses elaborate: first functions,
//! parameters and declarations ([`ModElab::declare_items`]), then
//! everything that builds logic ([`ModElab::elaborate_items`]).

use std::rc::Rc;

use chipsmith_netlist::{
    attr, Attributes, CellKind, Const, Dir, Generic, GenericPort, Module, ParamValue, SigBit,
    SigSpec, State, Wire,
};
use rustc_hash::{FxHashMap, FxHashSet};

use super::proc::{MemStorage, ProcOut};
use super::stmt::Mode;
use super::{ConstVal, DesignCtx, Dim, Env, Local, MemVar, PortInfo, Scope, ScopeId, Sym, Ty, Var};
use crate::ast::{
    self, DataType, Expr, ExprKind, GateKind, Ident, InstanceItem, Item, NetDecl, NetKind,
    ParamDecl, ParamKind, ParamOverride, PortDir, Stmt, StmtKind,
};
use crate::build::Builder;
use crate::pp::DefaultNettype;
use crate::source::{bail, Diag, Result, Span};

pub(crate) struct ModElab<'a, 'd> {
    pub d: &'d mut DesignCtx<'a>,
    pub ast: &'a ast::Module,
    pub b: Builder<'a>,
    pub scopes: Vec<Scope<'a>>,
    pub vars: Vec<Var>,
    pub mems: Vec<MemVar>,
    pub locals: Vec<Local>,
    /// Parameter values given by the instance (and defparams).
    overrides: FxHashMap<String, ConstVal>,
    /// Memories that must become registers (see [`super::proc`]).
    mem2reg: FxHashSet<Rc<str>>,
    /// Port names in header order, and their variables once declared.
    ports: Vec<(Ident, Option<usize>)>,
    /// `defparam` assignments in this module: path below it, value.
    defparams: Vec<(Vec<String>, ConstVal, Span)>,
    /// Block-local `reg`s already declared, by declaration and scope
    /// prefix: a named block in a loop declares its variables once.
    block_vars: FxHashMap<(Span, String), usize>,
    pub outs: Vec<ProcOut>,
    /// `for` loop variables' final values.
    pub loop_finals: FxHashMap<usize, Vec<State>>,
    /// Hidden env slots handed out.
    pub hidden: usize,
    /// Function and task call depth.
    pub depth: usize,
}

pub(crate) fn elaborate_module<'a>(
    d: &mut DesignCtx<'a>,
    ast: &'a ast::Module,
    name: &str,
    overrides: &[(String, ConstVal)],
) -> Result<(Module, Vec<PortInfo>)> {
    let sources = d.sources;
    let lib = ast.is_blackbox();
    let mut m = ModElab {
        d,
        ast,
        b: Builder::new(name, sources),
        scopes: Vec::new(),
        vars: Vec::new(),
        mems: Vec::new(),
        locals: Vec::new(),
        overrides: overrides.iter().cloned().collect(),
        mem2reg: FxHashSet::default(),
        ports: ast.ports.iter().map(|p| (p.clone(), None)).collect(),
        defparams: Vec::new(),
        block_vars: FxHashMap::default(),
        outs: Vec::new(),
        loop_finals: FxHashMap::default(),
        hidden: 0,
        depth: 0,
    };
    let root = m.new_scope(None, String::new());
    if !lib {
        prescan_mem2reg(&ast.items, &mut m.mem2reg);
    }
    m.declare_items(root, &ast.items, lib)?;
    for (p, v) in &m.ports {
        if v.is_none() {
            bail!(p.span, "port `{}` has no direction declared", p.name);
        }
    }
    // A library cell takes any parameters (they're passed on to the
    // cell); only the ones it declares shape its ports.
    if let Some(unused) = m.overrides.keys().filter(|_| !lib).find(|k| {
        !ast.items.iter().any(|i| match i {
            Item::Param(p) => p.assigns.iter().any(|(n, _)| &*n.name == k.as_str()),
            _ => false,
        })
    }) {
        bail!(
            ast.name.span,
            "module {} has no parameter {unused}",
            ast.name.name
        );
    }
    if !lib {
        m.elaborate_items(root, &ast.items)?;
    }
    m.finish(lib)
}

/// Memories written from logic or with `=` anywhere in these items.
fn prescan_mem2reg(items: &[Item], out: &mut FxHashSet<Rc<str>>) {
    fn stmt(s: &Stmt, comb: bool, out: &mut FxHashSet<Rc<str>>) {
        match &s.kind {
            StmtKind::Assign { lhs, blocking, .. } => {
                if comb || *blocking {
                    lhs_mems(lhs, out);
                }
            }
            StmtKind::Block { stmts, .. } => stmts.iter().for_each(|s| stmt(s, comb, out)),
            StmtKind::If { then, els, .. } => {
                stmt(then, comb, out);
                if let Some(e) = els {
                    stmt(e, comb, out);
                }
            }
            StmtKind::Case { items, .. } => items.iter().for_each(|i| stmt(&i.body, comb, out)),
            StmtKind::For { body, .. }
            | StmtKind::While { body, .. }
            | StmtKind::Repeat { body, .. } => stmt(body, comb, out),
            StmtKind::Event { body, .. } => stmt(body, comb, out),
            _ => {}
        }
    }
    fn lhs_mems(e: &Expr, out: &mut FxHashSet<Rc<str>>) {
        match &e.kind {
            ExprKind::Index { base, .. }
            | ExprKind::Slice { base, .. }
            | ExprKind::IndexedSlice { base, .. } => match &base.kind {
                ExprKind::Ident(id) => {
                    out.insert(id.name.clone());
                }
                _ => lhs_mems(base, out),
            },
            ExprKind::Concat(parts) => parts.iter().for_each(|p| lhs_mems(p, out)),
            _ => {}
        }
    }
    for it in items {
        match it {
            Item::Always { body, .. } => {
                let comb = match &body.kind {
                    StmtKind::Event { event, .. } => match event {
                        ast::Event::Star => true,
                        ast::Event::List(l) => l.iter().all(|i| i.edge.is_none()),
                    },
                    _ => true,
                };
                stmt(body, comb, out);
            }
            Item::GenFor { body, .. } | Item::GenBlock(body) => prescan_mem2reg(&body.items, out),
            Item::GenIf { then, els, .. } => {
                prescan_mem2reg(&then.items, out);
                if let Some(e) = els {
                    prescan_mem2reg(&e.items, out);
                }
            }
            Item::GenCase { items, .. } => {
                for i in items {
                    prescan_mem2reg(&i.body.items, out);
                }
            }
            _ => {}
        }
    }
}

fn dir_of(d: PortDir) -> Dir {
    match d {
        PortDir::Input => Dir::In,
        PortDir::Output => Dir::Out,
        PortDir::Inout => Dir::InOut,
    }
}

impl<'a, 'd> ModElab<'a, 'd> {
    pub fn new_scope(&mut self, parent: Option<ScopeId>, prefix: String) -> ScopeId {
        self.scopes.push(Scope {
            parent,
            prefix,
            names: FxHashMap::default(),
            blocks: FxHashMap::default(),
            generate_count: 0,
        });
        self.scopes.len() - 1
    }

    fn define(&mut self, scope: ScopeId, id: &Ident, sym: Sym<'a>) -> Result<()> {
        if let Some((_, prev)) = self.scopes[scope].names.get(&id.name) {
            bail!(
                id.span,
                "`{}` is already declared (at {})",
                id.name,
                self.b.sources.location(*prev)
            );
        }
        self.scopes[scope]
            .names
            .insert(id.name.clone(), (sym, id.span));
        Ok(())
    }

    fn attrs(&mut self, attrs: &'a [ast::Attr], scope: ScopeId, span: Span) -> Result<Attributes> {
        let mut out = Attributes::new();
        out.insert(attr::SRC.into(), ParamValue::Str(self.b.sources.src(span)));
        for a in attrs {
            let v = match &a.value {
                None => ParamValue::int(1),
                Some(e) => {
                    let c = self.eval_const(e, scope)?;
                    if c.string {
                        ParamValue::Str(c.as_string())
                    } else {
                        ParamValue::Bits(Const(c.bits))
                    }
                }
            };
            out.insert(a.name.name.to_string(), v);
        }
        Ok(out)
    }

    /// A declaration's packed range and signedness.
    pub fn decl_dim(&mut self, ty: &'a DataType, scope: ScopeId) -> Result<(Dim, bool)> {
        Ok(match ty.kind {
            Some(NetKind::Integer) => (Dim::of_width(32), true),
            Some(NetKind::Time) => (Dim::of_width(64), false),
            _ => (
                match &ty.range {
                    Some(r) => self.eval_dim(r, scope)?,
                    None => Dim::of_width(1),
                },
                ty.signed,
            ),
        })
    }

    // -----------------------------------------------------------------
    // Pass 1: declarations

    fn declare_items(&mut self, scope: ScopeId, items: &'a [Item], lib: bool) -> Result<()> {
        for it in items {
            match it {
                Item::Function(f) => self.define(scope, &f.name, Sym::Func(f, scope))?,
                Item::Task(t) => self.define(scope, &t.name, Sym::Task(t, scope))?,
                _ => {}
            }
        }
        // In the module scope: which names are ports, and their separate
        // net declarations (`output [3:0] q; reg [3:0] q;`).
        let mut port_nets: FxHashMap<Rc<str>, &'a NetDecl> = FxHashMap::default();
        if scope == 0 {
            for it in items {
                if let Item::Net(d) = it {
                    if d.dir.is_none() {
                        for n in &d.names {
                            if self.ports.iter().any(|(p, _)| p.name == n.name.name) {
                                port_nets.insert(n.name.name.clone(), d);
                            }
                        }
                    }
                }
            }
        }
        let has_header_params = self
            .ast
            .items
            .iter()
            .any(|i| matches!(i, Item::Param(p) if p.header));
        for it in items {
            match it {
                Item::Param(p) => {
                    let overridable = scope == 0 && !p.local && (p.header || !has_header_params);
                    self.declare_param_with(scope, p, overridable)?;
                }
                Item::Net(d) => {
                    if let Some(dir) = d.dir {
                        let merged = d
                            .names
                            .iter()
                            .find_map(|n| port_nets.get(&n.name.name).copied());
                        self.declare_port(scope, d, dir, merged)?;
                    } else if !(scope == 0
                        && d.names.iter().all(|n| port_nets.contains_key(&n.name.name)))
                    {
                        if lib {
                            continue;
                        }
                        self.declare_net(scope, d)?;
                    }
                }
                Item::Defparam(list) if !lib => {
                    for (path, e) in list {
                        let v = self.eval_const(e, scope)?;
                        let prefix = self.scopes[scope].prefix.clone();
                        let mut p: Vec<String> = path.iter().map(|i| i.name.to_string()).collect();
                        if let Some(first) = p.first_mut() {
                            *first = format!("{prefix}{first}");
                        }
                        self.defparams.push((p, v, e.span));
                    }
                }
                _ => {}
            }
        }
        if !lib {
            self.declare_implicit(scope, items)?;
        }
        Ok(())
    }

    pub fn declare_param(&mut self, scope: ScopeId, p: &'a ParamDecl) -> Result<()> {
        self.declare_param_with(scope, p, false)
    }

    fn declare_param_with(
        &mut self,
        scope: ScopeId,
        p: &'a ParamDecl,
        overridable: bool,
    ) -> Result<()> {
        if p.kind == ParamKind::Real {
            bail!(p.span, "real parameters are not supported");
        }
        for (id, e) in &p.assigns {
            let given = if overridable {
                self.overrides.get(&*id.name).cloned()
            } else {
                None
            };
            let v = match given {
                Some(v) => v,
                None => self.eval_const(e, scope)?,
            };
            let v = self.param_type(p, v, scope)?;
            self.define(scope, id, Sym::Param(v))?;
        }
        Ok(())
    }

    /// A parameter's value converted to its declared type: a range gives
    /// the width, `integer` 32 signed bits, nothing the value's own type
    /// (IEEE 1364-2005 §12.2.1).
    fn param_type(&mut self, p: &'a ParamDecl, v: ConstVal, scope: ScopeId) -> Result<ConstVal> {
        Ok(if p.kind == ParamKind::Integer {
            ConstVal {
                bits: crate::fold::extend(&v.bits, 32, v.signed),
                signed: true,
                string: false,
            }
        } else if let Some(r) = &p.range {
            let w = self.eval_dim(r, scope)?.width();
            ConstVal {
                bits: crate::fold::extend(&v.bits, w, v.signed),
                signed: p.signed,
                string: v.string,
            }
        } else if p.signed {
            ConstVal { signed: true, ..v }
        } else {
            v
        })
    }

    fn declare_port(
        &mut self,
        scope: ScopeId,
        d: &'a NetDecl,
        dir: PortDir,
        net: Option<&'a NetDecl>,
    ) -> Result<()> {
        // The type comes from the port declaration or the net declaration,
        // whichever says.
        let kind =
            d.ty.kind
                .or(net.and_then(|n| n.ty.kind))
                .unwrap_or(NetKind::Wire);
        let range =
            d.ty.range
                .as_ref()
                .or(net.and_then(|n| n.ty.range.as_ref()));
        let ty = DataType {
            kind: Some(kind),
            signed: d.ty.signed || net.is_some_and(|n| n.ty.signed),
            range: range.cloned(),
        };
        // `range` above borrows from the AST; the merged type is built
        // fresh, so evaluate its range here.
        let (dim, signed) = match kind {
            NetKind::Integer => (Dim::of_width(32), true),
            NetKind::Time => (Dim::of_width(64), false),
            _ => (
                match range {
                    Some(r) => self.eval_dim(r, scope)?,
                    None => Dim::of_width(1),
                },
                ty.signed,
            ),
        };
        for n in &d.names {
            let Some(pi) = self.ports.iter().position(|(p, _)| p.name == n.name.name) else {
                bail!(
                    n.name.span,
                    "`{}` is declared as a port but isn't in the port list",
                    n.name.name
                );
            };
            if !n.dims.is_empty() {
                bail!(n.name.span, "array ports are not supported");
            }
            let mut attrs = self.attrs(&d.attrs, scope, d.span)?;
            if let Some(net) = net {
                attrs.extend(self.attrs(&net.attrs, scope, net.span)?);
                attrs.insert(
                    attr::SRC.into(),
                    ParamValue::Str(self.b.sources.src(d.span)),
                );
            }
            let v = self.new_var(
                scope,
                &n.name,
                dim,
                signed,
                kind.is_variable(),
                Some(dir_of(dir)),
                attrs,
            )?;
            self.ports[pi].1 = Some(v);
        }
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    fn new_var(
        &mut self,
        scope: ScopeId,
        id: &Ident,
        dim: Dim,
        signed: bool,
        variable: bool,
        port: Option<Dir>,
        attrs: Attributes,
    ) -> Result<usize> {
        let nets = self.b.new_sig(dim.width());
        self.vars.push(Var {
            name: format!("{}{}", self.scopes[scope].prefix, id.name),
            dim,
            signed,
            nets,
            variable,
            port,
            attrs,
            init: None,
            span: id.span,
            cont_driven: false,
        });
        let v = self.vars.len() - 1;
        self.define(scope, id, Sym::Var(v))?;
        Ok(v)
    }

    /// `wire`/`reg`/`integer` declarations (not ports), and memories. A
    /// named block's variables are module variables (they are static,
    /// and reachable as `block.name`), declared once however often the
    /// block runs; in a function or task they are locals.
    pub fn declare_net(&mut self, scope: ScopeId, d: &'a NetDecl) -> Result<()> {
        let kind = d.ty.kind.unwrap_or(NetKind::Wire);
        let (dim, signed) = self.decl_dim(&d.ty, scope)?;
        for n in &d.names {
            // In a function or task, a block's variables are locals.
            if self.depth > 0 {
                if !n.dims.is_empty() {
                    bail!(
                        n.name.span,
                        "arrays in functions and tasks are not supported"
                    );
                }
                self.new_local(scope, &n.name.name, dim, signed, n.name.span);
                continue;
            }
            let key = (n.name.span, self.scopes[scope].prefix.clone());
            if let Some(&v) = self.block_vars.get(&key) {
                self.scopes[scope]
                    .names
                    .insert(n.name.name.clone(), (Sym::Var(v), n.name.span));
                continue;
            }
            let attrs = self.attrs(&d.attrs, scope, d.span)?;
            if !n.dims.is_empty() {
                self.declare_memory(scope, d, n, dim, signed, attrs)?;
                continue;
            }
            let v = self.new_var(scope, &n.name, dim, signed, kind.is_variable(), None, attrs)?;
            self.block_vars.insert(key, v);
            match kind {
                NetKind::Supply0 | NetKind::Supply1 => {
                    let s = if kind == NetKind::Supply1 {
                        SigBit::ONE
                    } else {
                        SigBit::ZERO
                    };
                    let nets = self.vars[v].nets.clone();
                    for b in nets {
                        self.b.join(b, s, n.name.span);
                    }
                }
                _ => {}
            }
        }
        Ok(())
    }

    fn declare_memory(
        &mut self,
        scope: ScopeId,
        d: &'a NetDecl,
        n: &'a ast::Declarator,
        word: Dim,
        signed: bool,
        attrs: Attributes,
    ) -> Result<()> {
        if !matches!(d.ty.kind, Some(NetKind::Reg | NetKind::Integer)) {
            bail!(n.name.span, "only `reg` arrays (memories) are supported");
        }
        if n.dims.len() > 1 {
            bail!(n.name.span, "multi-dimensional arrays are not supported");
        }
        if n.init.is_some() {
            bail!(
                n.name.span,
                "a memory can't have an initializer; use an initial block"
            );
        }
        let words = self.eval_dim(&n.dims[0], scope)?;
        let (lo, hi) = (words.msb.min(words.lsb), words.msb.max(words.lsb));
        let size = (hi - lo + 1) as usize;
        let name = format!("{}{}", self.scopes[scope].prefix, n.name.name);
        let force = attrs.contains_key("mem2reg");
        let never = attrs.contains_key("nomem2reg");
        let storage = if (force || self.mem2reg.contains(&n.name.name)) && !never {
            let mut regs = Vec::with_capacity(size);
            for i in 0..size {
                let id = Ident {
                    name: format!("{}[{}]", n.name.name, lo + i as i64).into(),
                    span: n.name.span,
                };
                let mut a = attrs.clone();
                a.remove("mem2reg");
                let v = self.new_var(scope, &id, word, signed, true, None, a)?;
                regs.push(v);
            }
            MemStorage::Regs(regs)
        } else {
            MemStorage::Ram {
                rd: Vec::new(),
                wr: Vec::new(),
            }
        };
        self.mems.push(MemVar {
            name,
            word,
            signed,
            lo,
            hi,
            attrs,
            span: n.name.span,
            init: vec![State::X; size * word.width()],
            storage,
        });
        let m = self.mems.len() - 1;
        self.define(scope, &n.name, Sym::Mem(m))
    }

    /// Implicit nets (IEEE 1364-2005 §4.5): an undeclared name on the left
    /// of a continuous assignment or in a port connection is a 1-bit wire,
    /// unless `` `default_nettype none ``.
    fn declare_implicit(&mut self, scope: ScopeId, items: &'a [Item]) -> Result<()> {
        let mut names: Vec<&'a Ident> = Vec::new();
        fn collect<'x>(e: &'x Expr, out: &mut Vec<&'x Ident>) {
            match &e.kind {
                ExprKind::Ident(id) => out.push(id),
                ExprKind::Concat(parts) => parts.iter().for_each(|p| collect(p, out)),
                _ => {}
            }
        }
        for it in items {
            match it {
                Item::Assign { lhs, .. } => collect(lhs, &mut names),
                Item::Instance(i) => {
                    for inst in &i.instances {
                        for c in &inst.conns {
                            if let Some(e) = &c.expr {
                                collect(e, &mut names);
                            }
                        }
                    }
                }
                Item::Gate(g) => g.terminals.iter().for_each(|t| collect(t, &mut names)),
                _ => {}
            }
        }
        for id in names {
            if self.lookup(scope, &id.name).is_some() {
                continue;
            }
            if self.ast.nettype == DefaultNettype::None {
                bail!(
                    id.span,
                    "`{}` is not declared (`default_nettype none)",
                    id.name
                );
            }
            let attrs = self.attrs(&[], scope, id.span)?;
            self.new_var(scope, id, Dim::of_width(1), false, false, None, attrs)?;
        }
        Ok(())
    }

    // -----------------------------------------------------------------
    // Pass 2: logic

    fn elaborate_items(&mut self, scope: ScopeId, items: &'a [Item]) -> Result<()> {
        for it in items {
            match it {
                Item::Net(d) => {
                    for n in &d.names {
                        let Some(init) = &n.init else { continue };
                        let Some((Sym::Var(v), _)) = self.lookup(scope, &n.name.name) else {
                            continue;
                        };
                        if self.vars[v].variable {
                            self.declare_init(v, init, scope)?;
                        } else {
                            self.vars[v].cont_driven = true;
                            let lhs = self.vars[v].nets.clone();
                            self.cont_assign_bits(lhs, init, scope, d.span)?;
                        }
                    }
                }
                Item::Assign { lhs, rhs, span, .. } => self.cont_assign(lhs, rhs, scope, *span)?,
                Item::Always { body, span, .. } => self.always(body, scope, *span)?,
                Item::Initial { body, .. } => self.initial(body, scope)?,
                Item::Instance(i) => self.instance(i, scope)?,
                Item::Gate(g) => self.gate(g, scope)?,
                Item::GenFor { .. }
                | Item::GenIf { .. }
                | Item::GenCase { .. }
                | Item::GenBlock(_) => self.generate(it, scope, None)?,
                Item::Param(_)
                | Item::Genvar
                | Item::Function(_)
                | Item::Task(_)
                | Item::Defparam(_) => {}
            }
        }
        Ok(())
    }

    /// The nets an expression names, for the left of a continuous
    /// assignment or an output port connection.
    pub fn net_lvalue(&mut self, e: &'a Expr, scope: ScopeId) -> Result<SigSpec> {
        Ok(match &e.kind {
            ExprKind::Ident(_) | ExprKind::Hier(_) => {
                let sym = match &e.kind {
                    ExprKind::Ident(id) => self.resolve(scope, id)?,
                    ExprKind::Hier(p) => self.resolve_hier(scope, p, e.span)?,
                    _ => unreachable!(),
                };
                match sym {
                    Sym::Var(v) => {
                        self.vars[v].cont_driven = true;
                        self.vars[v].nets.clone()
                    }
                    _ => bail!(e.span, "not a net"),
                }
            }
            ExprKind::Concat(parts) => {
                let mut v = SigSpec::new();
                for p in parts.iter().rev() {
                    v.extend(self.net_lvalue(p, scope)?);
                }
                v
            }
            ExprKind::Index { base, index } => {
                let (nets, dim) = self.lvalue_base(base, scope)?;
                let i = self.eval_i64(index, scope).map_err(|_| {
                    Diag::new(
                        index.span,
                        "a variable index isn't supported on the left of a continuous assignment",
                    )
                })?;
                match dim.pos(i) {
                    Some(p) => vec![nets[p]],
                    None => bail!(index.span, "bit {i} is outside [{}:{}]", dim.msb, dim.lsb),
                }
            }
            ExprKind::Slice { base, msb, lsb } => {
                let (nets, dim) = self.lvalue_base(base, scope)?;
                let m = self.eval_i64(msb, scope)?;
                let l = self.eval_i64(lsb, scope)?;
                let v = super::expr::const_part(&nets, dim, m, l);
                if v.contains(&SigBit::X) {
                    bail!(
                        e.span,
                        "part-select [{m}:{l}] is outside [{}:{}]",
                        dim.msb,
                        dim.lsb
                    );
                }
                v
            }
            ExprKind::IndexedSlice {
                base,
                start,
                width,
                up,
            } => {
                let (nets, dim) = self.lvalue_base(base, scope)?;
                let s = self.eval_i64(start, scope)?;
                let w = self.eval_i64(width, scope)?;
                let (m, l) = match (up, dim.upto()) {
                    (true, false) => (s + w - 1, s),
                    (true, true) => (s, s + w - 1),
                    (false, false) => (s, s - w + 1),
                    (false, true) => (s - w + 1, s),
                };
                let v = super::expr::const_part(&nets, dim, m, l);
                if v.contains(&SigBit::X) {
                    bail!(e.span, "part-select is outside [{}:{}]", dim.msb, dim.lsb);
                }
                v
            }
            _ => bail!(e.span, "not something a continuous assignment can drive"),
        })
    }

    fn lvalue_base(&mut self, base: &'a Expr, scope: ScopeId) -> Result<(SigSpec, Dim)> {
        let sym = match &base.kind {
            ExprKind::Ident(id) => self.resolve(scope, id)?,
            ExprKind::Hier(p) => self.resolve_hier(scope, p, base.span)?,
            _ => bail!(base.span, "only a simple name can be selected from here"),
        };
        match sym {
            Sym::Var(v) => {
                self.vars[v].cont_driven = true;
                Ok((self.vars[v].nets.clone(), self.vars[v].dim))
            }
            Sym::Mem(_) => bail!(
                base.span,
                "a memory can't be driven by a continuous assignment"
            ),
            _ => bail!(base.span, "not a net"),
        }
    }

    fn cont_assign(
        &mut self,
        lhs: &'a Expr,
        rhs: &'a Expr,
        scope: ScopeId,
        span: Span,
    ) -> Result<()> {
        let nets = self.net_lvalue(lhs, scope)?;
        self.cont_assign_bits(nets, rhs, scope, span)
    }

    fn cont_assign_bits(
        &mut self,
        lhs: SigSpec,
        rhs: &'a Expr,
        scope: ScopeId,
        span: Span,
    ) -> Result<()> {
        let rt = self.self_ty(rhs, scope)?;
        let mut env = Env::new(Mode::Cont);
        let mut v = self.lower(
            rhs,
            Ty {
                width: lhs.len().max(rt.width),
                signed: rt.signed,
            },
            scope,
            &mut env,
        )?;
        v.truncate(lhs.len());
        self.b.join_sig(&lhs, &v, span);
        Ok(())
    }

    // -----------------------------------------------------------------
    // Generate

    fn generate(&mut self, it: &'a Item, scope: ScopeId, number: Option<u32>) -> Result<()> {
        let n = match number {
            Some(n) => n,
            None => {
                self.scopes[scope].generate_count += 1;
                self.scopes[scope].generate_count
            }
        };
        match it {
            Item::GenIf {
                cond, then, els, ..
            } => {
                let c = self.eval_const(cond, scope)?;
                let Some(t) = c.is_true() else {
                    bail!(
                        cond.span,
                        "a generate condition must be defined (0 or 1), not x"
                    );
                };
                let chosen = if t { Some(then) } else { els.as_ref() };
                if let Some(b) = chosen {
                    // `else if`: the nested if belongs to this construct.
                    if b.name.is_none() && b.items.len() == 1 {
                        if let Item::GenIf { .. } | Item::GenCase { .. } = &b.items[0] {
                            return self.generate(&b.items[0], scope, Some(n));
                        }
                    }
                    self.gen_block(b, scope, n, None)?;
                }
            }
            Item::GenCase { expr, items, .. } => {
                let v = self.eval_const(expr, scope)?;
                let mut chosen = None;
                'items: for gi in items {
                    for e in &gi.exprs {
                        let p = self.eval_const(e, scope)?;
                        let w = v.bits.len().max(p.bits.len());
                        let signed = v.signed && p.signed;
                        if crate::fold::extend(&v.bits, w, signed)
                            == crate::fold::extend(&p.bits, w, signed)
                        {
                            chosen = Some(&gi.body);
                            break 'items;
                        }
                    }
                }
                if chosen.is_none() {
                    chosen = items.iter().find(|i| i.exprs.is_empty()).map(|i| &i.body);
                }
                if let Some(b) = chosen {
                    self.gen_block(b, scope, n, None)?;
                }
            }
            Item::GenFor {
                genvar,
                init,
                cond,
                step_var,
                step,
                body,
                span,
            } => {
                if step_var.name != genvar.name {
                    bail!(
                        step_var.span,
                        "the loop must step its own genvar `{}`",
                        genvar.name
                    );
                }
                let loop_scope = self.new_scope(Some(scope), self.scopes[scope].prefix.clone());
                let mut i = self.eval_const(init, scope)?;
                let mut seen = FxHashSet::default();
                let mut count = 0usize;
                loop {
                    let iv = ConstVal {
                        bits: crate::fold::extend(&i.bits, 32, i.signed),
                        signed: true,
                        string: false,
                    };
                    self.scopes[loop_scope]
                        .names
                        .insert(genvar.name.clone(), (Sym::Param(iv.clone()), genvar.span));
                    match self.eval_const(cond, loop_scope)?.is_true() {
                        Some(true) => {}
                        Some(false) => break,
                        None => bail!(cond.span, "the loop condition is x"),
                    }
                    let Some(index) = iv.as_i64() else {
                        bail!(init.span, "the genvar's value isn't defined");
                    };
                    if !seen.insert(index) {
                        bail!(*span, "the genvar takes the value {index} twice");
                    }
                    count += 1;
                    if count > 1 << 20 {
                        bail!(*span, "generate loop runs too many times");
                    }
                    self.gen_block(body, loop_scope, n, Some(index))?;
                    i = self.eval_const(step, loop_scope)?;
                }
            }
            Item::GenBlock(b) => self.gen_block(b, scope, n, None)?,
            _ => unreachable!("not a generate construct"),
        }
        Ok(())
    }

    fn gen_block(
        &mut self,
        b: &'a ast::GenBlock,
        scope: ScopeId,
        n: u32,
        index: Option<i64>,
    ) -> Result<()> {
        let base = match &b.name {
            Some(id) => id.name.to_string(),
            None => format!("genblk{n}"),
        };
        let name = match index {
            Some(i) => format!("{base}[{i}]"),
            None => base,
        };
        let prefix = format!("{}{name}.", self.scopes[scope].prefix);
        let inner = self.new_scope(Some(scope), prefix);
        // A loop's blocks are found through the loop's own scope's parent.
        let owner = if index.is_some() {
            self.scopes[scope].parent.unwrap_or(scope)
        } else {
            scope
        };
        self.scopes[owner].blocks.insert(name, inner);
        self.declare_items(inner, &b.items, false)?;
        self.elaborate_items(inner, &b.items)
    }

    // -----------------------------------------------------------------
    // Instances

    fn instance(&mut self, item: &'a InstanceItem, scope: ScopeId) -> Result<()> {
        let mname = &item.module.name;
        let Some(&child) = self.d.asts.get(&**mname) else {
            bail!(
                item.module.span,
                "module {mname} is not defined (and isn't in a cell library)"
            );
        };
        // The child's overridable parameters, in order.
        let formal: Vec<&'a Ident> = overridable_params(child);
        for inst in &item.instances {
            let mut given: Vec<(String, ConstVal)> = Vec::new();
            let mut positional = 0;
            for p in &item.params {
                match p {
                    ParamOverride::Positional(e) => {
                        let Some(f) = formal.get(positional) else {
                            bail!(
                                e.span,
                                "module {mname} has only {} parameters",
                                formal.len()
                            );
                        };
                        positional += 1;
                        given.push((f.name.to_string(), self.eval_const(e, scope)?));
                    }
                    ParamOverride::Named(n, Some(e)) => {
                        if !formal.iter().any(|f| f.name == n.name) && !child.is_blackbox() {
                            bail!(n.span, "module {mname} has no parameter {}", n.name);
                        }
                        given.push((n.name.to_string(), self.eval_const(e, scope)?));
                    }
                    ParamOverride::Named(_, None) => {}
                }
            }
            let path = format!("{}{}", self.scopes[scope].prefix, inst.name.name);
            for (p, v, span) in &self.defparams {
                if p.len() == 2 && p[0] == path {
                    if !formal.iter().any(|f| *f.name == *p[1]) {
                        bail!(*span, "module {mname} has no parameter {}", p[1]);
                    }
                    given.retain(|(n, _)| *n != p[1]);
                    given.push((p[1].clone(), v.clone()));
                } else if p.len() > 2 && p[0] == path {
                    bail!(
                        *span,
                        "defparam into a sub-instance's instances is not supported"
                    );
                }
            }
            // Order by the child's declaration, for a stable module name.
            given.sort_by_key(|(n, _)| {
                formal
                    .iter()
                    .position(|f| *f.name == **n)
                    .unwrap_or(usize::MAX)
            });
            let cell_params: Vec<(String, ConstVal)> = given.clone();
            let e = self.d.module(child, given, inst.span)?;
            self.connect_instance(
                item,
                inst,
                &e,
                child.is_blackbox(),
                cell_params,
                scope,
                &path,
            )?;
        }
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    fn connect_instance(
        &mut self,
        item: &'a InstanceItem,
        inst: &'a ast::Instance,
        e: &super::Elaborated,
        lib: bool,
        params: Vec<(String, ConstVal)>,
        scope: ScopeId,
        path: &str,
    ) -> Result<()> {
        let mut conns: Vec<(usize, &'a Expr)> = Vec::new();
        let named = inst.conns.iter().any(|c| c.port.is_some());
        for (k, c) in inst.conns.iter().enumerate() {
            let pi = match &c.port {
                Some(p) => match e.ports.iter().position(|q| *q.name == *p.name) {
                    Some(i) => i,
                    None => bail!(p.span, "module {} has no port {}", item.module.name, p.name),
                },
                None if named => bail!(c.span, "can't mix named and positional connections"),
                None => {
                    if k >= e.ports.len() {
                        bail!(
                            c.span,
                            "module {} has only {} ports",
                            item.module.name,
                            e.ports.len()
                        );
                    }
                    k
                }
            };
            if conns.iter().any(|(i, _)| *i == pi) {
                bail!(c.span, "port {} is connected twice", e.ports[pi].name);
            }
            if let Some(x) = &c.expr {
                conns.push((pi, x));
            }
        }
        conns.sort_by_key(|(i, _)| *i);
        let mut ports = Vec::new();
        for (pi, x) in conns {
            let p = &e.ports[pi];
            let sig = match p.dir {
                Dir::In => {
                    // Like `assign port = expr`.
                    let t = self.self_ty(x, scope)?;
                    let mut env = Env::new(Mode::Cont);
                    let mut v = self.lower(
                        x,
                        Ty {
                            width: p.width.max(t.width),
                            signed: t.signed,
                        },
                        scope,
                        &mut env,
                    )?;
                    v.truncate(p.width);
                    v
                }
                Dir::Out | Dir::InOut => {
                    let mut nets = self.net_lvalue(x, scope).map_err(|d| {
                        Diag::new(
                            x.span,
                            format!("output port {} must connect to nets: {}", p.name, d.message),
                        )
                    })?;
                    // A wider net keeps its top bits undriven; a narrower
                    // one leaves the port's top bits open.
                    if nets.len() < p.width {
                        let extra = self.b.new_sig(p.width - nets.len());
                        nets.extend(extra);
                    }
                    nets.truncate(p.width);
                    nets
                }
            };
            ports.push(GenericPort {
                name: p.name.clone(),
                dir: Some(p.dir),
                sig,
            });
        }
        let params = if lib {
            params
                .into_iter()
                .map(|(n, v)| {
                    (
                        n,
                        if v.string {
                            ParamValue::Str(v.as_string())
                        } else {
                            ParamValue::Bits(Const(v.bits))
                        },
                    )
                })
                .collect()
        } else {
            Default::default()
        };
        let attrs = self.attrs(&item.attrs, scope, inst.span)?;
        let cell = self.b.add_named(
            path.to_owned(),
            CellKind::Other(Generic {
                ty: e.name.clone(),
                params,
                ports,
            }),
            inst.span,
        );
        cell.attributes.extend(attrs);
        Ok(())
    }

    fn gate(&mut self, g: &'a ast::GateInst, scope: ScopeId) -> Result<()> {
        let mut env = Env::new(Mode::Cont);
        let mut ins = Vec::new();
        let (outs, inputs): (&[Expr], &[Expr]) = match g.kind {
            GateKind::Buf | GateKind::Not => {
                let n = g.terminals.len() - 1;
                (&g.terminals[..n], &g.terminals[n..])
            }
            _ => (&g.terminals[..1], &g.terminals[1..]),
        };
        if outs.is_empty() || inputs.is_empty() {
            bail!(g.span, "a gate needs an output and an input");
        }
        for t in inputs {
            let (v, _) = self.lower_self(t, scope, &mut env)?;
            if v.len() != 1 {
                bail!(t.span, "gate terminals must be one bit");
            }
            ins.push(v[0]);
        }
        use chipsmith_netlist::BinaryOp as B;
        let span = g.span;
        let y = match g.kind {
            GateKind::Buf => ins[0],
            GateKind::Not => self.b.not1(ins[0], span),
            GateKind::Bufif0 | GateKind::Bufif1 => {
                if ins.len() != 2 {
                    bail!(span, "bufif takes a data and an enable input");
                }
                let en = if g.kind == GateKind::Bufif1 {
                    ins[1]
                } else {
                    self.b.not1(ins[1], span)
                };
                self.b.tribuf(vec![ins[0]], en, span)[0]
            }
            k => {
                let op = match k {
                    GateKind::And | GateKind::Nand => B::And,
                    GateKind::Or | GateKind::Nor => B::Or,
                    _ => B::Xor,
                };
                let mut acc = ins[0];
                for &i in &ins[1..] {
                    acc = self.b.binary(op, vec![acc], false, vec![i], false, 1, span)[0];
                }
                if matches!(k, GateKind::Nand | GateKind::Nor | GateKind::Xnor) {
                    acc = self.b.not1(acc, span);
                }
                acc
            }
        };
        for o in outs {
            let nets = self.net_lvalue(o, scope)?;
            if nets.len() != 1 {
                bail!(o.span, "gate terminals must be one bit");
            }
            self.b.join(nets[0], y, span);
        }
        Ok(())
    }

    // -----------------------------------------------------------------
    // Finish

    fn finish(mut self, lib: bool) -> Result<(Module, Vec<PortInfo>)> {
        self.finish_processes()?;
        self.finish_memories();
        // A memory split into registers gets its initial contents as the
        // registers' init values.
        for m in &self.mems {
            if let MemStorage::Regs(words) = &m.storage {
                let w = m.width();
                for (i, &v) in words.iter().enumerate() {
                    let init = &m.init[i * w..(i + 1) * w];
                    if init.iter().any(|&b| b != State::X) {
                        self.vars[v].init = Some(init.to_vec());
                    }
                }
            }
        }
        let port_vars: Vec<usize> = self.ports.iter().filter_map(|(_, v)| *v).collect();
        let others: Vec<usize> = (0..self.vars.len())
            .filter(|v| !port_vars.contains(v))
            .collect();
        let mut wires = Vec::new();
        for &v in port_vars.iter().chain(&others) {
            let var = &self.vars[v];
            let mut attributes = var.attrs.clone();
            if let Some(init) = &var.init {
                attributes.insert(attr::INIT.into(), ParamValue::Bits(Const(init.clone())));
            }
            wires.push(Wire {
                name: var.name.clone(),
                bits: var.nets.clone(),
                port: var.port,
                signed: var.signed,
                offset: var.dim.offset(),
                upto: var.dim.upto(),
                attributes,
            });
        }
        self.b.module.wires = wires;
        let ports: Vec<PortInfo> = port_vars
            .iter()
            .map(|&v| PortInfo {
                name: self.vars[v].name.clone(),
                dir: self.vars[v].port.expect("a port"),
                width: self.vars[v].dim.width(),
            })
            .collect();
        let mut attributes = Attributes::new();
        attributes.insert(
            attr::SRC.into(),
            ParamValue::Str(self.b.sources.src(self.ast.span)),
        );
        for a in &self.ast.attrs {
            attributes.insert(a.name.name.to_string(), ParamValue::int(1));
        }
        if lib {
            attributes.insert(attr::BLACKBOX.into(), ParamValue::int(1));
        }
        let span = self.ast.span;
        let vars = std::mem::take(&mut self.vars);
        let mut module = match self.b.finish() {
            Ok(m) => m,
            Err(conflicts) => {
                let (net, at) = conflicts[0];
                let name = vars
                    .iter()
                    .find(|v| v.nets.contains(&SigBit::Net(net)))
                    .map_or("a net".to_owned(), |v| format!("`{}`", v.name));
                bail!(at, "{name} is driven by two different constants");
            }
        };
        module.attributes = attributes;
        check_drivers(&module).map_err(|m| Diag::new(span, m))?;
        Ok((module, ports))
    }
}

/// The parameters an instance can override, in order: the header's if it
/// has a parameter port list, else the body's non-local ones.
fn overridable_params(m: &ast::Module) -> Vec<&Ident> {
    let header = m
        .items
        .iter()
        .any(|i| matches!(i, Item::Param(p) if p.header));
    let mut out = Vec::new();
    for it in &m.items {
        if let Item::Param(p) = it {
            if !p.local && (p.header || !header) {
                out.extend(p.assigns.iter().map(|(n, _)| n));
            }
        }
    }
    out
}

/// Every net has at most one driver: a cell output, an input port or a
/// constant, except that tri-state buffers may share one. The error names
/// the net (a name that isn't an input, if it has one) and its drivers.
fn check_drivers(m: &Module) -> std::result::Result<(), String> {
    let mut drivers: Vec<Vec<String>> = vec![Vec::new(); m.net_count()];
    let mut tri = vec![0u32; m.net_count()];
    for (_, w, dir) in m.ports() {
        if dir == Dir::In {
            for b in w.bits.iter().filter_map(|b| b.net()) {
                drivers[b.index()].push(format!("input port `{}`", w.name));
            }
        }
    }
    let src = |c: &chipsmith_netlist::Cell| match c.attributes.get(attr::SRC) {
        Some(ParamValue::Str(s)) => format!(" at {s}"),
        _ => String::new(),
    };
    for c in &m.cells {
        let is_tri = matches!(c.kind, CellKind::Tribuf(_));
        let mut bad = None;
        c.kind.for_each_port(|p| {
            // An inout may be one of several drivers of a bus.
            if p.dir != Some(Dir::Out) {
                return;
            }
            for b in p.bits {
                match b {
                    SigBit::Net(n) if is_tri => tri[n.index()] += 1,
                    SigBit::Net(n) => drivers[n.index()].push(format!(
                        "{} {}{}",
                        c.kind.type_name(),
                        c.name,
                        src(c)
                    )),
                    SigBit::Const(_) if p.dir == Some(Dir::Out) => {
                        bad = Some(format!(
                            "{} {}{} drives a net that is also assigned a constant",
                            c.kind.type_name(),
                            c.name,
                            src(c)
                        ))
                    }
                    SigBit::Const(_) => {}
                }
            }
        });
        if let Some(b) = bad {
            return Err(b);
        }
    }
    for (i, d) in drivers.iter().enumerate() {
        if d.len() > 1 || (d.len() == 1 && tri[i] > 0) {
            let names: Vec<&Wire> = m
                .wires
                .iter()
                .filter(|w| {
                    w.bits
                        .iter()
                        .any(|b| b.net().is_some_and(|n| n.index() == i))
                })
                .collect();
            let name = names
                .iter()
                .find(|w| w.port != Some(Dir::In))
                .or(names.first())
                .map_or_else(|| format!("net {i}"), |w| w.name.clone());
            let mut list = d.clone();
            if tri[i] > 0 {
                list.push(format!("{} tri-state buffer(s)", tri[i]));
            }
            return Err(format!(
                "`{name}` has more than one driver: {}",
                list.join(", ")
            ));
        }
    }
    Ok(())
}
