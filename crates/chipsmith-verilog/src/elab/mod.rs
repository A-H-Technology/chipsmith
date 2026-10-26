//! Elaboration: from parsed modules to the netlist, at the level yosys
//! reaches after `hierarchy; proc`.
//!
//! One [`ModElab`] builds one module for one set of parameter values.
//! Names resolve through a tree of [`Scope`]s (the module, its generate
//! blocks, named blocks, function calls). Expressions are typed by IEEE
//! 1364-2005 §5.4/5.5 and lowered to RTLIL cells ([`expr`]); procedural
//! code runs symbolically in an [`stmt::Env`] that builds the
//! multiplexers, and the `always` and `initial` blocks become registers,
//! latches, memories and `init` attributes ([`proc`]).

mod expr;
mod items;
mod proc;
mod stmt;

use std::rc::Rc;

use chipsmith_netlist::{Attributes, Design, Dir, State};
use rustc_hash::FxHashMap;

use crate::ast::{self, Role};
use crate::source::{Diag, Result, SourceMap, Span};

pub(crate) use items::ModElab;
pub(crate) use stmt::Env;

/// The width and signedness of an expression or declaration.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Ty {
    pub width: usize,
    pub signed: bool,
}

impl Ty {
    pub const BIT: Ty = Ty {
        width: 1,
        signed: false,
    };
    pub const INTEGER: Ty = Ty {
        width: 32,
        signed: true,
    };
}

/// A constant: a parameter's value, a genvar, a folded expression.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) struct ConstVal {
    /// LSB first.
    pub bits: Vec<State>,
    pub signed: bool,
    /// Written as a string literal: a blackbox cell gets it as a string
    /// parameter, as yosys passes `"datac"` on.
    pub string: bool,
}

impl ConstVal {
    pub fn ty(&self) -> Ty {
        Ty {
            width: self.bits.len(),
            signed: self.signed,
        }
    }

    pub fn from_i64(v: i64) -> ConstVal {
        ConstVal {
            bits: chipsmith_netlist::Const::from_int(v, 32).0,
            signed: true,
            string: false,
        }
    }

    /// The value as an integer, if defined and in range.
    pub fn as_i64(&self) -> Option<i64> {
        if self
            .bits
            .iter()
            .any(|b| !matches!(b, State::Zero | State::One))
        {
            return None;
        }
        // Bits above 64 must repeat the sign (or be 0 if unsigned).
        let sign = if self.signed {
            *self.bits.last()?
        } else {
            State::Zero
        };
        if self.bits.len() > 64 && self.bits[64..].iter().any(|&b| b != sign) {
            return None;
        }
        let low = self.bits.len().min(64);
        let mut v: i128 = (0..low)
            .filter(|&i| self.bits[i] == State::One)
            .fold(0, |v, i| v | 1 << i);
        if sign == State::One {
            v -= 1i128 << low;
        }
        i64::try_from(v).ok()
    }

    pub fn is_true(&self) -> Option<bool> {
        if self.bits.contains(&State::One) {
            Some(true)
        } else if self.bits.iter().all(|&b| b == State::Zero) {
            Some(false)
        } else {
            None
        }
    }

    /// The string a string-literal parameter holds.
    pub fn as_string(&self) -> String {
        let bytes: Vec<u8> = self
            .bits
            .chunks(8)
            .rev()
            .map(|c| {
                c.iter()
                    .enumerate()
                    .filter(|(_, &b)| b == State::One)
                    .fold(0u8, |v, (i, _)| v | 1 << i)
            })
            .filter(|&b| b != 0)
            .collect();
        String::from_utf8_lossy(&bytes).into_owned()
    }
}

/// A declared packed range `[msb:lsb]`: which index is which bit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Dim {
    pub msb: i64,
    pub lsb: i64,
}

impl Dim {
    pub fn width(self) -> usize {
        (self.msb - self.lsb).unsigned_abs() as usize + 1
    }

    pub fn of_width(w: usize) -> Dim {
        Dim {
            msb: w as i64 - 1,
            lsb: 0,
        }
    }

    /// `[lo:hi]` rather than `[hi:lo]` (yosys' `upto`).
    pub fn upto(self) -> bool {
        self.msb < self.lsb
    }

    /// The lowest index (yosys' wire offset).
    pub fn offset(self) -> i64 {
        self.msb.min(self.lsb)
    }

