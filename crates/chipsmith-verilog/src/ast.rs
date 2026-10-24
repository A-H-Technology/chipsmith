//! The parse tree: Verilog as written, before any name is resolved or
//! parameter evaluated. Every node keeps its [`Span`] for errors and
//! `src` attributes.

use std::rc::Rc;

use chipsmith_netlist::State;

use crate::pp::DefaultNettype;
use crate::source::Span;

#[derive(Debug, Clone)]
pub struct Ident {
    pub name: Rc<str>,
    pub span: Span,
}

/// `(* name = value *)`; no value means 1.
#[derive(Debug, Clone)]
pub struct Attr {
    pub name: Ident,
    pub value: Option<Expr>,
}

pub type Attrs = Vec<Attr>;

#[derive(Debug)]
pub struct Module {
    pub name: Ident,
    pub attrs: Attrs,
    /// Port names in header order.
    pub ports: Vec<Ident>,
    pub items: Vec<Item>,
    pub nettype: DefaultNettype,
    /// From a blackbox library (`read_verilog -lib`): only the interface
    /// counts.
    pub lib: bool,
    pub span: Span,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PortDir {
    Input,
    Output,
    Inout,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NetKind {
    Wire,
    Tri,
    Reg,
    Integer,
    Time,
    Supply0,
    Supply1,
}

impl NetKind {
    /// A variable (assigned in procedures) rather than a net.
    pub fn is_variable(self) -> bool {
        matches!(self, NetKind::Reg | NetKind::Integer | NetKind::Time)
    }
}

/// `[msb:lsb]`
#[derive(Debug, Clone)]
pub struct Range {
    pub msb: Expr,
    pub lsb: Expr,
    pub span: Span,
}

/// The type part of a declaration: `reg signed [7:0]`. `kind` is `None` on
/// a port declaration that doesn't say (`input [3:0] a`).
#[derive(Debug, Clone)]
pub struct DataType {
    pub kind: Option<NetKind>,
    pub signed: bool,
    pub range: Option<Range>,
}

#[derive(Debug, Clone)]
pub struct Declarator {
    pub name: Ident,
    /// Unpacked dimensions: `mem [0:255]`.
    pub dims: Vec<Range>,
    pub init: Option<Expr>,
}

#[derive(Debug, Clone)]
pub struct NetDecl {
    pub attrs: Attrs,
    /// Set on a port declaration (`input`, `output reg`, ...).
    pub dir: Option<PortDir>,
    pub ty: DataType,
    pub names: Vec<Declarator>,
    pub span: Span,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ParamKind {
    /// `parameter [range] P` or plain `parameter P`.
    Plain,
    Integer,
    Real,
}

#[derive(Debug, Clone)]
pub struct ParamDecl {
    pub attrs: Attrs,
    pub local: bool,
    /// In the module's `#(...)` list: with one, only those can be
    /// overridden (IEEE 1364-2005 §12.2).
    pub header: bool,
    pub kind: ParamKind,
    pub signed: bool,
    pub range: Option<Range>,
    pub assigns: Vec<(Ident, Expr)>,
    pub span: Span,
}

#[derive(Debug, Clone)]
pub enum ParamOverride {
    Named(Ident, Option<Expr>),
    Positional(Expr),
}

#[derive(Debug, Clone)]
pub struct Connection {
    /// `None` for a positional connection.
    pub port: Option<Ident>,
    /// `None` for `.p()` or an empty positional slot: left open.
    pub expr: Option<Expr>,
    pub span: Span,
}

#[derive(Debug, Clone)]
pub struct Instance {
    pub name: Ident,
    pub conns: Vec<Connection>,
    pub span: Span,
}

#[derive(Debug, Clone)]
pub struct InstanceItem {
    pub attrs: Attrs,
    pub module: Ident,
    pub params: Vec<ParamOverride>,
    pub instances: Vec<Instance>,
    pub span: Span,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GateKind {
    And,
    Nand,
    Or,
    Nor,
    Xor,
    Xnor,
    Buf,
    Not,
    Bufif0,
    Bufif1,
}

#[derive(Debug, Clone)]
pub struct GateInst {
    pub attrs: Attrs,
    pub kind: GateKind,
    pub name: Option<Ident>,
    pub terminals: Vec<Expr>,
    pub span: Span,
}

/// A function or task: its arguments are the declarations with a
/// direction, in order.
#[derive(Debug)]
pub struct Subroutine {
    pub name: Ident,
    pub attrs: Attrs,
    /// A function's return type (`None` for a task).
    pub ret: Option<DataType>,
    pub decls: Vec<NetDecl>,
    pub params: Vec<ParamDecl>,
    pub body: Stmt,
    pub span: Span,
}

#[derive(Debug, Clone)]
pub struct GenBlock {
    /// `begin : name`; unnamed blocks are `genblk<n>`, IEEE 1364-2005
    /// §12.4.3.
    pub name: Option<Ident>,
    pub items: Vec<Item>,
    pub span: Span,
}

#[derive(Debug, Clone)]
pub struct GenCaseItem {
    /// Empty for `default`.
    pub exprs: Vec<Expr>,
    pub body: GenBlock,
}

#[derive(Debug, Clone)]
pub enum Item {
    Net(NetDecl),
    Param(ParamDecl),
    Genvar(Vec<Ident>),
    Assign {
        attrs: Attrs,
        lhs: Expr,
        rhs: Expr,
        span: Span,
    },
    Always {
        attrs: Attrs,
        body: Stmt,
        span: Span,
    },
    Initial {
        attrs: Attrs,
        body: Stmt,
        span: Span,
    },
    Instance(InstanceItem),
    Gate(GateInst),
    Function(Rc<Subroutine>),
    Task(Rc<Subroutine>),
    Defparam(Vec<(Vec<Ident>, Expr)>),
    GenFor {
        genvar: Ident,
        init: Expr,
        cond: Expr,
        step_var: Ident,
        step: Expr,
        body: GenBlock,
        span: Span,
    },
    GenIf {
        cond: Expr,
        then: GenBlock,
        els: Option<GenBlock>,
        span: Span,
    },
    GenCase {
        expr: Expr,
        items: Vec<GenCaseItem>,
        span: Span,
    },
    /// A bare `begin ... end` in a generate region.
    GenBlock(GenBlock),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Edge {
    Pos,
    Neg,
}

#[derive(Debug, Clone)]
pub struct EventItem {
    pub edge: Option<Edge>,
    pub expr: Expr,
}

#[derive(Debug, Clone)]
pub enum Event {
    /// `@*`, `@(*)`
    Star,
    List(Vec<EventItem>),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CaseKind {
    Case,
    Casez,
    Casex,
}

#[derive(Debug, Clone)]
pub struct CaseItem {
    /// Empty for `default`.
    pub exprs: Vec<Expr>,
    pub body: Stmt,
}

#[derive(Debug, Clone)]
pub struct Stmt {
    pub kind: StmtKind,
    pub attrs: Attrs,
    pub span: Span,
}

#[derive(Debug, Clone)]
pub enum StmtKind {
    Null,
    Block {
        name: Option<Ident>,
        decls: Vec<NetDecl>,
        params: Vec<ParamDecl>,
        stmts: Vec<Stmt>,
    },
    Assign {
        lhs: Expr,
        rhs: Expr,
        blocking: bool,
    },
    If {
        cond: Expr,
        then: Box<Stmt>,
        els: Option<Box<Stmt>>,
    },
    Case {
        kind: CaseKind,
        expr: Expr,
        items: Vec<CaseItem>,
    },
    For {
        init: Box<Stmt>,
        cond: Expr,
        step: Box<Stmt>,
        body: Box<Stmt>,
    },
    While {
        cond: Expr,
        body: Box<Stmt>,
    },
    Repeat {
        count: Expr,
        body: Box<Stmt>,
    },
    /// `@(...) stmt`: only at the top of an `always`.
    Event {
        event: Event,
        body: Box<Stmt>,
    },
    TaskCall {
        name: Ident,
        args: Vec<Expr>,
    },
    SysTaskCall {
        name: Ident,
        args: Vec<Expr>,
    },
    /// Something parsed only to say it isn't supported, where it is.
    Unsupported(&'static str),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnOp {
    Plus,
    Minus,
    LogicNot,
    BitNot,
    RedAnd,
    RedNand,
    RedOr,
    RedNor,
    RedXor,
    RedXnor,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BinOp {
    Add,
    Sub,
    Mul,
    Div,
    Mod,
    Pow,
    Shl,
    Shr,
    AShl,
    AShr,
    Lt,
    Le,
    Gt,
    Ge,
    Eq,
    Ne,
    CaseEq,
    CaseNe,
    And,
    Xor,
    Xnor,
    Or,
    LogicAnd,
    LogicOr,
}

/// A literal number, already in bits.
#[derive(Debug, Clone, PartialEq)]
pub struct Number {
    /// LSB first, at its full width (32 for an unsized constant, or more
    /// if its digits need it).
    pub bits: Vec<State>,
    pub signed: bool,
    pub sized: bool,
}

#[derive(Debug, Clone)]
pub struct Expr {
    pub kind: ExprKind,
    pub span: Span,
}

#[derive(Debug, Clone)]
pub enum ExprKind {
    Ident(Ident),
    /// `a.b[1].c`: a name inside generate blocks (or another instance,
    /// which isn't supported).
    Hier(Vec<(Ident, Option<Expr>)>),
    Number(Number),
    /// `'0`, `'1`, `'x`, `'z`
    Fill(State),
    Real(f64),
    Str(Rc<[u8]>),
    Index {
        base: Box<Expr>,
        index: Box<Expr>,
    },
    Slice {
        base: Box<Expr>,
        msb: Box<Expr>,
        lsb: Box<Expr>,
    },
    /// `[start +: width]` (`up`) or `[start -: width]`.
    IndexedSlice {
        base: Box<Expr>,
        start: Box<Expr>,
        width: Box<Expr>,
        up: bool,
    },
    Unary {
        op: UnOp,
        arg: Box<Expr>,
    },
    Binary {
        op: BinOp,
        lhs: Box<Expr>,
        rhs: Box<Expr>,
    },
    Cond {
        cond: Box<Expr>,
        then: Box<Expr>,
        els: Box<Expr>,
    },
    Concat(Vec<Expr>),
    Repeat {
        count: Box<Expr>,
        parts: Vec<Expr>,
    },
    Call {
        name: Ident,
        args: Vec<Expr>,
    },
    SysCall {
        name: Ident,
        args: Vec<Expr>,
    },
}
