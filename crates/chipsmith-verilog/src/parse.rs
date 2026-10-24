//! Recursive-descent parser from preprocessed tokens to [`ast`] modules,
//! for the Verilog-2005 grammar (IEEE 1364-2005 Annex A) as far as
//! synthesis goes. Constructs with no hardware meaning (delays, strengths)
//! are parsed and dropped; ones chipsmith doesn't elaborate either fail
//! here with their position or, where they can sit in dead code, become
//! [`StmtKind::Unsupported`].

use std::rc::Rc;

use chipsmith_netlist::State;

use crate::ast::*;
use crate::lex::{Base, Kw, Tok, Token, P};
use crate::pp::{DefaultNettype, Tokens};
use crate::source::{bail, Diag, Result, Span};

pub struct Parser<'a> {
    toks: &'a [Token],
    nettype: &'a [(usize, DefaultNettype)],
    pos: usize,
    lib: bool,
}

pub fn parse(tokens: &Tokens, lib: bool) -> Result<Vec<Module>> {
    let mut p = Parser {
        toks: &tokens.tokens,
        nettype: &tokens.nettype,
        pos: 0,
        lib,
    };
    let mut modules = Vec::new();
    loop {
        let attrs = p.attrs()?;
        match p.tok() {
            Tok::Eof => return Ok(modules),
            Tok::Kw(Kw::Module | Kw::Macromodule) => modules.push(p.module(attrs)?),
            Tok::Kw(Kw::Primitive) => bail!(p.span(), "user-defined primitives are not supported"),
            Tok::Kw(Kw::Config | Kw::Library) => {
                bail!(p.span(), "configurations and libraries are not supported")
            }
            _ => return Err(p.expected("`module`")),
        }
    }
}