    /// The bit position (0 = LSB) of an index, if in range.
    pub fn pos(self, index: i64) -> Option<usize> {
        let p = if self.upto() {
            self.lsb - index
        } else {
            index - self.lsb
        };
        (0..self.width() as i64).contains(&p).then_some(p as usize)
    }

    /// The position as an integer even when out of range.
    pub fn pos_unchecked(self, index: i64) -> i64 {
        if self.upto() {
            self.lsb - index
        } else {
            index - self.lsb
        }
    }
}

pub(crate) type VarId = usize;
pub(crate) type MemId = usize;
pub(crate) type LocalId = usize;
pub(crate) type ScopeId = usize;

/// A net or variable of the module: one wire in the netlist.
#[derive(Debug)]
pub(crate) struct Var {
    pub name: String,
    pub dim: Dim,
    pub signed: bool,
    pub nets: chipsmith_netlist::SigSpec,
    /// Declared `reg`/`integer`: may be assigned in procedures, not by
    /// continuous assignments.
    pub variable: bool,
    pub port: Option<Dir>,
    pub attrs: Attributes,
    /// Power-up value, from a declaration initializer or `initial`.
    pub init: Option<Vec<State>>,
    pub span: Span,
    /// Driven by a continuous assignment or an instance's output.
    pub cont_driven: bool,
}

impl Var {
    pub fn ty(&self) -> Ty {
        Ty {
            width: self.dim.width(),
            signed: self.signed,
        }
    }
}

/// A function's or task's argument or local variable, or a function's
/// result: lives in the [`Env`], not in the netlist.
#[derive(Debug, Clone)]
pub(crate) struct Local {
    pub dim: Dim,
    pub signed: bool,
}

impl Local {
    pub fn ty(&self) -> Ty {
        Ty {
            width: self.dim.width(),
            signed: self.signed,
        }
    }
}

/// A memory (`reg [w-1:0] m [0:n-1]`, or `m [0:3][0:7]`).
#[derive(Debug)]
pub(crate) struct MemVar {
    pub name: String,
    pub word: Dim,
    pub signed: bool,
    pub shape: MemShape,
    /// Lowest and highest word address.
    pub lo: i64,
    pub hi: i64,
    pub attrs: Attributes,
    pub span: Span,
    /// `size * width`, word `lo` first; x where not initialised.
    pub init: Vec<State>,
    pub storage: proc::MemStorage,
}

/// How a memory's indices make a word address.
#[derive(Debug, Clone)]
pub(crate) enum MemShape {
    /// One unpacked dimension: the index is the address.
    Flat,
    /// Two or more, outermost first: as yosys flattens them, one memory
    /// from address 0, each dimension counted from its left bound, the
    /// last varying fastest.
    Multi(Vec<Dim>),
}

impl MemShape {
    /// How many indices select a word.
    pub fn indices(&self) -> usize {
        match self {
            MemShape::Flat => 1,
            MemShape::Multi(dims) => dims.len(),
        }
    }
}

impl MemVar {
    pub fn size(&self) -> usize {
        (self.hi - self.lo + 1) as usize
    }

    pub fn width(&self) -> usize {
        self.word.width()
    }
}

#[derive(Debug, Clone)]
pub(crate) enum Sym<'a> {
    Var(VarId),
    Mem(MemId),
    Param(ConstVal),
    Local(LocalId),
    Func(&'a ast::Subroutine, ScopeId),
    Task(&'a ast::Subroutine, ScopeId),
}

/// A name space: the module, a generate block, a named block, a function
/// call.
#[derive(Debug)]
pub(crate) struct Scope<'a> {
    pub parent: Option<ScopeId>,
    /// What this scope's names are prefixed with in the netlist
    /// (`fold[3].`).
    pub prefix: String,
    pub names: FxHashMap<Rc<str>, (Sym<'a>, Span)>,
    /// Generate blocks inside, by name (`fold[3]`), for `fold[3].g`.
    pub blocks: FxHashMap<String, ScopeId>,
    /// Unnamed generate constructs so far, for `genblk<n>`.
    pub generate_count: u32,
}

/// A module's interface as an instance sees it.
#[derive(Debug, Clone)]
pub(crate) struct PortInfo {
    pub name: String,
    pub dir: Dir,
    pub width: usize,
}

#[derive(Debug, Clone)]
pub(crate) struct Elaborated {
    /// The module's name in the design (`$paramod...` if parameters were
    /// overridden).
    pub name: String,
    pub ports: Vec<PortInfo>,
}

/// The whole design being elaborated: the parsed modules, and the netlist
/// modules made so far, one per module and parameter set.
pub(crate) struct DesignCtx<'a> {
    pub sources: &'a SourceMap,
    pub asts: FxHashMap<&'a str, &'a ast::Module>,
    pub done: FxHashMap<(String, Vec<(String, ConstVal)>), Elaborated>,
    pub out: Vec<chipsmith_netlist::Module>,
    /// Modules being elaborated, to catch a module that contains itself.
    pub stack: Vec<String>,
}

