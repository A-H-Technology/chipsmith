//! Cyclone II M4K RAM blocks: the shapes the open flow supports and the
//! block's physical pins.
//!
//! yosys makes memories into `CY2_M4K` cells (cyclone2-re `synth/m4k.txt`,
//! `synth/m4k_map.v`) with logical ports: `CLK`, `A_ADDR[11:0]`,
//! `A_DATAIN[35:0]`, `A_WE`, `B_ADDR[11:0]` and `DATAOUT[35:0]`. The packer
//! moves them onto the pins of the chip database's `M4K` bel (`Pin`), which
//! the placer and router see; cram reads the cell back by those pin names.

use std::fmt;

use anyhow::{bail, Result};

/// How the block's two ports are used.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// Port A reads and writes (a ROM if it never writes).
    Single,
    /// Port A writes, port B reads, on one clock.
    SimpleDual,
}

/// Data width of a port; the depth follows (4,608 bits, parity included).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Width {
    X1,
    X2,
    X4,
    X9,
    X18,
    X36,
}

impl Width {
    pub fn bits(self) -> u8 {
        match self {
            Width::X1 => 1,
            Width::X2 => 2,
            Width::X4 => 4,
            Width::X9 => 9,
            Width::X18 => 18,
            Width::X36 => 36,
        }
    }

    pub fn from_bits(bits: u64) -> Option<Width> {
        Some(match bits {
            1 => Width::X1,
            2 => Width::X2,
            4 => Width::X4,
            9 => Width::X9,
            18 => Width::X18,
            36 => Width::X36,
            _ => return None,
        })
    }

    /// Address bits: a port of this width uses physical address pins
    /// `12 - addr_bits ..= 11`.
    pub fn addr_bits(self) -> u8 {
        match self {
            Width::X1 => 12,
            Width::X2 => 11,
            Width::X4 => 10,
            Width::X9 => 9,
            Width::X18 => 8,
            Width::X36 => 7,
        }
    }
}

/// A supported mode and width. A single-port x36 is Quartus' packed mode,
/// which the open flow doesn't support, so it can't be built.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Shape {
    mode: Mode,
    width: Width,
}

impl Shape {
    pub fn new(mode: Mode, width: Width) -> Result<Shape> {
        if mode == Mode::Single && width == Width::X36 {
            bail!("a x36 single port (Quartus' packed mode) is not supported");
        }
        Ok(Shape { mode, width })
    }

    /// From the cell's `MODE` and `WIDTH` parameters.
    pub fn parse(mode: &str, width: u64) -> Result<Shape> {
        let m = match mode.trim_end() {
            "SINGLE" => Mode::Single,
            "SIMPLE_DUAL" => Mode::SimpleDual,
            _ => bail!("mode {mode} width {width} is not supported"),
        };
        let Some(w) = Width::from_bits(width) else {
            bail!("mode {mode} width {width} is not supported");
        };
        Shape::new(m, w)
    }

    pub fn mode(self) -> Mode {
        self.mode
    }

    pub fn width(self) -> Width {
        self.width
    }

    pub fn mode_name(self) -> &'static str {
        match self.mode {
            Mode::Single => "SINGLE",
            Mode::SimpleDual => "SIMPLE_DUAL",
        }
    }
}

/// Which of the cell's two address buses.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum AddrPort {
    A,
    B,
}

/// A physical pin of the M4K bel (cyclone2-re `chipdb.rs` `m4k_tile`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Pin {
    /// Data input 0-35 (`DI<k>`).
    Di(u8),
    /// Write address 0-6 and 11 (`WA<k>`).
    Wa(u8),
    /// Read address 0-6 and 9-11 (`RA<k>`).
    Ra(u8),
    /// Clock 0, through a control input or a row clock.
    Clk0,
    /// Write enable.
    We,
    /// Data output 0-35 (`DO<k>`): column k of the read word.
    Do(u8),
}

impl Pin {
    /// Physical pin of address bit `p` (0-11) of port A or B. Some of the
    /// block's low address bits enter on data input pins (cyclone2-re
    /// `m4k.rs` `Input::addr`, by fuzzing).
    pub fn addr(port: AddrPort, p: u8) -> Pin {
        debug_assert!(p < 12);
        match port {
            AddrPort::A => match p {
                0..=3 => Pin::Di(12 + p),
                4 => Pin::Wa(11),
                _ => Pin::Wa(p - 5),
            },
            AddrPort::B => match p {
                0 => Pin::Di(26),
                1 => Pin::Di(35),
                2..=4 => Pin::Ra(7 + p),
                _ => Pin::Ra(p - 5),
            },
        }
    }

    pub fn is_output(self) -> bool {
        matches!(self, Pin::Do(_))
    }

    pub fn parse(name: &str) -> Option<Pin> {
        let num = |prefix: &str| name.strip_prefix(prefix)?.parse::<u8>().ok();
        let pin = match name {
            "CLK0" => Pin::Clk0,
            "WE" => Pin::We,
            _ => {
                if let Some(k) = num("DI") {
                    Pin::Di(k)
                } else if let Some(k) = num("WA") {
                    Pin::Wa(k)
                } else if let Some(k) = num("RA") {
                    Pin::Ra(k)
                } else {
                    Pin::Do(num("DO")?)
                }
            }
        };
        pin.exists().then_some(pin)
    }

    fn exists(self) -> bool {
        match self {
            Pin::Di(k) | Pin::Do(k) => k < 36,
            Pin::Wa(k) => k < 7 || k == 11,
            Pin::Ra(k) => k < 7 || (9..=11).contains(&k),
            Pin::Clk0 | Pin::We => true,
        }
    }
}

impl fmt::Display for Pin {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            Pin::Di(k) => write!(f, "DI{k}"),
            Pin::Wa(k) => write!(f, "WA{k}"),
            Pin::Ra(k) => write!(f, "RA{k}"),
            Pin::Clk0 => f.write_str("CLK0"),
            Pin::We => f.write_str("WE"),
            Pin::Do(k) => write!(f, "DO{k}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn address_pins_follow_the_uarch() {
        // nextpnr/cyclone2 `m4k_addr_pin`.
        let a: Vec<String> = (0..12)
            .map(|p| Pin::addr(AddrPort::A, p).to_string())
            .collect();
        assert_eq!(
            a,
            [
                "DI12", "DI13", "DI14", "DI15", "WA11", "WA0", "WA1", "WA2", "WA3", "WA4", "WA5",
                "WA6"
            ]
        );
        let b: Vec<String> = (0..12)
            .map(|p| Pin::addr(AddrPort::B, p).to_string())
            .collect();
        assert_eq!(
            b,
            [
                "DI26", "DI35", "RA9", "RA10", "RA11", "RA0", "RA1", "RA2", "RA3", "RA4", "RA5",
                "RA6"
            ]
        );
    }

    #[test]
    fn pin_names_round_trip_and_reject_missing_pins() {
        for name in ["DI0", "DI35", "WA6", "WA11", "RA9", "CLK0", "WE", "DO35"] {
            assert_eq!(Pin::parse(name).unwrap().to_string(), name);
        }
        for name in ["DI36", "WA7", "RA8", "DO", "CLK1"] {
            assert_eq!(Pin::parse(name), None, "{name}");
        }
    }

    #[test]
    fn a_single_port_x36_is_refused() {
        assert!(Shape::parse("SINGLE", 36).is_err());
        assert!(Shape::parse("SIMPLE_DUAL ", 36).is_ok());
        assert!(Shape::parse("SINGLE", 8).is_err());
    }
}
