//! Word-level combinational cells: RTLIL's operators, multiplexers,
//! `$alu`, `$lut` and `$tribuf`.
//!
//! Width parameters (`A_WIDTH`, `WIDTH`, `S_WIDTH`, ...) aren't stored:
//! they're the lengths of the ports, so they can't disagree with them.

use crate::ports::{ports, CellError, Decoder, Dir, Params};
use crate::sig::{Const, SigBit, SigSpec};

/// Names both ways for a fieldless operator enum.
macro_rules! op_names {
    ($ty:ident { $($op:ident = $name:literal),* $(,)? }) => {
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
        pub enum $ty { $($op),* }

        impl $ty {
            pub const ALL: &'static [$ty] = &[$($ty::$op),*];

            pub fn name(self) -> &'static str {
                match self { $($ty::$op => $name),* }
            }

            pub fn parse(name: &str) -> Option<$ty> {
                match name { $($name => Some($ty::$op),)* _ => None }
            }
        }
    };
}
pub(crate) use op_names;

op_names!(UnaryOp {
    Not = "$not",
    Pos = "$pos",
    Neg = "$neg",
    ReduceAnd = "$reduce_and",
    ReduceOr = "$reduce_or",
    ReduceXor = "$reduce_xor",
    ReduceXnor = "$reduce_xnor",
    ReduceBool = "$reduce_bool",
    LogicNot = "$logic_not",
});

op_names!(BinaryOp {
    And = "$and",
    Or = "$or",
    Xor = "$xor",
    Xnor = "$xnor",
    Shl = "$shl",
    Shr = "$shr",
    Sshl = "$sshl",
    Sshr = "$sshr",
    Shift = "$shift",
    Shiftx = "$shiftx",
    LogicAnd = "$logic_and",
    LogicOr = "$logic_or",
    Eq = "$eq",
    Ne = "$ne",
    Eqx = "$eqx",
    Nex = "$nex",
    Lt = "$lt",
    Le = "$le",
    Ge = "$ge",
    Gt = "$gt",
    Add = "$add",
    Sub = "$sub",
    Mul = "$mul",
    Div = "$div",
    Mod = "$mod",
    DivFloor = "$divfloor",
    ModFloor = "$modfloor",
    Pow = "$pow",
});

/// An operand and whether it's extended as signed (`A_SIGNED`). RTLIL
/// keeps the flag per operand; for most operators both are the same.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Operand {
    pub sig: SigSpec,
    pub signed: bool,
}

impl Operand {
    fn decode(d: &mut Decoder, port: &str) -> Result<Operand, CellError> {
        let sig = d.port(port, Dir::In)?;
        let (signed, width) = match port {
            "A" => ("A_SIGNED", "A_WIDTH"),
            _ => ("B_SIGNED", "B_WIDTH"),
        };
        d.width(width, sig.len())?;
        Ok(Operand {
            signed: d.flag(signed)?,
            sig,
        })
    }
}

/// `Y = op(A)`, Y extended or truncated to its width.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Unary {
    pub op: UnaryOp,
    pub a: Operand,
    pub y: SigSpec,
}
ports!(Unary { "A" In a.sig, "Y" Out y });

impl Unary {
    pub(crate) fn decode(op: UnaryOp, d: &mut Decoder) -> Result<Unary, CellError> {
        let a = Operand::decode(d, "A")?;
        let y = d.port("Y", Dir::Out)?;
        d.width("Y_WIDTH", y.len())?;
        Ok(Unary { op, a, y })
    }

    pub(crate) fn params(&self, p: &mut Params) {
        p.flag("A_SIGNED", self.a.signed)
            .int("A_WIDTH", self.a.sig.len())
            .int("Y_WIDTH", self.y.len());
    }
}

/// `Y = A op B`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Binary {
    pub op: BinaryOp,
    pub a: Operand,
    pub b: Operand,
    pub y: SigSpec,
}
ports!(Binary { "A" In a.sig, "B" In b.sig, "Y" Out y });

impl Binary {
    pub(crate) fn decode(op: BinaryOp, d: &mut Decoder) -> Result<Binary, CellError> {
        let a = Operand::decode(d, "A")?;
        let b = Operand::decode(d, "B")?;
        let y = d.port("Y", Dir::Out)?;
        d.width("Y_WIDTH", y.len())?;
        Ok(Binary { op, a, b, y })
    }