impl<'a> Parser<'a> {
    fn tok(&self) -> &'a Tok {
        &self.toks[self.pos].tok
    }

    fn peek_tok(&self, k: usize) -> &'a Tok {
        let i = (self.pos + k).min(self.toks.len() - 1);
        &self.toks[i].tok
    }

    fn span(&self) -> Span {
        self.toks[self.pos].span
    }

    fn prev_span(&self) -> Span {
        self.toks[self.pos.saturating_sub(1)].span
    }

    fn bump(&mut self) -> &'a Token {
        let t = &self.toks[self.pos];
        if self.pos + 1 < self.toks.len() {
            self.pos += 1;
        }
        t
    }

    fn is(&self, p: P) -> bool {
        *self.tok() == Tok::P(p)
    }

    fn is_kw(&self, k: Kw) -> bool {
        *self.tok() == Tok::Kw(k)
    }

    fn eat(&mut self, p: P) -> bool {
        if self.is(p) {
            self.bump();
            true
        } else {
            false
        }
    }

    fn eat_kw(&mut self, k: Kw) -> bool {
        if self.is_kw(k) {
            self.bump();
            true
        } else {
            false
        }
    }

    fn expected(&self, what: &str) -> Diag {
        Diag::new(
            self.span(),
            format!("expected {what}, found {}", self.tok().describe()),
        )
    }

    fn expect(&mut self, p: P) -> Result<Span> {
        if self.is(p) {
            Ok(self.bump().span)
        } else {
            Err(self.expected(&format!("`{}`", p.text())))
        }
    }

    fn expect_kw(&mut self, k: Kw) -> Result<Span> {
        if self.is_kw(k) {
            Ok(self.bump().span)
        } else {
            Err(self.expected(&format!("`{}`", k.name())))
        }
    }

    fn ident(&mut self) -> Result<Ident> {
        match self.tok() {
            Tok::Ident(name) => {
                let span = self.bump().span;
                Ok(Ident {
                    name: name.clone(),
                    span,
                })
            }
            _ => Err(self.expected("an identifier")),
        }
    }

    fn nettype_at(&self, pos: usize) -> DefaultNettype {
        self.nettype
            .iter()
            .take_while(|(at, _)| *at <= pos)
            .last()
            .map_or(DefaultNettype::Wire, |&(_, k)| k)
    }

    /// `(* a, b = 1 *)`, any number of them.
    fn attrs(&mut self) -> Result<Attrs> {
        let mut attrs = Vec::new();
        while self.eat(P::AttrOpen) {
            loop {
                let name = match self.tok() {
                    Tok::Kw(k) => {
                        let span = self.bump().span;
                        Ident {
                            name: k.name().into(),
                            span,
                        }
                    }
                    _ => self.ident()?,
                };
                let value = if self.eat(P::Eq) {
                    Some(self.expr()?)
                } else {
                    None
                };
                attrs.push(Attr { name, value });
                if !self.eat(P::Comma) {
                    break;
                }
            }
            self.expect(P::AttrClose)?;
        }
        Ok(attrs)
    }

    // ---------------------------------------------------------------
    // Modules

    fn module(&mut self, attrs: Attrs) -> Result<Module> {
        let start = self.bump().span;
        let nettype = self.nettype_at(self.pos);
        let name = self.ident()?;
        let mut items = Vec::new();
        if self.eat(P::Hash) {
            self.expect(P::LParen)?;
            if !self.is(P::RParen) {
                let mut kind = None;
                loop {
                    let attrs = self.attrs()?;
                    let lo = self.span();
                    let local = self.is_kw(Kw::Localparam);
                    if self.eat_kw(Kw::Parameter) || self.eat_kw(Kw::Localparam) {
                        kind = Some(self.param_type(local)?);
                    }
                    let Some((local, k, signed, range)) = kind.clone() else {
                        return Err(self.expected("`parameter`"));
                    };
                    let id = self.ident()?;
                    self.expect(P::Eq)?;
                    let value = self.expr()?;
                    items.push(Item::Param(ParamDecl {
                        attrs,
                        local,
                        header: true,
                        kind: k,
                        signed,
                        range,
                        assigns: vec![(id, value)],
                        span: lo.to(self.prev_span()),
                    }));
                    if !self.eat(P::Comma) {
                        break;
                    }
                }
            }
            self.expect(P::RParen)?;
        }
        let mut ports = Vec::new();
        if self.eat(P::LParen) {
            let ansi = {
                let mut k = 0;
                while *self.peek_tok(k) == Tok::P(P::AttrOpen) {
                    while *self.peek_tok(k) != Tok::P(P::AttrClose) && *self.peek_tok(k) != Tok::Eof
                    {
                        k += 1;
                    }
                    k += 1;
                }
                matches!(
                    self.peek_tok(k),
                    Tok::Kw(Kw::Input | Kw::Output | Kw::Inout)
                )
            };
            if ansi {
                self.ansi_ports(&mut ports, &mut items)?;
            } else if !self.is(P::RParen) {
                loop {
                    if self.is(P::Dot) || self.is(P::LBrace) {
                        bail!(self.span(), "port expressions in a module header are not supported");
                    }
                    ports.push(self.ident()?);
                    if self.is(P::LBrack) {
                        bail!(self.span(), "port expressions in a module header are not supported");
                    }
                    if !self.eat(P::Comma) {
                        break;
                    }
                }
            }
            self.expect(P::RParen)?;
        }
        self.expect(P::Semi)?;
        while !self.is_kw(Kw::Endmodule) {
            if *self.tok() == Tok::Eof {
                bail!(start, "module {} has no `endmodule`", name.name);
            }
            self.module_item(&mut items)?;
        }
        let end = self.bump().span;
        if self.eat(P::Colon) {
            self.ident()?;
        }
        Ok(Module {
            name,
            attrs,
            ports,
            items,
            nettype,
            lib: self.lib,
            span: start.to(end),
        })
    }

    #[allow(clippy::type_complexity)]
    fn param_type(&mut self, local: bool) -> Result<(bool, ParamKind, bool, Option<Range>)> {
        let mut kind = ParamKind::Plain;
        if self.eat_kw(Kw::Integer) {
            kind = ParamKind::Integer;
        } else if self.eat_kw(Kw::Real) || self.eat_kw(Kw::Realtime) {
            kind = ParamKind::Real;
        } else if self.eat_kw(Kw::Time) {
            kind = ParamKind::Integer;
        }
        let signed = self.eat_kw(Kw::Signed);
        let range = if self.is(P::LBrack) {
            Some(self.range()?)
        } else {
            None
        };
        Ok((local, kind, signed, range))
    }

    fn ansi_ports(&mut self, ports: &mut Vec<Ident>, items: &mut Vec<Item>) -> Result<()> {
        let mut current: Option<(PortDir, DataType)> = None;
        loop {
            let attrs = self.attrs()?;
            let lo = self.span();
            let dir = match self.tok() {
                Tok::Kw(Kw::Input) => Some(PortDir::Input),
                Tok::Kw(Kw::Output) => Some(PortDir::Output),
                Tok::Kw(Kw::Inout) => Some(PortDir::Inout),
                _ => None,
            };
            if let Some(dir) = dir {
                self.bump();
                let ty = self.data_type(true)?;
                current = Some((dir, ty));
            } else if self.is(P::Dot) {
                bail!(self.span(), "port expressions in a module header are not supported");
            }
            let Some((dir, ty)) = current.clone() else {
                return Err(self.expected("a port direction"));
            };
            let name = self.ident()?;
            let mut dims = Vec::new();
            while self.is(P::LBrack) {
                dims.push(self.range()?);
            }
            let init = if self.eat(P::Eq) {
                Some(self.expr()?)
            } else {
                None
            };
            ports.push(name.clone());
            items.push(Item::Net(NetDecl {
                attrs,
                dir: Some(dir),
                ty,
                names: vec![Declarator { name, dims, init }],
                span: lo.to(self.prev_span()),
            }));
            if !self.eat(P::Comma) {
                return Ok(());
            }
        }
    }

    /// `[wire|reg|integer|...] [signed] [range]`. On a port the kind may be
    /// left out.
    fn data_type(&mut self, port: bool) -> Result<DataType> {
        let kind = match self.tok() {
            Tok::Kw(Kw::Wire) | Tok::Kw(Kw::Uwire) => Some(NetKind::Wire),
            Tok::Kw(Kw::Tri) => Some(NetKind::Tri),
            Tok::Kw(Kw::Reg) => Some(NetKind::Reg),
            Tok::Kw(Kw::Integer) => Some(NetKind::Integer),
            Tok::Kw(Kw::Time) => Some(NetKind::Time),
            Tok::Kw(Kw::Supply0) => Some(NetKind::Supply0),
            Tok::Kw(Kw::Supply1) => Some(NetKind::Supply1),
            Tok::Kw(k @ (Kw::Wand | Kw::Wor | Kw::Triand | Kw::Trior | Kw::Tri0 | Kw::Tri1 | Kw::Trireg)) => {
                bail!(self.span(), "`{}` nets are not supported", k.name())
            }
            Tok::Kw(Kw::Real | Kw::Realtime) => {
                bail!(self.span(), "real variables are not supported")
            }
            _ if port => None,
            _ => return Err(self.expected("a net or variable type")),
        };
        if kind.is_some() {
            self.bump();
        }
        if self.is_kw(Kw::Vectored) || self.is_kw(Kw::Scalared) {
            self.bump();
        }
        let signed = self.eat_kw(Kw::Signed);
        if self.is_kw(Kw::Unsigned) {
            self.bump();
        }
        let range = if self.is(P::LBrack) && !matches!(kind, Some(NetKind::Integer | NetKind::Time)) {
            Some(self.range()?)
        } else {
            None
        };
        Ok(DataType {
            kind,
            signed,
            range,
        })
    }

    fn range(&mut self) -> Result<Range> {
        let lo = self.expect(P::LBrack)?;
        let msb = self.expr()?;
        self.expect(P::Colon)?;
        let lsb = self.expr()?;
        let hi = self.expect(P::RBrack)?;
        Ok(Range {
            msb,
            lsb,
            span: lo.to(hi),
        })
    }

    fn declarators(&mut self, allow_init: bool) -> Result<Vec<Declarator>> {
        let mut names = Vec::new();
        loop {
            let name = self.ident()?;
            let mut dims = Vec::new();
            while self.is(P::LBrack) {
                dims.push(self.range()?);
            }
            let init = if allow_init && self.eat(P::Eq) {
                Some(self.expr()?)
            } else {
                None
            };
            names.push(Declarator { name, dims, init });
            if !self.eat(P::Comma) {
                return Ok(names);
            }
        }
    }

    /// Skips `#delay` and drive strengths `(strong0, weak1)` where a
    /// declaration or assignment may have them.
    fn skip_delay_and_strength(&mut self) -> Result<()> {
        if self.is(P::LParen)
            && matches!(
                self.peek_tok(1),
                Tok::Kw(
                    Kw::Strong0
                        | Kw::Strong1
                        | Kw::Weak0
                        | Kw::Weak1
                        | Kw::Pull0
                        | Kw::Pull1
                        | Kw::Highz0
                        | Kw::Highz1
                        | Kw::Supply0
                        | Kw::Supply1
                )
            )
        {
            while !self.eat(P::RParen) {
                self.bump();
            }
        }
        self.skip_delay()
    }

    fn skip_delay(&mut self) -> Result<()> {
        if self.eat(P::Hash) {
            if self.eat(P::LParen) {
                let mut depth = 1;
                while depth > 0 {
                    match self.bump().tok {
                        Tok::P(P::LParen) => depth += 1,
                        Tok::P(P::RParen) => depth -= 1,
                        Tok::Eof => bail!(self.span(), "unterminated delay"),
                        _ => {}
                    }
                }
            } else {
                self.bump();
            }
        }
        Ok(())
    }

    fn net_decl(&mut self, attrs: Attrs, dir: Option<PortDir>) -> Result<NetDecl> {
        let lo = self.span();
        let ty = self.data_type(dir.is_some())?;
        self.skip_delay_and_strength()?;
        let names = self.declarators(true)?;
        self.expect(P::Semi)?;
        Ok(NetDecl {
            attrs,
            dir,
            ty,
            names,
            span: lo.to(self.prev_span()),
        })
    }

    fn param_decl(&mut self, attrs: Attrs) -> Result<ParamDecl> {
        let lo = self.span();
        let local = self.is_kw(Kw::Localparam);
        self.bump();
        let (local, kind, signed, range) = self.param_type(local)?;
        let mut assigns = Vec::new();
        loop {
            let id = self.ident()?;
            self.expect(P::Eq)?;
            assigns.push((id, self.expr()?));
            if !self.eat(P::Comma) {
                break;
            }
        }
        self.expect(P::Semi)?;
        Ok(ParamDecl {
            attrs,
            local,
            header: false,
            kind,
            signed,
            range,
            assigns,
            span: lo.to(self.prev_span()),
        })
    }

    fn module_item(&mut self, items: &mut Vec<Item>) -> Result<()> {
        let attrs = self.attrs()?;
        let lo = self.span();
        match self.tok() {
            Tok::P(P::Semi) => {
                self.bump();
            }
            Tok::Kw(Kw::Input) | Tok::Kw(Kw::Output) | Tok::Kw(Kw::Inout) => {
                let dir = match self.bump().tok {
                    Tok::Kw(Kw::Input) => PortDir::Input,
                    Tok::Kw(Kw::Output) => PortDir::Output,
                    _ => PortDir::Inout,
                };
                items.push(Item::Net(self.net_decl(attrs, Some(dir))?));
            }
            Tok::Kw(
                Kw::Wire
                | Kw::Uwire
                | Kw::Tri
                | Kw::Reg
                | Kw::Integer
                | Kw::Time
                | Kw::Supply0
                | Kw::Supply1
                | Kw::Wand
                | Kw::Wor
                | Kw::Triand
                | Kw::Trior
                | Kw::Tri0
                | Kw::Tri1
                | Kw::Trireg
                | Kw::Real
                | Kw::Realtime,
            ) => items.push(Item::Net(self.net_decl(attrs, None)?)),
            Tok::Kw(Kw::Parameter | Kw::Localparam) => {
                items.push(Item::Param(self.param_decl(attrs)?))
            }
            Tok::Kw(Kw::Specparam) => bail!(lo, "specparam is not supported"),
            Tok::Kw(Kw::Genvar) => {
                self.bump();
                let mut names = vec![self.ident()?];
                while self.eat(P::Comma) {
                    names.push(self.ident()?);
                }
                self.expect(P::Semi)?;
                items.push(Item::Genvar(names));
            }
            Tok::Kw(Kw::Defparam) => {
                self.bump();
                let mut assigns = Vec::new();
                loop {
                    let mut path = vec![self.ident()?];
                    while self.eat(P::Dot) {
                        path.push(self.ident()?);
                    }
                    self.expect(P::Eq)?;
                    assigns.push((path, self.expr()?));
                    if !self.eat(P::Comma) {
                        break;
                    }
                }
                self.expect(P::Semi)?;
                items.push(Item::Defparam(assigns));
            }
            Tok::Kw(Kw::Assign) => {
                self.bump();
                self.skip_delay_and_strength()?;
                loop {
                    let alo = self.span();
                    let lhs = self.lvalue()?;
                    self.expect(P::Eq)?;
                    let rhs = self.expr()?;
                    items.push(Item::Assign {
                        attrs: attrs.clone(),
                        lhs,
                        rhs,
                        span: alo.to(self.prev_span()),
                    });
                    if !self.eat(P::Comma) {
                        break;
                    }
                }
                self.expect(P::Semi)?;
            }
            Tok::Kw(Kw::Always) => {
                self.bump();
                let body = self.stmt()?;
                items.push(Item::Always {
                    attrs,
                    body,
                    span: lo.to(self.prev_span()),
                });
            }
            Tok::Kw(Kw::Initial) => {
                self.bump();
                let body = self.stmt()?;
                items.push(Item::Initial {
                    attrs,
                    body,
                    span: lo.to(self.prev_span()),
                });
            }
            Tok::Kw(Kw::Function) => items.push(Item::Function(Rc::new(self.subroutine(attrs, true)?))),
            Tok::Kw(Kw::Task) => items.push(Item::Task(Rc::new(self.subroutine(attrs, false)?))),
            Tok::Kw(Kw::Generate) => {
                self.bump();
                while !self.eat_kw(Kw::Endgenerate) {
                    if *self.tok() == Tok::Eof {
                        bail!(lo, "`generate` without `endgenerate`");
                    }
                    self.module_item(items)?;
                }
            }
            Tok::Kw(Kw::For) => items.push(self.gen_for()?),
            Tok::Kw(Kw::If) => items.push(self.gen_if()?),
            Tok::Kw(Kw::Case) => items.push(self.gen_case()?),
            Tok::Kw(Kw::Begin) => items.push(Item::GenBlock(self.gen_block()?)),
            Tok::Kw(
                k @ (Kw::And
                | Kw::Nand
                | Kw::Or
                | Kw::Nor
                | Kw::Xor
                | Kw::Xnor
                | Kw::Buf
                | Kw::Not
                | Kw::Bufif0
                | Kw::Bufif1),
            ) => {
                let kind = match k {
                    Kw::And => GateKind::And,
                    Kw::Nand => GateKind::Nand,
                    Kw::Or => GateKind::Or,
                    Kw::Nor => GateKind::Nor,
                    Kw::Xor => GateKind::Xor,
                    Kw::Xnor => GateKind::Xnor,
                    Kw::Buf => GateKind::Buf,
                    Kw::Not => GateKind::Not,
                    Kw::Bufif0 => GateKind::Bufif0,
                    _ => GateKind::Bufif1,
                };
                self.bump();
                self.skip_delay_and_strength()?;
                loop {
                    let glo = self.span();
                    let name = if let Tok::Ident(_) = self.tok() {
                        Some(self.ident()?)
                    } else {
                        None
                    };
                    if self.is(P::LBrack) {
                        bail!(self.span(), "arrays of gate instances are not supported");
                    }
                    self.expect(P::LParen)?;
                    let mut terminals = vec![self.expr()?];
                    while self.eat(P::Comma) {
                        terminals.push(self.expr()?);
                    }
                    self.expect(P::RParen)?;
                    items.push(Item::Gate(GateInst {
                        attrs: attrs.clone(),
                        kind,
                        name,
                        terminals,
                        span: glo.to(self.prev_span()),
                    }));
                    if !self.eat(P::Comma) {
                        break;
                    }
                }
                self.expect(P::Semi)?;
            }
            Tok::Kw(k @ (Kw::Specify | Kw::Pullup | Kw::Pulldown | Kw::Event | Kw::Nmos | Kw::Pmos | Kw::Cmos | Kw::Tran | Kw::Notif0 | Kw::Notif1)) => {
                bail!(lo, "`{}` is not supported", k.name())
            }
            Tok::Ident(_) => items.push(Item::Instance(self.instance(attrs)?)),
            _ => return Err(self.expected("a module item")),
        }
        Ok(())
    }

    fn instance(&mut self, attrs: Attrs) -> Result<InstanceItem> {
        let lo = self.span();
        let module = self.ident()?;
        let mut params = Vec::new();
        if self.eat(P::Hash) {
            if self.eat(P::LParen) {
                if !self.is(P::RParen) {
                    loop {
                        if self.eat(P::Dot) {
                            let name = self.ident()?;
                            self.expect(P::LParen)?;
                            let e = if self.is(P::RParen) {
                                None
                            } else {
                                Some(self.expr()?)
                            };
                            self.expect(P::RParen)?;
                            params.push(ParamOverride::Named(name, e));
                        } else {
                            params.push(ParamOverride::Positional(self.expr()?));
                        }
                        if !self.eat(P::Comma) {
                            break;
                        }
                    }
                }
                self.expect(P::RParen)?;
            } else {
                params.push(ParamOverride::Positional(self.primary()?));
            }
        }
        let mut instances = Vec::new();
        loop {
            let ilo = self.span();
            let name = self.ident()?;
            if self.is(P::LBrack) {
                bail!(self.span(), "arrays of instances are not supported");
            }
            self.expect(P::LParen)?;
            let mut conns = Vec::new();
            if !self.is(P::RParen) {
                loop {
                    let clo = self.span();
                    self.attrs()?;
                    if self.eat(P::Dot) {
                        if self.is(P::Star) {
                            bail!(self.span(), "`.*` connections are not supported");
                        }
                        let port = self.ident()?;
                        let expr = if self.eat(P::LParen) {
                            let e = if self.is(P::RParen) {
                                None
                            } else {
                                Some(self.expr()?)
                            };
                            self.expect(P::RParen)?;
                            e
                        } else {
                            bail!(port.span, "implicit `.{}` connections are not supported", port.name);
                        };
                        conns.push(Connection {
                            port: Some(port),
                            expr,
                            span: clo.to(self.prev_span()),
                        });
                    } else {
                        let expr = if self.is(P::Comma) || self.is(P::RParen) {
                            None
                        } else {
                            Some(self.expr()?)
                        };
                        conns.push(Connection {
                            port: None,
                            expr,
                            span: clo.to(self.prev_span()),
                        });
                    }
                    if !self.eat(P::Comma) {
                        break;
                    }
                }
            }
            self.expect(P::RParen)?;
            instances.push(Instance {
                name,
                conns,
                span: ilo.to(self.prev_span()),
            });
            if !self.eat(P::Comma) {
                break;
            }
        }
        self.expect(P::Semi)?;
        Ok(InstanceItem {
            attrs,
            module,
            params,
            instances,
            span: lo.to(self.prev_span()),
        })
    }

    fn subroutine(&mut self, attrs: Attrs, function: bool) -> Result<Subroutine> {
        let lo = self.bump().span;
        self.eat_kw(Kw::Automatic);
        let ret = if function {
            let ty = if self.is_kw(Kw::Integer) || self.is_kw(Kw::Reg) {
                self.data_type(false)?
            } else {
                let signed = self.eat_kw(Kw::Signed);
                let range = if self.is(P::LBrack) {
                    Some(self.range()?)
                } else {
                    None
                };
                DataType {
                    kind: Some(NetKind::Reg),
                    signed,
                    range,
                }
            };
            Some(ty)
        } else {
            None
        };
        let name = self.ident()?;
        let mut decls = Vec::new();
        let mut params = Vec::new();
        if self.eat(P::LParen) {
            let mut current: Option<(PortDir, DataType)> = None;
            if !self.is(P::RParen) {
                loop {
                    let pattrs = self.attrs()?;
                    let plo = self.span();
                    let dir = match self.tok() {
                        Tok::Kw(Kw::Input) => Some(PortDir::Input),
                        Tok::Kw(Kw::Output) => Some(PortDir::Output),
                        Tok::Kw(Kw::Inout) => Some(PortDir::Inout),
                        _ => None,
                    };
                    if let Some(dir) = dir {
                        self.bump();
                        let mut ty = self.data_type(true)?;
                        if ty.kind.is_none() {
                            ty.kind = Some(NetKind::Reg);
                        }
                        current = Some((dir, ty));
                    }
                    let Some((dir, ty)) = current.clone() else {
                        return Err(self.expected("an argument direction"));
                    };
                    let n = self.ident()?;
                    decls.push(NetDecl {
                        attrs: pattrs,
                        dir: Some(dir),
                        ty,
                        names: vec![Declarator {
                            name: n,
                            dims: Vec::new(),
                            init: None,
                        }],
                        span: plo.to(self.prev_span()),
                    });
                    if !self.eat(P::Comma) {
                        break;
                    }
                }
            }
            self.expect(P::RParen)?;
        }
        self.expect(P::Semi)?;
        // Declarations, then one statement (several are accepted, as most
        // tools do, and become a block).
        let mut stmts = Vec::new();
        let end = if function { Kw::Endfunction } else { Kw::Endtask };
        loop {
            let a = self.attrs()?;
            match self.tok() {
                Tok::Kw(Kw::Input | Kw::Output | Kw::Inout) if stmts.is_empty() => {
                    let dir = match self.bump().tok {
                        Tok::Kw(Kw::Input) => PortDir::Input,
                        Tok::Kw(Kw::Output) => PortDir::Output,
                        _ => PortDir::Inout,
                    };
                    let mut d = self.net_decl(a, Some(dir))?;
                    if d.ty.kind.is_none() {
                        d.ty.kind = Some(NetKind::Reg);
                    }
                    decls.push(d);
                }
                Tok::Kw(Kw::Reg | Kw::Integer | Kw::Time | Kw::Real) if stmts.is_empty() => {
                    decls.push(self.net_decl(a, None)?)
                }
                Tok::Kw(Kw::Parameter | Kw::Localparam) if stmts.is_empty() => {
                    params.push(self.param_decl(a)?)
                }
                Tok::Kw(k) if *k == end => {
                    self.bump();
                    break;
                }
                Tok::Eof => bail!(lo, "`{}` without `{}`", if function { "function" } else { "task" }, end.name()),
                _ => {
                    let mut s = self.stmt()?;
                    s.attrs.extend(a);
                    stmts.push(s);
                }
            }
        }
        if self.eat(P::Colon) {
            self.ident()?;
        }
        let span = lo.to(self.prev_span());
        let body = match stmts.len() {
            1 => stmts.pop().expect("one statement"),
            _ => Stmt {
                kind: StmtKind::Block {
                    name: None,
                    decls: Vec::new(),
                    params: Vec::new(),
                    stmts,
                },
                attrs: Vec::new(),
                span,
            },
        };
        Ok(Subroutine {
            name,
            attrs,
            ret,
            decls,
            params,
            body,
            span,
        })
    }

    // ---------------------------------------------------------------
    // Generate

    /// A generate construct's body: `begin [: name] ... end` or one item.
    fn gen_block(&mut self) -> Result<GenBlock> {
        let lo = self.span();
        if self.eat_kw(Kw::Begin) {
            let name = if self.eat(P::Colon) {
                Some(self.ident()?)
            } else {
                None
            };
            let mut items = Vec::new();
            while !self.eat_kw(Kw::End) {
                if *self.tok() == Tok::Eof {
                    bail!(lo, "`begin` without `end`");
                }
                self.module_item(&mut items)?;
            }
            if self.eat(P::Colon) {
                self.ident()?;
            }
            Ok(GenBlock {
                name,
                items,
                span: lo.to(self.prev_span()),
            })
        } else {
            let mut items = Vec::new();
            self.module_item(&mut items)?;
            Ok(GenBlock {
                name: None,
                items,
                span: lo.to(self.prev_span()),
            })
        }
    }

    fn gen_for(&mut self) -> Result<Item> {
        let lo = self.bump().span;
        self.expect(P::LParen)?;
        self.eat_kw(Kw::Genvar);
        let genvar = self.ident()?;
        self.expect(P::Eq)?;
        let init = self.expr()?;
        self.expect(P::Semi)?;
        let cond = self.expr()?;
        self.expect(P::Semi)?;
        let step_var = self.ident()?;
        self.expect(P::Eq)?;
        let step = self.expr()?;
        self.expect(P::RParen)?;
        let body = self.gen_block()?;
        Ok(Item::GenFor {
            genvar,
            init,
            cond,
            step_var,
            step,
            body,
            span: lo.to(self.prev_span()),
        })
    }

    fn gen_if(&mut self) -> Result<Item> {
        let lo = self.bump().span;
        self.expect(P::LParen)?;
        let cond = self.expr()?;
        self.expect(P::RParen)?;
        let then = self.gen_block()?;
        let els = if self.eat_kw(Kw::Else) {
            Some(self.gen_block()?)
        } else {
            None
        };
        Ok(Item::GenIf {
            cond,
            then,
            els,
            span: lo.to(self.prev_span()),
        })
    }

    fn gen_case(&mut self) -> Result<Item> {
        let lo = self.bump().span;
        self.expect(P::LParen)?;
        let expr = self.expr()?;
        self.expect(P::RParen)?;
        let mut items = Vec::new();
        while !self.eat_kw(Kw::Endcase) {
            let exprs = if self.eat_kw(Kw::Default) {
                self.eat(P::Colon);
                Vec::new()
            } else {
                let mut v = vec![self.expr()?];
                while self.eat(P::Comma) {
                    v.push(self.expr()?);
                }
                self.expect(P::Colon)?;
                v
            };
            let body = self.gen_block()?;
            items.push(GenCaseItem { exprs, body });
        }
        Ok(Item::GenCase {
            expr,
            items,
            span: lo.to(self.prev_span()),
        })
    }

    // ---------------------------------------------------------------
    // Statements

    fn stmt(&mut self) -> Result<Stmt> {
        let attrs = self.attrs()?;
        let lo = self.span();
        let kind = match self.tok() {
            Tok::P(P::Semi) => {
                self.bump();
                StmtKind::Null
            }
            Tok::Kw(Kw::Begin) => {
                self.bump();
                let name = if self.eat(P::Colon) {
                    Some(self.ident()?)
                } else {
                    None
                };
                let mut decls = Vec::new();
                let mut params = Vec::new();
                let mut stmts = Vec::new();
                loop {
                    if self.eat_kw(Kw::End) {
                        break;
                    }
                    if *self.tok() == Tok::Eof {
                        bail!(lo, "`begin` without `end`");
                    }
                    let save = self.pos;
                    let a = self.attrs()?;
                    match self.tok() {
                        Tok::Kw(Kw::Reg | Kw::Integer | Kw::Time | Kw::Real | Kw::Realtime) => {
                            decls.push(self.net_decl(a, None)?)
                        }
                        Tok::Kw(Kw::Parameter | Kw::Localparam) => {
                            params.push(self.param_decl(a)?)
                        }
                        _ => {
                            self.pos = save;
                            stmts.push(self.stmt()?);
                        }
                    }
                }
                if self.eat(P::Colon) {
                    self.ident()?;
                }
                StmtKind::Block {
                    name,
                    decls,
                    params,
                    stmts,
                }
            }
            Tok::Kw(Kw::If) => {
                self.bump();
                self.expect(P::LParen)?;
                let cond = self.expr()?;
                self.expect(P::RParen)?;
                let then = Box::new(self.stmt()?);
                let els = if self.eat_kw(Kw::Else) {
                    Some(Box::new(self.stmt()?))
                } else {
                    None
                };
                StmtKind::If { cond, then, els }
            }
            Tok::Kw(k @ (Kw::Case | Kw::Casez | Kw::Casex)) => {
                let kind = match k {
                    Kw::Case => CaseKind::Case,
                    Kw::Casez => CaseKind::Casez,
                    _ => CaseKind::Casex,
                };
                self.bump();
                self.expect(P::LParen)?;
                let expr = self.expr()?;
                self.expect(P::RParen)?;
                let mut items = Vec::new();
                while !self.eat_kw(Kw::Endcase) {
                    if *self.tok() == Tok::Eof {
                        bail!(lo, "`case` without `endcase`");
                    }
                    let exprs = if self.eat_kw(Kw::Default) {
                        self.eat(P::Colon);
                        Vec::new()
                    } else {
                        let mut v = vec![self.expr()?];
                        while self.eat(P::Comma) {
                            v.push(self.expr()?);
                        }
                        self.expect(P::Colon)?;
                        v
                    };
                    let body = self.stmt()?;
                    items.push(CaseItem { exprs, body });
                }
                StmtKind::Case { kind, expr, items }
            }
            Tok::Kw(Kw::For) => {
                self.bump();
                self.expect(P::LParen)?;
                let init = Box::new(self.simple_assign()?);
                self.expect(P::Semi)?;
                let cond = self.expr()?;
                self.expect(P::Semi)?;
                let step = Box::new(self.simple_assign()?);
                self.expect(P::RParen)?;
                let body = Box::new(self.stmt()?);
                StmtKind::For {
                    init,
                    cond,
                    step,
                    body,
                }
            }
            Tok::Kw(Kw::While) => {
                self.bump();
                self.expect(P::LParen)?;
                let cond = self.expr()?;
                self.expect(P::RParen)?;
                StmtKind::While {
                    cond,
                    body: Box::new(self.stmt()?),
                }
            }
            Tok::Kw(Kw::Repeat) => {
                self.bump();
                self.expect(P::LParen)?;
                let count = self.expr()?;
                self.expect(P::RParen)?;
                StmtKind::Repeat {
                    count,
                    body: Box::new(self.stmt()?),
                }
            }
            Tok::Kw(Kw::Forever) => {
                self.bump();
                self.stmt()?;
                StmtKind::Unsupported("`forever`")
            }
            Tok::P(P::At) => {
                self.bump();
                let event = self.event()?;
                StmtKind::Event {
                    event,
                    body: Box::new(self.stmt()?),
                }
            }
            Tok::P(P::Hash) => {
                self.skip_delay()?;
                return self.stmt().map(|mut s| {
                    s.attrs.extend(attrs);
                    s
                });
            }
            Tok::Kw(Kw::Wait) => bail!(lo, "`wait` is not supported"),
            Tok::Kw(Kw::Fork) => bail!(lo, "`fork` is not supported"),
            Tok::Kw(Kw::Disable) => {
                self.bump();
                self.ident()?;
                self.expect(P::Semi)?;
                StmtKind::Unsupported("`disable`")
            }
            Tok::P(P::Arrow) => bail!(lo, "events are not supported"),
            Tok::Kw(k @ (Kw::Assign | Kw::Deassign | Kw::Force | Kw::Release)) => {
                bail!(lo, "procedural `{}` is not supported", k.name())
            }
            Tok::System(_) => {
                let name = match self.tok() {
                    Tok::System(n) => n.clone(),
                    _ => unreachable!(),
                };
                let span = self.bump().span;
                let mut args = Vec::new();
                if self.eat(P::LParen) {
                    if !self.is(P::RParen) {
                        loop {
                            if self.is(P::Comma) {
                                // An empty argument ($display(a,,b)).
                                args.push(Expr {
                                    kind: ExprKind::Str(Rc::from(&b""[..])),
                                    span: self.span(),
                                });
                            } else {
                                args.push(self.expr()?);
                            }
                            if !self.eat(P::Comma) {
                                break;
                            }
                        }
                    }
                    self.expect(P::RParen)?;
                }
                self.expect(P::Semi)?;
                StmtKind::SysTaskCall {
                    name: Ident { name, span },
                    args,
                }
            }
            Tok::Ident(_) if matches!(self.peek_tok(1), Tok::P(P::Semi) | Tok::P(P::LParen)) => {
                let name = self.ident()?;
                let mut args = Vec::new();
                if self.eat(P::LParen) {
                    if !self.is(P::RParen) {
                        loop {
                            args.push(self.expr()?);
                            if !self.eat(P::Comma) {
                                break;
                            }
                        }
                    }
                    self.expect(P::RParen)?;
                }
                self.expect(P::Semi)?;
                StmtKind::TaskCall { name, args }
            }
            _ => {
                let s = self.simple_assign()?;
                self.expect(P::Semi)?;
                return Ok(Stmt {
                    attrs,
                    ..s
                });
            }
        };
        Ok(Stmt {
            kind,
            attrs,
            span: lo.to(self.prev_span()),
        })
    }

    /// `lvalue = expr` or `lvalue <= expr` (an intra-assignment delay or
    /// event is dropped), without the `;`.
    fn simple_assign(&mut self) -> Result<Stmt> {
        let lo = self.span();
        let lhs = self.lvalue()?;
        let blocking = if self.eat(P::Eq) {
            true
        } else if self.eat(P::Le) {
            false
        } else {
            return Err(self.expected("`=` or `<=`"));
        };
        self.skip_delay()?;
        if self.is(P::At) {
            bail!(self.span(), "intra-assignment event controls are not supported");
        }
        let rhs = self.expr()?;
        Ok(Stmt {
            kind: StmtKind::Assign { lhs, rhs, blocking },
            attrs: Vec::new(),
            span: lo.to(self.prev_span()),
        })
    }

    fn event(&mut self) -> Result<Event> {
        if self.eat(P::Star) {
            return Ok(Event::Star);
        }
        if self.is(P::AttrOpen) && *self.peek_tok(1) == Tok::P(P::RParen) {
            // `@(*)` lexed with `(*` as an attribute opener.
            self.bump();
            self.bump();
            return Ok(Event::Star);
        }
        self.expect(P::LParen)?;
        if self.eat(P::Star) {
            self.expect(P::RParen)?;
            return Ok(Event::Star);
        }
        let mut items = Vec::new();
        loop {
            let edge = if self.eat_kw(Kw::Posedge) {
                Some(Edge::Pos)
            } else if self.eat_kw(Kw::Negedge) {
                Some(Edge::Neg)
            } else {
                None
            };
            let expr = self.expr()?;
            items.push(EventItem { edge, expr });
            if !(self.eat_kw(Kw::Or) || self.eat(P::Comma)) {
                break;
            }
        }
        self.expect(P::RParen)?;
        Ok(Event::List(items))
    }

    // ---------------------------------------------------------------
    // Expressions

    /// A net or variable reference, possibly selected, or a concatenation
    /// of them.
    fn lvalue(&mut self) -> Result<Expr> {
        if self.is(P::LBrace) {
            let lo = self.bump().span;
            let mut parts = vec![self.lvalue()?];
            while self.eat(P::Comma) {
                parts.push(self.lvalue()?);
            }
            let hi = self.expect(P::RBrace)?;
            return Ok(Expr {
                kind: ExprKind::Concat(parts),
                span: lo.to(hi),
            });
        }
        let base = self.name_expr()?;
        self.selects(base)
    }

    fn name_expr(&mut self) -> Result<Expr> {
        let first = self.ident()?;
        if !self.is(P::Dot) && !(self.is(P::LBrack) && self.hier_index_follows()) {
            return Ok(Expr {
                span: first.span,
                kind: ExprKind::Ident(first),
            });
        }
        // `a[1].b.c`: a hierarchical name. An index before a dot belongs
        // to the path.
        let lo = first.span;
        let mut path = Vec::new();
        let mut cur = first;
        loop {
            let mut idx = None;
            if self.is(P::LBrack) && self.hier_index_follows() {
                self.bump();
                idx = Some(self.expr()?);
                self.expect(P::RBrack)?;
            }
            path.push((cur, idx));
            if !self.eat(P::Dot) {
                break;
            }
            cur = self.ident()?;
        }
        Ok(Expr {
            kind: ExprKind::Hier(path),
            span: lo.to(self.prev_span()),
        })
    }

    /// Whether `[ ... ]` at the cursor is followed by a `.`: an index in a
    /// hierarchical path rather than a select.
    fn hier_index_follows(&self) -> bool {
        let mut depth = 0;
        let mut k = 0;
        loop {
            match self.peek_tok(k) {
                Tok::P(P::LBrack) => depth += 1,
                Tok::P(P::RBrack) => {
                    depth -= 1;
                    if depth == 0 {
                        return *self.peek_tok(k + 1) == Tok::P(P::Dot);
                    }
                }
                Tok::Eof | Tok::P(P::Semi) => return false,
                _ => {}
            }
            k += 1;
        }
    }

    fn selects(&mut self, mut e: Expr) -> Result<Expr> {
        while self.is(P::LBrack) {
            self.bump();
            let first = self.expr()?;
            let kind = if self.eat(P::Colon) {
                let lsb = self.expr()?;
                ExprKind::Slice {
                    base: Box::new(e),
                    msb: Box::new(first),
                    lsb: Box::new(lsb),
                }
            } else if self.is(P::PlusColon) || self.is(P::MinusColon) {
                let up = self.is(P::PlusColon);
                self.bump();
                let width = self.expr()?;
                ExprKind::IndexedSlice {
                    base: Box::new(e),
                    start: Box::new(first),
                    width: Box::new(width),
                    up,
                }
            } else {
                ExprKind::Index {
                    base: Box::new(e),
                    index: Box::new(first),
                }
            };
            let hi = self.expect(P::RBrack)?;
            let span = match &kind {
                ExprKind::Slice { base, .. }
                | ExprKind::IndexedSlice { base, .. }
                | ExprKind::Index { base, .. } => base.span.to(hi),
                _ => unreachable!(),
            };
            e = Expr { kind, span };
        }
        Ok(e)
    }

    pub fn expr(&mut self) -> Result<Expr> {
        let cond = self.binary(0)?;
        if self.eat(P::Question) {
            self.attrs()?;
            let then = self.expr()?;
            self.expect(P::Colon)?;
            let els = self.expr()?;
            let span = cond.span.to(els.span);
            return Ok(Expr {
                kind: ExprKind::Cond {
                    cond: Box::new(cond),
                    then: Box::new(then),
                    els: Box::new(els),
                },
                span,
            });
        }
        Ok(cond)
    }

    /// Binary operators by precedence climbing; IEEE 1364-2005 Table 5-4.
    fn binary(&mut self, min_prec: u8) -> Result<Expr> {
        let mut lhs = self.unary()?;
        loop {
            let Tok::P(p) = self.tok() else { break };
            let Some((op, prec)) = binop(*p) else { break };
            if prec < min_prec {
                break;
            }
            self.bump();
            self.attrs()?;
            let rhs = self.binary(prec + 1)?;
            let span = lhs.span.to(rhs.span);
            lhs = Expr {
                kind: ExprKind::Binary {
                    op,
                    lhs: Box::new(lhs),
                    rhs: Box::new(rhs),
                },
                span,
            };
        }
        Ok(lhs)
    }

    fn unary(&mut self) -> Result<Expr> {
        let op = match self.tok() {
            Tok::P(P::Plus) => Some(UnOp::Plus),
            Tok::P(P::Minus) => Some(UnOp::Minus),
            Tok::P(P::Bang) => Some(UnOp::LogicNot),
            Tok::P(P::Tilde) => Some(UnOp::BitNot),
            Tok::P(P::Amp) => Some(UnOp::RedAnd),
            Tok::P(P::TildeAmp) => Some(UnOp::RedNand),
            Tok::P(P::Pipe) => Some(UnOp::RedOr),
            Tok::P(P::TildePipe) => Some(UnOp::RedNor),
            Tok::P(P::Caret) => Some(UnOp::RedXor),
            Tok::P(P::TildeCaret | P::CaretTilde) => Some(UnOp::RedXnor),
            _ => None,
        };
        if let Some(op) = op {
            let lo = self.bump().span;
            self.attrs()?;
            let arg = self.unary()?;
            let span = lo.to(arg.span);
            return Ok(Expr {
                kind: ExprKind::Unary {
                    op,
                    arg: Box::new(arg),
                },
                span,
            });
        }
        self.primary()
    }

    fn primary(&mut self) -> Result<Expr> {
        let lo = self.span();
        let e = match self.tok() {
            Tok::Decimal(_) | Tok::Based { .. } => {
                let n = self.number()?;
                Expr {
                    kind: ExprKind::Number(n),
                    span: lo.to(self.prev_span()),
                }
            }
            Tok::Fill(c) => {
                self.bump();
                let s = State::parse(*c).unwrap_or(match c {
                    '0' => State::Zero,
                    _ => State::One,
                });
                Expr {
                    kind: ExprKind::Fill(s),
                    span: lo,
                }
            }
            Tok::Real(r) => {
                self.bump();
                let v: f64 = r
                    .parse()
                    .map_err(|_| Diag::new(lo, format!("bad real number {r}")))?;
                Expr {
                    kind: ExprKind::Real(v),
                    span: lo,
                }
            }
            Tok::Str(s) => {
                self.bump();
                Expr {
                    kind: ExprKind::Str(s.clone()),
                    span: lo,
                }
            }
            Tok::P(P::LParen) => {
                self.bump();
                let e = self.expr()?;
                if self.is(P::Colon) {
                    // A min:typ:max expression: the typical one.
                    self.bump();
                    let typ = self.expr()?;
                    self.expect(P::Colon)?;
                    self.expr()?;
                    self.expect(P::RParen)?;
                    return Ok(typ);
                }
                let hi = self.expect(P::RParen)?;
                Expr {
                    kind: e.kind,
                    span: lo.to(hi),
                }
            }
            Tok::P(P::LBrace) => {
                self.bump();
                let first = self.expr()?;
                if self.is(P::LBrace) {
                    // Replication: {n{a, b}}
                    self.bump();
                    let mut parts = vec![self.expr()?];
                    while self.eat(P::Comma) {
                        parts.push(self.expr()?);
                    }
                    self.expect(P::RBrace)?;
                    let hi = self.expect(P::RBrace)?;
                    Expr {
                        kind: ExprKind::Repeat {
                            count: Box::new(first),
                            parts,
                        },
                        span: lo.to(hi),
                    }
                } else {
                    let mut parts = vec![first];
                    while self.eat(P::Comma) {
                        parts.push(self.expr()?);
                    }
                    let hi = self.expect(P::RBrace)?;
                    Expr {
                        kind: ExprKind::Concat(parts),
                        span: lo.to(hi),
                    }
                }
            }
            Tok::System(name) => {
                let name = Ident {
                    name: name.clone(),
                    span: lo,
                };
                self.bump();
                let mut args = Vec::new();
                if self.eat(P::LParen) {
                    if !self.is(P::RParen) {
                        loop {
                            args.push(self.expr()?);
                            if !self.eat(P::Comma) {
                                break;
                            }
                        }
                    }
                    self.expect(P::RParen)?;
                }
                Expr {
                    kind: ExprKind::SysCall { name, args },
                    span: lo.to(self.prev_span()),
                }
            }
            Tok::Ident(_) if *self.peek_tok(1) == Tok::P(P::LParen) => {
                let name = self.ident()?;
                self.bump();
                let mut args = Vec::new();
                if !self.is(P::RParen) {
                    loop {
                        args.push(self.expr()?);
                        if !self.eat(P::Comma) {
                            break;
                        }
                    }
                }
                self.expect(P::RParen)?;
                Expr {
                    kind: ExprKind::Call { name, args },
                    span: lo.to(self.prev_span()),
                }
            }
            Tok::Ident(_) => {
                let base = self.name_expr()?;
                return self.selects(base);
            }
            _ => return Err(self.expected("an expression")),
        };
        // A parenthesized expression or concatenation can be selected from
        // only in SystemVerilog, but a concatenation's select is common
        // enough in generated code to accept.
        if matches!(e.kind, ExprKind::Concat(_)) && self.is(P::LBrack) {
            return self.selects(e);
        }
        Ok(e)
    }

    /// `[size] 'base digits`, or a plain decimal.
    fn number(&mut self) -> Result<Number> {
        let lo = self.span();
        let size = if let Tok::Decimal(d) = self.tok() {
            let d = d.clone();
            self.bump();
            match self.tok() {
                Tok::Based { .. } => {
                    let size: u32 = d
                        .parse()
                        .ok()
                        .filter(|&s| s > 0 && s <= 1 << 24)
                        .ok_or_else(|| Diag::new(lo, format!("bad size {d}")))?;
                    Some(size)
                }
                _ => {
                    let bits = decimal_bits(&d);
                    let width = bits.len().max(32);
                    let mut bits = bits;
                    bits.resize(width, State::Zero);
                    // An unsized decimal wider than 32 bits keeps its value.
                    return Ok(Number {
                        bits,
                        signed: true,
                        sized: false,
                    });
                }
            }
        } else {
            None
        };
        let Tok::Based {
            signed,
            base,
            digits,
        } = self.tok()
        else {
            return Err(self.expected("a number"));
        };
        self.bump();
        let span = lo.to(self.prev_span());
        let mut bits: Vec<State> = match base.bits_per_digit() {
            Some(bpd) => {
                let mut v = Vec::new();
                for c in digits.chars().rev() {
                    let d = match c {
                        'x' => [State::X; 4],
                        'z' | '?' => [State::Z; 4],
                        _ => {
                            let d = c.to_digit(16).filter(|&d| d < 1 << bpd).ok_or_else(|| {
                                Diag::new(span, format!("digit {c} is out of range for the base"))
                            })?;
                            let b = |i: u32| State::from(d >> i & 1 == 1);
                            [b(0), b(1), b(2), b(3)]
                        }
                    };
                    v.extend_from_slice(&d[..bpd as usize]);
                }
                v
            }
            None => match &digits[..] {
                "x" => vec![State::X],
                "z" | "?" => vec![State::Z],
                d if d.chars().all(|c| c.is_ascii_digit()) => decimal_bits(d),
                _ => bail!(span, "bad decimal digits {digits}"),
            },
        };
        let fill = match bits.last() {
            Some(State::X) => State::X,
            Some(State::Z) => State::Z,
            _ => State::Zero,
        };
        let width = match size {
            Some(s) => s as usize,
            None => bits.len().max(32),
        };
        bits.resize(width, fill);
        Ok(Number {
            bits,
            signed: *signed,
            sized: size.is_some(),
        })
    }
}