pub(crate) fn elaborate(sources: &SourceMap, modules: &[ast::Module], top: &str) -> Result<Design> {
    let mut asts: FxHashMap<&str, &ast::Module> = FxHashMap::default();
    for m in modules {
        // A later definition replaces an earlier one, except that a design
        // module wins over a library's interface of the same name.
        match asts.get(&*m.name.name) {
            Some(prev) if prev.role == Role::Design && m.role == Role::Library => {}
            Some(prev) if prev.role == Role::Design => {
                return Err(Diag::new(
                    m.name.span,
                    format!(
                        "module {} is defined twice (first at {})",
                        m.name.name,
                        sources.location(prev.name.span)
                    ),
                ))
            }
            _ => {
                asts.insert(&m.name.name, m);
            }
        }
    }
    let Some(top_ast) = asts.get(top).copied() else {
        return Err(Diag::global(format!("top module {top} not found")));
    };
    if top_ast.is_blackbox() {
        return Err(Diag::new(
            top_ast.name.span,
            format!("top module {top} is a blackbox"),
        ));
    }
    let mut ctx = DesignCtx {
        sources,
        asts,
        done: FxHashMap::default(),
        out: Vec::new(),
        stack: Vec::new(),
    };
    let e = ctx.module(top_ast, Vec::new(), top_ast.name.span)?;
    let mut design = Design { modules: ctx.out };
    if let Some(m) = design.module_mut(&e.name) {
        m.attributes.insert(
            chipsmith_netlist::attr::TOP.into(),
            chipsmith_netlist::ParamValue::int(1),
        );
    }
    Ok(design)
}

impl<'a> DesignCtx<'a> {
    /// The netlist module for `ast` with these parameter overrides (by
    /// name, already evaluated), elaborating it the first time.
    pub fn module(
        &mut self,
        ast: &'a ast::Module,
        overrides: Vec<(String, ConstVal)>,
        site: Span,
    ) -> Result<Elaborated> {
        let key = (ast.name.name.to_string(), overrides);
        if let Some(e) = self.done.get(&key) {
            return Ok(e.clone());
        }
        if self.stack.contains(&key.0) {
            return Err(Diag::new(
                site,
                format!("module {} instantiates itself", key.0),
            ));
        }
        self.stack.push(key.0.clone());
        let name = if key.1.is_empty() || ast.is_blackbox() {
            key.0.clone()
        } else {
            paramod_name(&key.0, &key.1)
        };
        let result = items::elaborate_module(self, ast, &name, &key.1);
        self.stack.pop();
        let (module, ports) = result?;
        let e = Elaborated {
            name: name.clone(),
            ports,
        };
        // A blackbox is kept once, as yosys keeps a `-lib` module: its
        // instances carry their parameters.
        if !(ast.is_blackbox() && self.out.iter().any(|m| m.name == name)) {
            self.out.push(module);
        }
        self.done.insert(key, e.clone());
        Ok(e)
    }
}

/// yosys' name for a module with overridden parameters:
/// `$paramod\name\P=32'00000101`.
fn paramod_name(name: &str, params: &[(String, ConstVal)]) -> String {
    let mut s = format!("$paramod\\{name}");
    for (p, v) in params {
        s.push('\\');
        s.push_str(p);
        s.push('=');
        if v.string {
            s.push_str(&v.as_string());
        } else {
            if v.signed {
                s.push('s');
            }
            s.push_str(&format!(
                "{}'{}",
                v.bits.len(),
                chipsmith_netlist::Const(v.bits.clone())
            ));
        }
    }
    s
}
