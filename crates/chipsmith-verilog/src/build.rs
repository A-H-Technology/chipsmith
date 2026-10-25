//! Building one netlist module: cells that fold when their inputs are
//! constant, and nets joined by continuous assignments.
//!
//! The IR's nets are canonical (ADR-0005): `assign a = b` can't be a
//! connection between two wires, so while a module is built every such
//! join goes into a union-find over its nets, and [`Builder::finish`]
//! rewrites every bit to its representative once. A net joined to a
//! constant becomes that constant.

use chipsmith_netlist::{
    attr, Binary, BinaryOp, Cell, CellKind, Module, Mux, Net, Operand, ParamValue, Pmux, SigBit,
    SigSpec, State, Tribuf, Unary, UnaryOp,
};

use crate::fold;
use crate::source::{SourceMap, Span};

/// What a net was joined to.
#[derive(Debug, Clone, Copy)]
enum Link {
    Root,
    Parent(u32),
    Const(State),
}

pub struct Builder<'s> {
    pub module: Module,
    pub sources: &'s SourceMap,
    links: Vec<Link>,
    /// Every net allocated, by index: `Net` can only be made by the
    /// module, so the union-find keeps them to map indices back.
    nets: Vec<Net>,
    /// Numbers the cells, as yosys' `$add$file:line$N` names do.
    serial: u32,
    /// Where each net was joined to a different constant than it already
    /// had: reported by [`Builder::finish`].
    conflicts: Vec<(Net, Span)>,
}

pub fn konst(bits: &[State]) -> SigSpec {
    bits.iter().map(|&s| SigBit::Const(s)).collect()
}

/// The bits of a signal if all are constant.
pub fn as_const(sig: &[SigBit]) -> Option<Vec<State>> {
    sig.iter()
        .map(|b| match b {
            SigBit::Const(s) => Some(*s),
            SigBit::Net(_) => None,
        })
        .collect()
}

pub fn xs(width: usize) -> SigSpec {
    vec![SigBit::X; width]
}

/// Extends (with the sign bit if `signed`) or truncates.
pub fn extend(sig: &[SigBit], width: usize, signed: bool) -> SigSpec {
    let mut v: SigSpec = sig.iter().take(width).copied().collect();
    let fill = if signed {
        sig.last().copied().unwrap_or(SigBit::ZERO)
    } else {
        SigBit::ZERO
    };
    v.resize(width, fill);
    v
}

fn all_z(sig: &[SigBit]) -> bool {
    !sig.is_empty() && sig.iter().all(|&b| b == SigBit::Z)
}