fn binop(p: P) -> Option<(BinOp, u8)> {
    Some(match p {
        P::Pow => (BinOp::Pow, 11),
        P::Star => (BinOp::Mul, 10),
        P::Slash => (BinOp::Div, 10),
        P::Percent => (BinOp::Mod, 10),
        P::Plus => (BinOp::Add, 9),
        P::Minus => (BinOp::Sub, 9),
        P::Shl => (BinOp::Shl, 8),
        P::Shr => (BinOp::Shr, 8),
        P::AShl => (BinOp::AShl, 8),
        P::AShr => (BinOp::AShr, 8),
        P::Lt => (BinOp::Lt, 7),
        P::Le => (BinOp::Le, 7),
        P::Gt => (BinOp::Gt, 7),
        P::Ge => (BinOp::Ge, 7),
        P::EqEq => (BinOp::Eq, 6),
        P::BangEq => (BinOp::Ne, 6),
        P::EqEqEq => (BinOp::CaseEq, 6),
        P::BangEqEq => (BinOp::CaseNe, 6),
        P::Amp => (BinOp::And, 5),
        P::Caret => (BinOp::Xor, 4),
        P::TildeCaret | P::CaretTilde => (BinOp::Xnor, 4),
        P::Pipe => (BinOp::Or, 3),
        P::AmpAmp => (BinOp::LogicAnd, 2),
        P::PipePipe => (BinOp::LogicOr, 1),
        _ => return None,
    })
}

