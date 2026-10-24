//! The generic form of a cell (type name, parameters, named ports) and the
//! decoding of it into the typed cells.
//!
//! Every typed cell converts both ways: `decode` reads it from the generic
//! form (strictly: a missing port or parameter, a width parameter that
//! disagrees with its port, or one left over is an error, since dropping it
//! would change the design), and its port visitor and `params` write it
//! back. The JSON layer only ever sees the generic form.

use std::collections::BTreeMap;

use crate::param::ParamValue;
use crate::sig::{Const, Polarity, SigBit, SigSpec};

/// Which way a port points.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Dir {
    In,
    Out,
    InOut,
}

impl Dir {
    pub fn parse(s: &str) -> Option<Dir> {
        match s {
            "input" => Some(Dir::In),
            "output" => Some(Dir::Out),
            "inout" => Some(Dir::InOut),
            _ => None,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Dir::In => "input",
            Dir::Out => "output",
            Dir::InOut => "inout",
        }
    }

    pub fn drives(self) -> bool {
        matches!(self, Dir::Out | Dir::InOut)
    }

    pub fn reads(self) -> bool {
        matches!(self, Dir::In | Dir::InOut)
    }
}

/// A port of a cell whose type the IR doesn't model.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GenericPort {
    pub name: String,
    /// yosys leaves it out for a cell type it knows nothing about (no
    /// module, not an internal cell); the IR then can't tell a driver from
    /// a user, and treats the port as both.
    pub dir: Option<Dir>,
    pub sig: SigSpec,
}

/// A cell as yosys' JSON has it: a type name, parameters and ports.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Generic {
    pub ty: String,
    pub params: BTreeMap<String, ParamValue>,
    pub ports: Vec<GenericPort>,
}

impl Generic {
    pub fn port(&self, name: &str) -> Option<&GenericPort> {
        self.ports.iter().find(|p| p.name == name)
    }
}

/// Why a cell's generic form isn't a valid typed cell.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{ty}: {message}")]
pub struct CellError {
    pub ty: String,
    pub message: String,
}

/// Borrowing a port's bits whatever its Rust type.
pub(crate) trait AsBits {
    fn bits(&self) -> &[SigBit];
    fn bits_mut(&mut self) -> &mut [SigBit];
}

impl AsBits for SigBit {
    fn bits(&self) -> &[SigBit] {
        std::slice::from_ref(self)
    }
    fn bits_mut(&mut self) -> &mut [SigBit] {
        std::slice::from_mut(self)
    }
}

impl AsBits for SigSpec {
    fn bits(&self) -> &[SigBit] {
        self
    }
    fn bits_mut(&mut self) -> &mut [SigBit] {
        self
    }
}

/// A typed cell's ports, in a fixed order.
///
/// A port can come in several pieces with the same name, which concatenate
/// (lowest bits first): `$mem_v2`'s `RD_ADDR` is every read port's address.
/// Pieces that only exist as constants in the generic form (an
/// asynchronous read port's clock) are visited by `for_each_port` and not by
/// `for_each_bit_mut`, which only reaches bits that can be rewired.
pub(crate) trait Ports {
    fn for_each_port<'a>(&'a self, f: &mut dyn FnMut(&'a str, Dir, &'a [SigBit]));
    fn for_each_bit_mut(&mut self, f: &mut dyn FnMut(&mut SigBit));
}

/// `Ports` for a struct whose ports are fields: `"A" In a.sig`.
macro_rules! ports {
    ($ty:ty { $($name:literal $dir:ident $($field:ident).+),* $(,)? }) => {
        impl $crate::ports::Ports for $ty {
            fn for_each_port<'a>(
                &'a self,
                f: &mut dyn FnMut(&'a str, $crate::ports::Dir, &'a [$crate::sig::SigBit]),
            ) {
                use $crate::ports::AsBits;
                $( f($name, $crate::ports::Dir::$dir, self.$($field).+.bits()); )*
            }
            fn for_each_bit_mut(&mut self, f: &mut dyn FnMut(&mut $crate::sig::SigBit)) {
                use $crate::ports::AsBits;
                $( self.$($field).+.bits_mut().iter_mut().for_each(&mut *f); )*
            }
        }
    };
}
pub(crate) use ports;

/// Takes a typed cell apart from its generic form.
pub(crate) struct Decoder {
    ty: String,
    params: BTreeMap<String, ParamValue>,
    ports: Vec<GenericPort>,
}

impl Decoder {
    pub fn new(g: Generic) -> Decoder {
        Decoder {
            ty: g.ty,
            params: g.params,
            ports: g.ports,
        }
    }

    pub fn ty(&self) -> &str {
        &self.ty
    }

    pub fn error(&self, message: impl Into<String>) -> CellError {
        CellError {
            ty: self.ty.clone(),
            message: message.into(),
        }
    }