    pub(crate) fn params(&self, p: &mut Params) {
        p.flag("A_SIGNED", self.a.signed)
            .flag("B_SIGNED", self.b.signed)
            .int("A_WIDTH", self.a.sig.len())
            .int("B_WIDTH", self.b.sig.len())
            .int("Y_WIDTH", self.y.len());
    }
}

/// `$mux`: `Y = S ? B : A`. A, B and Y are the same width.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Mux {
    pub a: SigSpec,
    pub b: SigSpec,
    pub s: SigBit,
    pub y: SigSpec,
}
ports!(Mux { "A" In a, "B" In b, "S" In s, "Y" Out y });

impl Mux {
    pub(crate) fn decode(d: &mut Decoder) -> Result<Mux, CellError> {
        let a = d.port("A", Dir::In)?;
        let w = a.len();
        d.width("WIDTH", w)?;
        Ok(Mux {
            a,
            b: d.port_w("B", Dir::In, w)?,
            s: d.bit("S", Dir::In)?,
            y: d.port_w("Y", Dir::Out, w)?,
        })
    }

    pub(crate) fn params(&self, p: &mut Params) {
        p.int("WIDTH", self.y.len());
    }

    pub(crate) fn check(&self) -> Result<(), String> {
        same_width(&[("A", &self.a), ("B", &self.b), ("Y", &self.y)])
    }
}

/// `$pmux`: Y is A, or the `i`th word of B if bit `i` of S is set (S is
/// one-hot or zero; more than one set bit gives x).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pmux {
    pub a: SigSpec,
    /// `s.len()` words of `a.len()` bits.
    pub b: SigSpec,
    pub s: SigSpec,
    pub y: SigSpec,
}
ports!(Pmux { "A" In a, "B" In b, "S" In s, "Y" Out y });

impl Pmux {
    pub(crate) fn decode(d: &mut Decoder) -> Result<Pmux, CellError> {
        let a = d.port("A", Dir::In)?;
        let s = d.port("S", Dir::In)?;
        d.width("WIDTH", a.len())?;
        d.width("S_WIDTH", s.len())?;
        Ok(Pmux {
            b: d.port_w("B", Dir::In, a.len() * s.len())?,
            y: d.port_w("Y", Dir::Out, a.len())?,
            a,
            s,
        })
    }

    pub(crate) fn params(&self, p: &mut Params) {
        p.int("WIDTH", self.a.len()).int("S_WIDTH", self.s.len());
    }

    pub(crate) fn check(&self) -> Result<(), String> {
        same_width(&[("A", &self.a), ("Y", &self.y)])?;
        expect_width("B", &self.b, self.a.len() * self.s.len())
    }
}

/// `$bmux`: Y is the S'th word of A.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Bmux {
    /// `2^s.len()` words of `y.len()` bits.
    pub a: SigSpec,
    pub s: SigSpec,
    pub y: SigSpec,
}
ports!(Bmux { "A" In a, "S" In s, "Y" Out y });

impl Bmux {
    pub(crate) fn decode(d: &mut Decoder) -> Result<Bmux, CellError> {
        let s = d.port("S", Dir::In)?;
        let y = d.port("Y", Dir::Out)?;
        d.width("WIDTH", y.len())?;
        d.width("S_WIDTH", s.len())?;
        Ok(Bmux {
            a: d.port_w("A", Dir::In, y.len() << s.len())?,
            s,
            y,
        })
    }

    pub(crate) fn params(&self, p: &mut Params) {
        p.int("WIDTH", self.y.len()).int("S_WIDTH", self.s.len());
    }

    pub(crate) fn check(&self) -> Result<(), String> {
        expect_width("A", &self.a, self.y.len() << self.s.len())
    }
}

/// `$demux`: the S'th word of Y is A, the others 0.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Demux {
    pub a: SigSpec,
    pub s: SigSpec,
    /// `2^s.len()` words of `a.len()` bits.
    pub y: SigSpec,
}
ports!(Demux { "A" In a, "S" In s, "Y" Out y });

impl Demux {
    pub(crate) fn decode(d: &mut Decoder) -> Result<Demux, CellError> {
        let a = d.port("A", Dir::In)?;
        let s = d.port("S", Dir::In)?;
        d.width("WIDTH", a.len())?;
        d.width("S_WIDTH", s.len())?;
        Ok(Demux {
            y: d.port_w("Y", Dir::Out, a.len() << s.len())?,
            a,
            s,
        })
    }

    pub(crate) fn params(&self, p: &mut Params) {
        p.int("WIDTH", self.a.len()).int("S_WIDTH", self.s.len());
    }

