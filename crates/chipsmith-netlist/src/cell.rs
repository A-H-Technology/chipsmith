//! Cells: typed for the RTLIL cells synthesis matches on, generic for the
//! rest.

use std::borrow::Cow;

use crate::comb::{Alu, Binary, BinaryOp, Bmux, Demux, Lut, Mux, Pmux, Tribuf, Unary, UnaryOp};
use crate::ff::{Bit, Ff, Word};
use crate::gate::{Gate, GateOp};
use crate::mem::Mem;
use crate::param::Attributes;
use crate::ports::{CellError, Decoder, Dir, Generic, GenericPort, Params, Ports};
use crate::sig::SigBit;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cell {
    pub name: String,
    pub kind: CellKind,
    pub attributes: Attributes,
}

/// What a cell is. Each RTLIL cell family synthesis passes look for is a
/// variant with its ports as fields, so a `$mux` can't lack its `S` and a
/// pass reads `mux.s` instead of looking a port up by name. Everything else
/// (instances of the design's own modules, blackboxes and device cells,
/// RTLIL cells not modelled yet such as `$memrd_v2` or `$scopeinfo`) is
/// [`CellKind::Other`], carried through untouched.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CellKind {
    Unary(Unary),
    Binary(Binary),
    Mux(Mux),
    Pmux(Pmux),
    Bmux(Bmux),
    Demux(Demux),
    Tribuf(Tribuf),
    Alu(Alu),
    Lut(Lut),
    /// `$dff`, `$adffe`, `$dlatch`, ... (see [`crate::ff`]).
    Ff(Ff<Word>),
    /// `$_AND_`, `$_MUX_`, ...
    Gate(Gate),
    /// `$_DFF_P_`, `$_SDFFE_PN0P_`, ...
    FfGate(Ff<Bit>),
    Mem(Mem),
    Other(Generic),
}

/// One port of a cell as [`CellKind::for_each_port`] visits it.
#[derive(Debug, Clone, Copy)]
pub struct PortSig<'a> {
    pub name: &'a str,
    /// `None` only on an [`CellKind::Other`] port of unknown direction.
    pub dir: Option<Dir>,
    pub bits: &'a [SigBit],
}

impl CellKind {
    /// The typed cell, or `Other` if the type isn't one the IR models.
    /// An error if it is one but the parameters or ports don't fit it.
    pub fn from_generic(g: Generic) -> Result<CellKind, CellError> {
        let ty = g.ty.as_str();
        let typed = ty.starts_with('$')
            && (UnaryOp::parse(ty).is_some()
                || BinaryOp::parse(ty).is_some()
                || GateOp::parse(ty).is_some()
                || matches!(
                    ty,
                    "$mux" | "$pmux" | "$bmux" | "$demux" | "$tribuf" | "$alu" | "$lut" | "$mem_v2"
                )
                || crate::ff::WORD_TYPES.contains(&ty)
                || ty.starts_with("$_"));
        if !typed {
            return Ok(CellKind::Other(g));
        }
        let mut d = Decoder::new(g);
        let ty = d.ty().to_owned();
        let kind = if let Some(op) = UnaryOp::parse(&ty) {
            CellKind::Unary(Unary::decode(op, &mut d)?)
        } else if let Some(op) = BinaryOp::parse(&ty) {
            CellKind::Binary(Binary::decode(op, &mut d)?)
        } else if let Some(op) = GateOp::parse(&ty) {
            CellKind::Gate(Gate::decode(op, &mut d)?)
        } else if let Some(ff) = Ff::<Word>::decode(&mut d) {
            CellKind::Ff(ff?)
        } else if let Some(ff) = Ff::<Bit>::decode(&mut d) {
            CellKind::FfGate(ff?)
        } else {
            match ty.as_str() {
                "$mux" => CellKind::Mux(Mux::decode(&mut d)?),
                "$pmux" => CellKind::Pmux(Pmux::decode(&mut d)?),
                "$bmux" => CellKind::Bmux(Bmux::decode(&mut d)?),
                "$demux" => CellKind::Demux(Demux::decode(&mut d)?),
                "$tribuf" => CellKind::Tribuf(Tribuf::decode(&mut d)?),
                "$alu" => CellKind::Alu(Alu::decode(&mut d)?),
                "$lut" => CellKind::Lut(Lut::decode(&mut d)?),
                "$mem_v2" => CellKind::Mem(Mem::decode(&mut d)?),
                // Some other `$_..._` gate-level cell (`$_MUX4_`, `$_SR_`
                // spelled differently, ...): not modelled.
                _ => return Ok(CellKind::Other(d.into_generic())),
            }
        };
        d.finish()?;
        Ok(kind)
    }