/// A decimal digit string's value in bits, LSB first, as few as it needs
/// (at least one).
pub fn decimal_bits(digits: &str) -> Vec<State> {
    let mut limbs: Vec<u64> = vec![0];
    for c in digits.bytes() {
        let mut carry = (c - b'0') as u128;
        for l in limbs.iter_mut() {
            let v = *l as u128 * 10 + carry;
            *l = v as u64;
            carry = v >> 64;
        }
        if carry > 0 {
            limbs.push(carry as u64);
        }
    }
    let mut bits: Vec<State> = (0..limbs.len() * 64)
        .map(|i| State::from(limbs[i / 64] >> (i % 64) & 1 == 1))
        .collect();
    while bits.len() > 1 && bits.last() == Some(&State::Zero) {
        bits.pop();
    }
    bits
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pp::{Defines, Preprocessor};
    use crate::source::SourceMap;

    fn parse_text(text: &str) -> Result<Vec<Module>> {
        let mut sm = SourceMap::default();
        let f = sm.add_file("t.v".into(), text.into());
        let toks = Preprocessor::run(&mut sm, &mut Defines::default(), &[], f)?;
        parse(&toks, false)
    }

    #[test]
    fn numbers() {
        let m = parse_text("module m; localparam A = 4'b1x0z, B = 'hx, C = 8'sd200, D = 12; endmodule").unwrap();
        let Item::Param(p) = &m[0].items[0] else { panic!() };
        let n = |i: usize| match &p.assigns[i].1.kind {
            ExprKind::Number(n) => n.clone(),
            other => panic!("{other:?}"),
        };
        assert_eq!(chipsmith_netlist::Const(n(0).bits).to_string(), "1x0z");
        assert_eq!(n(1).bits, vec![State::X; 32]);
        assert!(n(2).signed && n(2).sized);
        assert_eq!(chipsmith_netlist::Const(n(2).bits).as_u64(), Some(200));
        assert!(n(3).signed && !n(3).sized);
        assert_eq!(n(3).bits.len(), 32);
    }

    #[test]
    fn precedence() {
        let m = parse_text("module m; assign x = a + b * c << 1 == d & e | f && g || h ? i : j; endmodule").unwrap();
        let Item::Assign { rhs, .. } = &m[0].items[0] else { panic!() };
        let ExprKind::Cond { cond, .. } = &rhs.kind else { panic!() };
        let ExprKind::Binary { op: BinOp::LogicOr, .. } = &cond.kind else { panic!() };
    }

    #[test]
    fn errors_say_what_was_expected() {
        let e = parse_text("module m; assign x = ; endmodule").unwrap_err();
        assert_eq!(e.message, "expected an expression, found `;`");
    }
}