    pub(crate) fn check(&self) -> Result<(), String> {
        expect_width("Y", &self.y, self.a.len() << self.s.len())
    }
}

/// `$tribuf`: `Y = EN ? A : z`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Tribuf {
    pub a: SigSpec,
    pub en: SigBit,
    pub y: SigSpec,
}
ports!(Tribuf { "A" In a, "EN" In en, "Y" Out y });

impl Tribuf {
    pub(crate) fn decode(d: &mut Decoder) -> Result<Tribuf, CellError> {
        let a = d.port("A", Dir::In)?;
        d.width("WIDTH", a.len())?;
        Ok(Tribuf {
            en: d.bit("EN", Dir::In)?,
            y: d.port_w("Y", Dir::Out, a.len())?,
            a,
        })
    }

    pub(crate) fn params(&self, p: &mut Params) {
        p.int("WIDTH", self.a.len());
    }

    pub(crate) fn check(&self) -> Result<(), String> {
        same_width(&[("A", &self.a), ("Y", &self.y)])
    }
}

/// `$alu`, what `alumacc` turns `$add`/`$sub`/comparisons into:
/// `Y = A + (BI ? ~B : B) + CI` at Y's width, with `X = A ^ B'` and `CO`
/// each bit's carry out.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Alu {
    pub a: Operand,
    pub b: Operand,
    pub ci: SigBit,
    pub bi: SigBit,
    pub x: SigSpec,
    pub y: SigSpec,
    pub co: SigSpec,
}
ports!(Alu { "A" In a.sig, "B" In b.sig, "CI" In ci, "BI" In bi, "X" Out x, "Y" Out y, "CO" Out co });

impl Alu {
    pub(crate) fn decode(d: &mut Decoder) -> Result<Alu, CellError> {
        let a = Operand::decode(d, "A")?;
        let b = Operand::decode(d, "B")?;
        let y = d.port("Y", Dir::Out)?;
        let w = y.len();
        d.width("Y_WIDTH", w)?;
        Ok(Alu {
            a,
            b,
            ci: d.bit("CI", Dir::In)?,
            bi: d.bit("BI", Dir::In)?,
            x: d.port_w("X", Dir::Out, w)?,
            co: d.port_w("CO", Dir::Out, w)?,
            y,
        })
    }

    pub(crate) fn params(&self, p: &mut Params) {
        p.flag("A_SIGNED", self.a.signed)
            .flag("B_SIGNED", self.b.signed)
            .int("A_WIDTH", self.a.sig.len())
            .int("B_WIDTH", self.b.sig.len())
            .int("Y_WIDTH", self.y.len());
    }

    pub(crate) fn check(&self) -> Result<(), String> {
        same_width(&[("Y", &self.y), ("X", &self.x), ("CO", &self.co)])
    }
}

/// `$lut`: Y is bit `A` of the truth table.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Lut {
    pub a: SigSpec,
    pub y: SigBit,
    /// `2^a.len()` bits, bit `i` the output for input value `i`.
    pub table: Const,
}
ports!(Lut { "A" In a, "Y" Out y });

impl Lut {
    pub(crate) fn decode(d: &mut Decoder) -> Result<Lut, CellError> {
        let a = d.port("A", Dir::In)?;
        d.width("WIDTH", a.len())?;
        Ok(Lut {
            table: d.konst_w("LUT", 1 << a.len())?,
            y: d.bit("Y", Dir::Out)?,
            a,
        })
    }

    pub(crate) fn params(&self, p: &mut Params) {
        p.int("WIDTH", self.a.len())
            .konst("LUT", self.table.clone());
    }

    pub(crate) fn check(&self) -> Result<(), String> {
        if self.table.len() != 1 << self.a.len() {
            return Err(format!(
                "LUT is {} bits for {} inputs",
                self.table.len(),
                self.a.len()
            ));
        }
        Ok(())
    }
}

pub(crate) fn same_width(ports: &[(&str, &SigSpec)]) -> Result<(), String> {
    let (first, sig) = ports[0];
    ports[1..]
        .iter()
        .try_for_each(|(name, s)| expect_width(name, s, sig.len()))
        .map_err(|e| format!("{e} (as {first})"))
}

pub(crate) fn expect_width(name: &str, sig: &[SigBit], width: usize) -> Result<(), String> {
    if sig.len() != width {
        return Err(format!("{name} is {} bits, expected {width}", sig.len()));
    }
    Ok(())
}