    pub fn type_name(&self) -> Cow<'_, str> {
        match self {
            CellKind::Unary(c) => c.op.name().into(),
            CellKind::Binary(c) => c.op.name().into(),
            CellKind::Mux(_) => "$mux".into(),
            CellKind::Pmux(_) => "$pmux".into(),
            CellKind::Bmux(_) => "$bmux".into(),
            CellKind::Demux(_) => "$demux".into(),
            CellKind::Tribuf(_) => "$tribuf".into(),
            CellKind::Alu(_) => "$alu".into(),
            CellKind::Lut(_) => "$lut".into(),
            CellKind::Ff(c) => c.type_name().into(),
            CellKind::Gate(c) => c.op().name().into(),
            CellKind::FfGate(c) => c.type_name().into(),
            CellKind::Mem(_) => "$mem_v2".into(),
            CellKind::Other(g) => g.ty.as_str().into(),
        }
    }

    /// The generic form's parameters, for a typed cell.
    fn typed_params(&self) -> Params {
        let mut p = Params::new();
        match self {
            CellKind::Unary(c) => c.params(&mut p),
            CellKind::Binary(c) => c.params(&mut p),
            CellKind::Mux(c) => c.params(&mut p),
            CellKind::Pmux(c) => c.params(&mut p),
            CellKind::Bmux(c) => c.params(&mut p),
            CellKind::Demux(c) => c.params(&mut p),
            CellKind::Tribuf(c) => c.params(&mut p),
            CellKind::Alu(c) => c.params(&mut p),
            CellKind::Lut(c) => c.params(&mut p),
            CellKind::Ff(c) => c.params(&mut p),
            CellKind::Mem(c) => c.params(&mut p),
            CellKind::Gate(_) | CellKind::FfGate(_) | CellKind::Other(_) => {}
        }
        p
    }

    /// The cell as yosys' JSON has it. Concatenates ports that come in
    /// pieces (`$mem_v2`'s).
    pub fn to_generic(&self) -> Generic {
        if let CellKind::Other(g) = self {
            return g.clone();
        }
        let mut ports: Vec<GenericPort> = Vec::new();
        self.for_each_port(|p| match ports.last_mut() {
            Some(last) if last.name == p.name => last.sig.extend_from_slice(p.bits),
            _ => ports.push(GenericPort {
                name: p.name.to_owned(),
                dir: p.dir,
                sig: p.bits.to_vec(),
            }),
        });
        Generic {
            ty: self.type_name().into_owned(),
            params: self
                .typed_params()
                .0
                .into_iter()
                .map(|(k, v)| (k.to_owned(), v))
                .collect(),
            ports,
        }
    }

    /// Every port, in a fixed order per cell type. A `$mem_v2` port comes
    /// in one piece per memory port, consecutive, lowest bits first.
    pub fn for_each_port<'a>(&'a self, mut f: impl FnMut(PortSig<'a>)) {
        let mut typed = |name, dir, bits| {
            f(PortSig {
                name,
                dir: Some(dir),
                bits,
            })
        };
        match self {
            CellKind::Unary(c) => c.for_each_port(&mut typed),
            CellKind::Binary(c) => c.for_each_port(&mut typed),
            CellKind::Mux(c) => c.for_each_port(&mut typed),
            CellKind::Pmux(c) => c.for_each_port(&mut typed),
            CellKind::Bmux(c) => c.for_each_port(&mut typed),
            CellKind::Demux(c) => c.for_each_port(&mut typed),
            CellKind::Tribuf(c) => c.for_each_port(&mut typed),
            CellKind::Alu(c) => c.for_each_port(&mut typed),
            CellKind::Lut(c) => c.for_each_port(&mut typed),
            CellKind::Ff(c) => c.for_each_port(&mut typed),
            CellKind::Gate(c) => c.for_each_port(&mut typed),
            CellKind::FfGate(c) => c.for_each_port(&mut typed),
            CellKind::Mem(c) => c.for_each_port(&mut typed),
            CellKind::Other(g) => {
                for p in &g.ports {
                    f(PortSig {
                        name: &p.name,
                        dir: p.dir,
                        bits: &p.sig,
                    })
                }
            }
        }
    }

    /// Every bit a pass could rewire, for substituting nets.
    pub fn for_each_bit_mut(&mut self, mut f: impl FnMut(&mut SigBit)) {
        let f: &mut dyn FnMut(&mut SigBit) = &mut f;
        match self {
            CellKind::Unary(c) => c.for_each_bit_mut(f),
            CellKind::Binary(c) => c.for_each_bit_mut(f),
            CellKind::Mux(c) => c.for_each_bit_mut(f),
            CellKind::Pmux(c) => c.for_each_bit_mut(f),
            CellKind::Bmux(c) => c.for_each_bit_mut(f),
            CellKind::Demux(c) => c.for_each_bit_mut(f),
            CellKind::Tribuf(c) => c.for_each_bit_mut(f),
            CellKind::Alu(c) => c.for_each_bit_mut(f),
            CellKind::Lut(c) => c.for_each_bit_mut(f),
            CellKind::Ff(c) => c.for_each_bit_mut(f),
            CellKind::Gate(c) => c.for_each_bit_mut(f),
            CellKind::FfGate(c) => c.for_each_bit_mut(f),
            CellKind::Mem(c) => c.for_each_bit_mut(f),
            CellKind::Other(g) => g.ports.iter_mut().flat_map(|p| &mut p.sig).for_each(f),
        }
    }

    /// The width relations between ports the types don't enforce (a
    /// `$mux`'s A, B and Y are one width). Decoding checks them; a pass
    /// that rewires ports can break them.
    pub fn check(&self) -> Result<(), String> {
        match self {
            CellKind::Mux(c) => c.check(),
            CellKind::Pmux(c) => c.check(),
            CellKind::Bmux(c) => c.check(),
            CellKind::Demux(c) => c.check(),
            CellKind::Tribuf(c) => c.check(),
            CellKind::Alu(c) => c.check(),
            CellKind::Lut(c) => c.check(),
            CellKind::Ff(c) => c.check().and_then(|()| c.check_values()),
            CellKind::Mem(c) => c.check(),
            CellKind::Unary(_)
            | CellKind::Binary(_)
            | CellKind::Gate(_)
            | CellKind::FfGate(_)
            | CellKind::Other(_) => Ok(()),
        }
    }
}