impl<'s> Builder<'s> {
    pub fn new(name: &str, sources: &'s SourceMap) -> Builder<'s> {
        Builder {
            module: Module::new(name),
            sources,
            links: Vec::new(),
            nets: Vec::new(),
            serial: 0,
            conflicts: Vec::new(),
        }
    }

    pub fn new_sig(&mut self, width: usize) -> SigSpec {
        let sig = self.module.new_sig(width);
        self.nets.extend(sig.iter().filter_map(|b| b.net()));
        self.links.resize(self.module.net_count(), Link::Root);
        sig
    }

    /// A cell name: `$type$path:line$n`, as yosys' front end names them.
    pub fn cell_name(&mut self, ty: &str, span: Span) -> String {
        self.serial += 1;
        let loc = self.sources.location(span);
        format!("{ty}${}:{}${}", loc.path, loc.line, self.serial)
    }

    pub fn add(&mut self, ty_name: &str, kind: CellKind, span: Span) -> usize {
        let name = self.cell_name(ty_name, span);
        let id = self.module.add_cell(name, kind);
        self.module.cells[id]
            .attributes
            .insert(attr::SRC.into(), ParamValue::Str(self.sources.src(span)));
        id
    }

    pub fn add_named(&mut self, name: String, kind: CellKind, span: Span) -> &mut Cell {
        let id = self.module.add_cell(name, kind);
        let c = &mut self.module.cells[id];
        c.attributes
            .insert(attr::SRC.into(), ParamValue::Str(self.sources.src(span)));
        c
    }

    pub fn unary(
        &mut self,
        op: UnaryOp,
        a: SigSpec,
        signed: bool,
        width: usize,
        span: Span,
    ) -> SigSpec {
        if let Some(c) = as_const(&a) {
            return konst(&fold::unary(op, &c, signed, width));
        }
        let y = self.new_sig(width);
        self.add(
            op.name(),
            CellKind::Unary(Unary {
                op,
                a: Operand { sig: a, signed },
                y: y.clone(),
            }),
            span,
        );
        y
    }

    /// `Y = A op B`; operands' signedness as given.
    #[allow(clippy::too_many_arguments)]
    pub fn binary(
        &mut self,
        op: BinaryOp,
        a: SigSpec,
        a_signed: bool,
        b: SigSpec,
        b_signed: bool,
        width: usize,
        span: Span,
    ) -> SigSpec {
        if let (Some(ca), Some(cb)) = (as_const(&a), as_const(&b)) {
            return konst(&fold::binary(op, &ca, a_signed, &cb, b_signed, width));
        }
        let y = self.new_sig(width);
        self.add(
            op.name(),
            CellKind::Binary(Binary {
                op,
                a: Operand {
                    sig: a,
                    signed: a_signed,
                },
                b: Operand {
                    sig: b,
                    signed: b_signed,
                },
                y: y.clone(),
            }),
            span,
        );
        y
    }

    /// One bit: any bit of `a` set.
    pub fn reduce_bool(&mut self, a: SigSpec, span: Span) -> SigBit {
        if a.len() == 1 {
            return a[0];
        }
        self.unary(UnaryOp::ReduceBool, a, false, 1, span)[0]
    }

    pub fn not1(&mut self, a: SigBit, span: Span) -> SigBit {
        self.unary(UnaryOp::Not, vec![a], false, 1, span)[0]
    }

    pub fn and1(&mut self, a: SigBit, b: SigBit, span: Span) -> SigBit {
        match (a, b) {
            (SigBit::ONE, x) | (x, SigBit::ONE) => x,
            _ => self.binary(BinaryOp::And, vec![a], false, vec![b], false, 1, span)[0],
        }
    }

    pub fn or1(&mut self, a: SigBit, b: SigBit, span: Span) -> SigBit {
        match (a, b) {
            (SigBit::ZERO, x) | (x, SigBit::ZERO) => x,
            _ => self.binary(BinaryOp::Or, vec![a], false, vec![b], false, 1, span)[0],
        }
    }

    /// `s ? b : a`. A side that is all `z` makes a tri-state buffer, as
    /// yosys' `tribuf` pass does with the `$mux` its front end builds.
    pub fn mux(&mut self, s: SigBit, a: SigSpec, b: SigSpec, span: Span) -> SigSpec {
        debug_assert_eq!(a.len(), b.len());
        if a == b {
            return a;
        }
        match s {
            SigBit::Const(State::One) => return b,
            SigBit::Const(State::Zero) => return a,
            SigBit::Const(_) => {
                if let (Some(ca), Some(cb)) = (as_const(&a), as_const(&b)) {
                    return ca
                        .iter()
                        .zip(&cb)
                        .map(|(&x, &y)| SigBit::Const(if x == y { x } else { State::X }))
                        .collect();
                }
            }
            SigBit::Net(_) => {}
        }
        if all_z(&a) && !all_z(&b) {
            return self.tribuf(b, s, span);
        }
        if all_z(&b) && !all_z(&a) {
            let en = self.not1(s, span);
            return self.tribuf(a, en, span);
        }
        let y = self.new_sig(a.len());
        self.add(
            "$mux",
            CellKind::Mux(Mux {
                a,
                b,
                s,
                y: y.clone(),
            }),
            span,
        );
        y
    }

    pub fn tribuf(&mut self, a: SigSpec, en: SigBit, span: Span) -> SigSpec {
        match en {
            SigBit::ONE => return a,
            SigBit::ZERO => return vec![SigBit::Z; a.len()],
            _ => {}
        }
        let y = self.new_sig(a.len());
        self.add(
            "$tribuf",
            CellKind::Tribuf(Tribuf {
                a,
                en,
                y: y.clone(),
            }),
            span,
        );
        y
    }

    /// `A` unless one of `cases`' selects is set, then its value; at most
    /// one set (a `$pmux`). Constant selects fold.
    pub fn pmux(&mut self, a: SigSpec, cases: Vec<(SigBit, SigSpec)>, span: Span) -> SigSpec {
        let cases: Vec<_> = cases
            .into_iter()
            .filter(|(s, _)| *s != SigBit::ZERO)
            .collect();
        if let Some(i) = cases.iter().position(|(s, _)| *s == SigBit::ONE) {
            if cases.len() == 1 {
                return cases[i].1.clone();
            }
        }
        match cases.len() {
            0 => a,
            1 => {
                let (s, b) = cases.into_iter().next().expect("one case");
                self.mux(s, a, b, span)
            }
            _ => {
                let y = self.new_sig(a.len());
                let (s, b): (Vec<SigBit>, Vec<SigSpec>) = cases.into_iter().unzip();
                self.add(
                    "$pmux",
                    CellKind::Pmux(Pmux {
                        a,
                        b: b.concat(),
                        s,
                        y: y.clone(),
                    }),
                    span,
                );
                y
            }
        }
    }

    fn find(&mut self, n: u32) -> Link {
        let mut root = n;
        loop {
            match self.links[root as usize] {
                Link::Parent(p) => root = p,
                Link::Root => break,
                Link::Const(s) => {
                    self.compress(n, root);
                    return Link::Const(s);
                }
            }
        }
        self.compress(n, root);
        Link::Parent(root)
    }

    fn compress(&mut self, mut n: u32, root: u32) {
        while n != root {
            let Link::Parent(p) = self.links[n as usize] else {
                break;
            };
            self.links[n as usize] = Link::Parent(root);
            n = p;
        }
    }

    /// Joins two bits: `assign a = b` for one bit. A bit joined to two
    /// different constants is reported by [`Builder::finish`].
    pub fn join(&mut self, a: SigBit, b: SigBit, span: Span) {
        let ra = match a {
            SigBit::Net(n) => self.find(n.index() as u32),
            SigBit::Const(s) => Link::Const(s),
        };
        let rb = match b {
            SigBit::Net(n) => self.find(n.index() as u32),
            SigBit::Const(s) => Link::Const(s),
        };
        match (ra, rb) {
            (Link::Parent(x), Link::Parent(y)) => {
                if x != y {
                    self.links[x as usize] = Link::Parent(y);
                }
            }
            (Link::Parent(x), Link::Const(s)) | (Link::Const(s), Link::Parent(x)) => {
                self.links[x as usize] = Link::Const(s);
            }
            (Link::Const(s), Link::Const(t)) => {
                if s != t {
                    let net = a.net().or(b.net());
                    if let Some(net) = net {
                        self.conflicts.push((net, span));
                    }
                }
            }
            (Link::Root, _) | (_, Link::Root) => unreachable!("find returns roots as Parent"),
        }
    }

    pub fn join_sig(&mut self, a: &[SigBit], b: &[SigBit], span: Span) {
        debug_assert_eq!(a.len(), b.len());
        for (&x, &y) in a.iter().zip(b) {
            self.join(x, y, span);
        }
    }

    /// The module with every bit rewritten to its representative. Errors
    /// name a net joined to two different constants.
    pub fn finish(mut self) -> Result<Module, Vec<(Net, Span)>> {
        if !self.conflicts.is_empty() {
            return Err(self.conflicts);
        }
        let n = self.links.len();
        let mut map: Vec<SigBit> = Vec::with_capacity(n);
        for i in 0..n as u32 {
            map.push(match self.find(i) {
                Link::Const(s) => SigBit::Const(s),
                Link::Parent(r) => SigBit::Net(self.nets[r as usize]),
                Link::Root => unreachable!(),
            });
        }
        self.module.map_bits(|b| match b {
            SigBit::Net(n) => map[n.index()],
            c => c,
        });
        Ok(self.module)
    }
}