    pub fn port(&mut self, name: &str, dir: Dir) -> Result<SigSpec, CellError> {
        let i = self
            .ports
            .iter()
            .position(|p| p.name == name)
            .ok_or_else(|| self.error(format!("no port {name}")))?;
        let p = self.ports.swap_remove(i);
        if p.dir.is_some_and(|d| d != dir) {
            return Err(self.error(format!("port {name} is not an {}", dir.name())));
        }
        Ok(p.sig)
    }

    /// A port of exactly `width` bits.
    pub fn port_w(&mut self, name: &str, dir: Dir, width: usize) -> Result<SigSpec, CellError> {
        let sig = self.port(name, dir)?;
        if sig.len() != width {
            return Err(self.error(format!(
                "port {name} is {} bits, expected {width}",
                sig.len()
            )));
        }
        Ok(sig)
    }

    pub fn bit(&mut self, name: &str, dir: Dir) -> Result<SigBit, CellError> {
        Ok(self.port_w(name, dir, 1)?[0])
    }

    pub fn param(&mut self, name: &str) -> Result<ParamValue, CellError> {
        self.params
            .remove(name)
            .ok_or_else(|| self.error(format!("no parameter {name}")))
    }

    pub fn konst(&mut self, name: &str) -> Result<Const, CellError> {
        match self.param(name)? {
            ParamValue::Bits(c) => Ok(c),
            ParamValue::Str(s) => Err(self.error(format!("parameter {name} is a string ({s:?})"))),
        }
    }

    /// A constant of exactly `width` bits. yosys writes a zero-width
    /// per-port parameter of `$mem_v2` (no ports of that kind) as one 0 bit,
    /// so that is accepted for 0 too.
    pub fn konst_w(&mut self, name: &str, width: usize) -> Result<Const, CellError> {
        let c = self.konst(name)?;
        if c.len() == width {
            Ok(c)
        } else if width == 0 && c.0 == [crate::sig::State::Zero] {
            Ok(Const::default())
        } else {
            Err(self.error(format!(
                "parameter {name} is {} bits, expected {width}",
                c.len()
            )))
        }
    }

    pub fn int(&mut self, name: &str) -> Result<i64, CellError> {
        let c = self.konst(name)?;
        c.as_i64()
            .ok_or_else(|| self.error(format!("parameter {name} = {c} is not an integer")))
    }

    pub fn uint(&mut self, name: &str) -> Result<usize, CellError> {
        let v = self.int(name)?;
        usize::try_from(v).map_err(|_| self.error(format!("parameter {name} = {v} is negative")))
    }

    pub fn flag(&mut self, name: &str) -> Result<bool, CellError> {
        Ok(self.konst(name)?.as_bool())
    }

    pub fn polarity(&mut self, name: &str) -> Result<Polarity, CellError> {
        Ok(Polarity::from_bool(self.flag(name)?))
    }

    /// A width parameter, which must equal `actual`: the IR derives widths
    /// from the ports, so a disagreeing one can't be kept.
    pub fn width(&mut self, name: &str, actual: usize) -> Result<(), CellError> {
        let w = self.uint(name)?;
        if w != actual {
            return Err(self.error(format!("{name} = {w} but the port is {actual} bits")));
        }
        Ok(())
    }

    /// Gives up decoding: the cell stays generic.
    pub fn into_generic(self) -> Generic {
        Generic {
            ty: self.ty,
            params: self.params,
            ports: self.ports,
        }
    }

    /// Fails if a port or parameter wasn't taken: the typed cell would drop
    /// it.
    pub fn finish(self) -> Result<(), CellError> {
        if let Some(p) = self.ports.first() {
            return Err(self.error(format!("unexpected port {}", p.name)));
        }
        if let Some(name) = self.params.keys().next() {
            return Err(self.error(format!("unexpected parameter {name}")));
        }
        Ok(())
    }
}

/// Builds the generic form's parameter list.
pub(crate) struct Params(pub Vec<(&'static str, ParamValue)>);

impl Params {
    pub fn new() -> Params {
        Params(Vec::new())
    }

    pub fn int(&mut self, name: &'static str, v: usize) -> &mut Params {
        self.0.push((name, ParamValue::int(v as i64)));
        self
    }

    pub fn flag(&mut self, name: &'static str, b: bool) -> &mut Params {
        // 32 bits, as most yosys passes write flags (`proc` writes 1).
        self.0.push((name, ParamValue::int(b as i64)));
        self
    }

    pub fn polarity(&mut self, name: &'static str, p: Polarity) -> &mut Params {
        self.flag(name, p.is_pos())
    }

    pub fn konst(&mut self, name: &'static str, c: Const) -> &mut Params {
        self.0.push((name, ParamValue::Bits(c)));
        self
    }
}
