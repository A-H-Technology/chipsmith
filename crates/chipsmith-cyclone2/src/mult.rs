//! Cyclone II embedded multipliers: the `CY2_MULT` cell synthesis makes
//! and the physical pins of the chip database's `MULT` bel.
//!
//! The EP2C35 has 35, one per LAB row in column X39 (cyclone2-re README
//! "Embedded multipliers"). A block is one 18 x 18 multiplier or two 9 x 9
//! ones; the open flow uses one multiplier per block, nine-bit mode when
//! both operands fit 9 bits, as Quartus does for a lone 9 x 9.
//!
//! The cell's operands are LSB-aligned (`A[A_WIDTH-1:0]`) and
//! `DATAOUT[A_WIDTH+B_WIDTH-1:0]` is their product, each operand signed or
//! not by a parameter. The packer puts an operand on the top pins of its
//! row (MSB-aligned, as Quartus does), so a narrow signed operand needs no
//! sign extension: the unused low pins are tied to 0 and the product's low
//! bits are 0. The product bits then come out shifted by the unused pins.

use std::fmt;

use anyhow::{ensure, Result};

/// Blocks on the EP2C35.
pub const BLOCKS: usize = 35;
/// Operand width of one block.
pub const MAX_WIDTH: usize = 18;
/// Operand width of one nine-bit row.
pub const NINE: usize = 9;

/// One operand.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Operand {
    A,
    B,
}

/// What a `CY2_MULT` computes: operand widths (1-18) and signedness.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Shape {
    widths: [u8; 2],
    pub signed: [bool; 2],
}

impl Shape {
    pub fn new(a: usize, b: usize, signed: [bool; 2]) -> Result<Shape> {
        for w in [a, b] {
            ensure!(
                (1..=MAX_WIDTH).contains(&w),
                "multiplier operand of {w} bits (1 to {MAX_WIDTH})"
            );
        }
        Ok(Shape {
            widths: [a as u8, b as u8],
            signed,
        })
    }

    pub fn width(self, op: Operand) -> usize {
        self.widths[op as usize] as usize
    }

    /// Both operands fit a nine-bit row.
    pub fn nine_bit(self) -> bool {
        self.widths.iter().all(|&w| w as usize <= NINE)
    }

    /// Bits of a row in this shape's mode.
    pub fn row(self) -> usize {
        if self.nine_bit() {
            NINE
        } else {
            MAX_WIDTH
        }
    }

    /// The physical pin of bit `i` of an operand: MSB-aligned in its row.
    pub fn pin(self, op: Operand, i: usize) -> Pin {
        debug_assert!(i < self.width(op));
        let k = (self.row() - self.width(op) + i) as u8;
        match op {
            Operand::A => Pin::A(k),
            Operand::B => Pin::B(k),
        }
    }

    /// Unused low pins of an operand's row, which read 0.
    pub fn pad(self, op: Operand) -> usize {
        self.row() - self.width(op)
    }

    /// The physical output of product bit `k`: shifted up by both operands'
    /// unused low pins.
    pub fn out(self, k: usize) -> Pin {
        Pin::Out((k + self.pad(Operand::A) + self.pad(Operand::B)) as u8)
    }
}

/// A physical pin of the `MULT` bel (cyclone2-re `chipdb.rs`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Pin {
    /// Operand A data input 0-17 (`MULTA<k>`, asmdb `MAC_ROW_DATA_INPUT` k).
    A(u8),
    /// Operand B data input 0-17 (`MULTB<k>`, `MAC_ROW_DATA_INPUT` 18 + k).
    B(u8),
    /// The signs (`SIGNA`, `SIGNB`): 1 = signed. Unconnected reads 1.
    Sign(Operand),
    /// Product bit 0-35 (`MULTOUT<k>`, on `DSP_BUFFER` k).
    Out(u8),
}

impl Pin {
    pub fn is_output(self) -> bool {
        matches!(self, Pin::Out(_))
    }

    pub fn parse(name: &str) -> Option<Pin> {
        let num = |prefix: &str| name.strip_prefix(prefix)?.parse::<u8>().ok();
        let pin = match name {
            "SIGNA" => Pin::Sign(Operand::A),
            "SIGNB" => Pin::Sign(Operand::B),
            _ => {
                if let Some(k) = num("MULTA") {
                    Pin::A(k)
                } else if let Some(k) = num("MULTB") {
                    Pin::B(k)
                } else {
                    Pin::Out(num("MULTOUT")?)
                }
            }
        };
        pin.exists().then_some(pin)
    }

    fn exists(self) -> bool {
        match self {
            Pin::A(k) | Pin::B(k) => (k as usize) < MAX_WIDTH,
            Pin::Out(k) => (k as usize) < 2 * MAX_WIDTH,
            Pin::Sign(_) => true,
        }
    }

    /// Every pin, in chip database order.
    pub fn all() -> Vec<Pin> {
        let mut v: Vec<Pin> = (0..MAX_WIDTH as u8).map(Pin::A).collect();
        v.extend((0..MAX_WIDTH as u8).map(Pin::B));
        v.extend([Pin::Sign(Operand::A), Pin::Sign(Operand::B)]);
        v.extend((0..2 * MAX_WIDTH as u8).map(Pin::Out));
        v
    }
}

impl fmt::Display for Pin {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            Pin::A(k) => write!(f, "MULTA{k}"),
            Pin::B(k) => write!(f, "MULTB{k}"),
            Pin::Sign(Operand::A) => f.write_str("SIGNA"),
            Pin::Sign(Operand::B) => f.write_str("SIGNB"),
            Pin::Out(k) => write!(f, "MULTOUT{k}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn operands_sit_on_the_top_pins_of_their_row() {
        // Quartus' 9 x 7 (specimens t1 m2): B on pins 2-8 of row 0, the
        // product on DSP_BUFFER 2-17.
        let s = Shape::new(9, 7, [true, false]).unwrap();
        assert!(s.nine_bit());
        assert_eq!(s.pin(Operand::B, 0), Pin::B(2));
        assert_eq!(s.out(0), Pin::Out(2));
        assert_eq!(s.out(15), Pin::Out(17));
        let s = Shape::new(18, 18, [false, false]).unwrap();
        assert_eq!(s.pin(Operand::A, 17), Pin::A(17));
        assert_eq!(s.out(35), Pin::Out(35));
        assert!(Shape::new(19, 1, [false, false]).is_err());
    }

    #[test]
    fn pin_names_round_trip() {
        for p in Pin::all() {
            assert_eq!(Pin::parse(&p.to_string()), Some(p));
        }
        for name in ["MULTA18", "MULTOUT36", "SIGNC", "MULTB"] {
            assert_eq!(Pin::parse(name), None, "{name}");
        }
    }
}
