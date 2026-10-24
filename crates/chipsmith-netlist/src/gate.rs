//! Single-bit logic gates, RTLIL's `$_AND_` family. They have no
//! parameters; every port is one bit.

use crate::comb::op_names;
use crate::ports::{CellError, Decoder, Dir, Ports};
use crate::sig::SigBit;

op_names!(GateOp {
    Buf = "$_BUF_",
    Not = "$_NOT_",
    And = "$_AND_",
    Nand = "$_NAND_",
    Or = "$_OR_",
    Nor = "$_NOR_",
    Xor = "$_XOR_",
    Xnor = "$_XNOR_",
    AndNot = "$_ANDNOT_",
    OrNot = "$_ORNOT_",
    Mux = "$_MUX_",
    Nmux = "$_NMUX_",
    Aoi3 = "$_AOI3_",
    Oai3 = "$_OAI3_",
    Aoi4 = "$_AOI4_",
    Oai4 = "$_OAI4_",
    Tbuf = "$_TBUF_",
});

impl GateOp {
    /// Input port names, in the order of [`Gate::inputs`].
    pub fn input_names(self) -> &'static [&'static str] {
        match self {
            GateOp::Buf | GateOp::Not => &["A"],
            GateOp::Mux | GateOp::Nmux => &["A", "B", "S"],
            GateOp::Aoi3 | GateOp::Oai3 => &["A", "B", "C"],
            GateOp::Aoi4 | GateOp::Oai4 => &["A", "B", "C", "D"],
            GateOp::Tbuf => &["A", "E"],
            _ => &["A", "B"],
        }
    }

    /// The output for these inputs (in `input_names` order); `None` for
    /// `$_TBUF_` disabled (z).
    pub fn eval(self, i: &[bool]) -> Option<bool> {
        Some(match self {
            GateOp::Buf => i[0],
            GateOp::Not => !i[0],
            GateOp::And => i[0] & i[1],
            GateOp::Nand => !(i[0] & i[1]),
            GateOp::Or => i[0] | i[1],
            GateOp::Nor => !(i[0] | i[1]),
            GateOp::Xor => i[0] ^ i[1],
            GateOp::Xnor => !(i[0] ^ i[1]),
            GateOp::AndNot => i[0] & !i[1],
            GateOp::OrNot => i[0] | !i[1],
            GateOp::Mux => {
                if i[2] {
                    i[1]
                } else {
                    i[0]
                }
            }
            GateOp::Nmux => !(if i[2] { i[1] } else { i[0] }),
            GateOp::Aoi3 => !((i[0] & i[1]) | i[2]),
            GateOp::Oai3 => !((i[0] | i[1]) & i[2]),
            GateOp::Aoi4 => !((i[0] & i[1]) | (i[2] & i[3])),
            GateOp::Oai4 => !((i[0] | i[1]) & (i[2] | i[3])),
            GateOp::Tbuf => return i[1].then_some(i[0]),
        })
    }
}

/// A gate. Its inputs are an array sized for the operator, so a gate can't
/// lack an input or have one too many.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Gate {
    op: GateOp,
    inputs: [SigBit; 4],
    pub y: SigBit,
}

impl Gate {
    /// `inputs` in [`GateOp::input_names`] order; `None` if there are the
    /// wrong number.
    pub fn new(op: GateOp, inputs: &[SigBit], y: SigBit) -> Option<Gate> {
        if inputs.len() != op.input_names().len() {
            return None;
        }
        let mut all = [SigBit::X; 4];
        all[..inputs.len()].copy_from_slice(inputs);
        Some(Gate { op, inputs: all, y })
    }

    pub fn op(&self) -> GateOp {
        self.op
    }

    pub fn inputs(&self) -> &[SigBit] {
        &self.inputs[..self.op.input_names().len()]
    }

    pub fn inputs_mut(&mut self) -> &mut [SigBit] {
        let n = self.op.input_names().len();
        &mut self.inputs[..n]
    }

    pub(crate) fn decode(op: GateOp, d: &mut Decoder) -> Result<Gate, CellError> {
        let mut inputs = [SigBit::X; 4];
        for (slot, name) in inputs.iter_mut().zip(op.input_names()) {
            *slot = d.bit(name, Dir::In)?;
        }
        Ok(Gate {
            op,
            inputs,
            y: d.bit("Y", Dir::Out)?,
        })
    }
}

impl Ports for Gate {
    fn for_each_port<'a>(&'a self, f: &mut dyn FnMut(&'a str, Dir, &'a [SigBit])) {
        for (name, bit) in self.op.input_names().iter().zip(&self.inputs) {
            f(name, Dir::In, std::slice::from_ref(bit));
        }
        f("Y", Dir::Out, std::slice::from_ref(&self.y));
    }

    fn for_each_bit_mut(&mut self, f: &mut dyn FnMut(&mut SigBit)) {
        self.inputs_mut().iter_mut().for_each(&mut *f);
        f(&mut self.y);
    }
}
