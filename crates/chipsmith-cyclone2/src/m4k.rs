//! Cyclone II M4K RAM blocks: how one is configured ([`Shape`]), the cell
//! synthesis makes of it (`CY2_M4K`, [`CellPort`] and the parameters
//! [`Shape::params`] writes), and the block's physical pins ([`Pin`]).
//!
//! The cell's ports are the block's logical ports: `CLK` (clock 0), `CLK1`,
//! `ENA0`, `ENA1`, `CLR0`, `CLR1`, and per port (A, B) an address
//! (`A_ADDR[11:0]`, MSB-aligned: an n-bit address on bits `12 - n ..= 11`),
//! data in by physical column (`A_DATAIN[35:0]`, `B_DATAIN[17:0]`), a write
//! enable and byte enables, and `DATAOUT[35:0]`, the block's 36 read columns
//! (port A's word from column 0, port B's in true dual port from 18). The
//! packer moves them onto the pins of the chip database's `M4K` bel, which
//! the placer and router see; the bitstream (`bitstream::nextpnr`) reads the
//! cell back by those pin names.
//!
//! Cells from the yosys flow (cyclone2-re `synth/m4k_map.v`) are the subset
//! `CLK`, `A_ADDR`, `A_DATAIN`, `A_WE`, `B_ADDR`, `DATAOUT` with `MODE`,
//! `WIDTH` and `INIT`, and read the same.

use std::collections::BTreeSet;
use std::fmt;

use anyhow::{bail, ensure, Context, Result};

use crate::bitstream::m4k::Mode;
pub use crate::bitstream::m4k::{OutReg, ReadPort, Sel, Width, BITS};

/// One of the block's two ports.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Side {
    A,
    B,
}

/// How the block's ports are used. A port reads and writes unless it is
/// the write side of a simple dual port; whether it writes is whether its
/// write enable is driven (a ROM never writes).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ports {
    /// Port A, alone. Reading the word being written returns the new data
    /// (flow-through).
    Single { a: ReadPort },
    /// Port A writes, port B reads (its input registers on `read_clock`);
    /// reading the word being written returns the old data. Without a
    /// write, a x36 ROM, which Quartus builds as a read-only port B.
    SimpleDual {
        write: Width,
        read: ReadPort,
        read_clock: Sel,
    },
    /// Both ports read and write. Each sees its own write (new data) and
    /// reads the old data of a word the other port writes. Port B's input
    /// registers on `b_clock`: clock 1 only while port A never writes,
    /// which is how Quartus builds a simple dual port on two clocks (A
    /// reads on clock 0, B writes on clock 1; Quartus 13.0 rejects both a
    /// simple dual port and a writing true dual port on two clocks).
    TrueDual {
        a: ReadPort,
        b: ReadPort,
        b_clock: Sel,
    },
}

/// How a base word's bits sit in a x9/x18/x36 base word's columns.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Packing {
    /// Bit k in column k.
    Dense,
    /// Eight bits per nine columns, each byte under its own byte enable:
    /// bit k in column `9 * (k / 8) + k % 8`.
    Bytes,
}

/// A block's configuration: its ports, and where a memory's bits sit in it.
///
/// The memory's words map to *base words* of physical width `base`, each
/// holding `slice` data bits (`packing` says in which columns). A port of
/// width P sees `depth(base) / depth(P)` consecutive base words as one of its
/// words, so ports of different widths (mixed widths) agree on where every
/// bit is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Shape {
    ports: Ports,
    base: Width,
    slice: u8,
    packing: Packing,
}

/// Words of a port of this width (x1, x2, x4 skip the ninth columns).
pub fn depth(w: Width) -> usize {
    if w.bits() < 9 {
        4096 / w.bits()
    } else {
        BITS / w.bits()
    }
}

fn out_reg_name(r: Option<OutReg>) -> (&'static str, &'static str) {
    match r {
        None => ("NONE", "NONE"),
        Some(OutReg { clock, clear }) => (
            match clock {
                Sel::Zero => "CLOCK0",
                Sel::One => "CLOCK1",
            },
            match clear {
                None => "NONE",
                Some(Sel::Zero) => "CLEAR0",
                Some(Sel::One) => "CLEAR1",
            },
        ),
    }
}

fn parse_out_reg(reg: Option<&str>, clr: Option<&str>) -> Result<Option<OutReg>> {
    let clear = match clr.map(str::trim) {
        None | Some("NONE") => None,
        Some("CLEAR0") => Some(Sel::Zero),
        Some("CLEAR1") => Some(Sel::One),
        Some(c) => bail!("output clear {c}"),
    };
    let clock = match reg.map(str::trim) {
        None | Some("NONE") => {
            ensure!(
                clear.is_none(),
                "an output clear without an output register"
            );
            return Ok(None);
        }
        Some("CLOCK0") => Sel::Zero,
        Some("CLOCK1") => Sel::One,
        Some(c) => bail!("output register clock {c}"),
    };
    Ok(Some(OutReg { clock, clear }))
}

/// A cell parameter, as [`Shape::params`] writes it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Param {
    Str(&'static str),
    Int(u64),
}

impl Shape {
    pub fn new(ports: Ports, base: Width, slice: u8, packing: Packing) -> Result<Shape> {
        match ports {
            Ports::Single { a } => ensure!(
                a.width <= Width::X18,
                "a x36 single port (Quartus' packed mode) is not supported"
            ),
            Ports::TrueDual { a, b, .. } => ensure!(
                a.width <= Width::X18 && b.width <= Width::X18,
                "a true dual port wider than x18"
            ),
            Ports::SimpleDual { .. } => {}
        }
        let cap = match packing {
            Packing::Dense => base.bits(),
            Packing::Bytes => {
                ensure!(base >= Width::X9, "byte packing in a x{} base", base.bits());
                base.bits() / 9 * 8
            }
        };
        ensure!(
            slice >= 1 && slice as usize <= cap,
            "{slice} bits in a x{} base word",
            base.bits()
        );
        let s = Shape {
            ports,
            base,
            slice,
            packing,
        };
        for (_, w) in s.widths() {
            ensure!(
                depth(w) <= depth(base),
                "a x{} port over x{} base words",
                w.bits(),
                base.bits()
            );
        }
        // Some address pins are data pins of the other width: a port that
        // needs one can't be paired with a port writing there.
        let mut seen = BTreeSet::new();
        for pin in s.input_pins() {
            ensure!(seen.insert(pin), "pin {pin} used twice");
        }
        Ok(s)
    }

    /// The shape of cells from the yosys flow: one width, bit k in column
    /// k, `bits` of them.
    pub fn legacy(ports: Ports, bits: u8) -> Result<Shape> {
        let base = match ports {
            Ports::Single { a } => a.width,
            Ports::SimpleDual { read, .. } => read.width,
            Ports::TrueDual { a, .. } => a.width,
        };
        Shape::new(ports, base, bits, Packing::Dense)
    }

    pub fn ports(self) -> Ports {
        self.ports
    }
    pub fn base(self) -> Width {
        self.base
    }
    pub fn slice(self) -> u8 {
        self.slice
    }
    pub fn packing(self) -> Packing {
        self.packing
    }

    /// Each port's width, port A first; a simple dual port's write side is
    /// port A.
    pub fn widths(self) -> Vec<(Side, Width)> {
        match self.ports {
            Ports::Single { a } => vec![(Side::A, a.width)],
            Ports::SimpleDual { write, read, .. } => vec![(Side::A, write), (Side::B, read.width)],
            Ports::TrueDual { a, b, .. } => vec![(Side::A, a.width), (Side::B, b.width)],
        }
    }

    pub fn width(self, side: Side) -> Option<Width> {
        self.widths()
            .into_iter()
            .find(|&(s, _)| s == side)
            .map(|(_, w)| w)
    }

    /// The ports that read, and their output registers.
    pub fn read_ports(self) -> Vec<(Side, ReadPort)> {
        match self.ports {
            Ports::Single { a } => vec![(Side::A, a)],
            Ports::SimpleDual { read, .. } => vec![(Side::B, read)],
            Ports::TrueDual { a, b, .. } => vec![(Side::A, a), (Side::B, b)],
        }
    }

    /// Whether this port can write.
    pub fn writable(self, side: Side) -> bool {
        match self.ports {
            Ports::Single { .. } => side == Side::A,
            Ports::SimpleDual { .. } => side == Side::A,
            Ports::TrueDual { .. } => true,
        }
    }

    /// Clock of a port's input (address, data, enable) registers.
    pub fn input_clock(self, side: Side) -> Sel {
        match (self.ports, side) {
            (Ports::SimpleDual { read_clock, .. }, Side::B) => read_clock,
            (Ports::TrueDual { b_clock, .. }, Side::B) => b_clock,
            _ => Sel::Zero,
        }
    }

    /// Base words per word of a port of width `w`.
    pub fn ratio(self, w: Width) -> usize {
        depth(self.base) / depth(w)
    }

    /// Data bits a port of width `w` carries.
    pub fn data_bits(self, w: Width) -> usize {
        self.ratio(w) * self.slice as usize
    }

    fn base_column(self, bit: usize) -> usize {
        match self.packing {
            Packing::Dense => bit,
            Packing::Bytes => 9 * (bit / 8) + bit % 8,
        }
    }

    /// Linear position (in the block's 36-column rows) of data bit `k` of
    /// word `word` of a port of width `w`.
    pub fn linear(self, w: Width, word: usize, k: usize) -> usize {
        let s = self.slice as usize;
        let base_word = word * self.ratio(w) + k / s;
        self.base.linear(base_word, self.base_column(k % s))
    }

    /// Column of a port's word that data bit `k` is in.
    pub fn column(self, w: Width, k: usize) -> usize {
        w.column(self.linear(w, 0, k))
    }

    /// Whether linear position `l` holds a data bit (the rest read 0).
    pub fn holds(self, l: usize) -> bool {
        let w = self.base;
        let col = w.column(l);
        if w.bits() < 9 && l % 9 == 8 {
            return false;
        }
        match self.packing {
            Packing::Dense => col < self.slice as usize,
            Packing::Bytes => col % 9 != 8 && col / 9 * 8 + col % 9 < self.slice as usize,
        }
    }

    /// Physical address pins of a port: its address bits on the top ones.
    pub fn addr_pins(self, side: Side) -> Vec<Pin> {
        let Some(w) = self.width(side) else {
            return Vec::new();
        };
        let n = w.addr_bits() as u8;
        (12 - n..12).map(|p| Pin::addr(side, p)).collect()
    }

    /// Data input pin of column `col` of a port's word.
    pub fn data_pin(side: Side, col: usize) -> Pin {
        match side {
            Side::A => Pin::Di(col as u8),
            Side::B => Pin::Di(18 + col as u8),
        }
    }

    /// Input pins a write (data, addresses) can use: what may not collide.
    fn input_pins(self) -> Vec<Pin> {
        let mut v = Vec::new();
        for (side, w) in self.widths() {
            v.extend(self.addr_pins(side));
            if self.writable(side) {
                for k in 0..self.data_bits(w) {
                    v.push(Shape::data_pin(side, self.column(w, k)));
                }
            }
        }
        v
    }

    /// The bitstream's view, given which ports write.
    pub fn mode(self, a_writes: bool) -> Result<Mode> {
        Ok(match self.ports {
            Ports::Single { a } => Mode::Single { a },
            Ports::SimpleDual {
                write,
                read,
                read_clock,
            } => {
                ensure!(
                    a_writes || read.width == Width::X36,
                    "a read-only simple dual port narrower than x36 is not supported"
                );
                Mode::SimpleDual {
                    write: a_writes.then_some(write),
                    read,
                    read_clock,
                }
            }
            Ports::TrueDual { a, b, b_clock } => {
                ensure!(
                    b_clock == Sel::Zero || !a_writes,
                    "a true dual port on two clocks whose port A writes"
                );
                Mode::true_dual(a, b, b_clock)?
            }
        })
    }

    /// The cell's parameters (but `INIT`), as synthesis writes them.
    /// `B_WIDTH`, `BASE`, `SLICE`, `PACKING` and the output registers are
    /// left out where the yosys flow's cells imply them.
    pub fn params(self) -> Vec<(&'static str, Param)> {
        let (mode, a, b) = match self.ports {
            Ports::Single { a } => ("SINGLE", a.width, None),
            Ports::SimpleDual { write, read, .. } => ("SIMPLE_DUAL", write, Some(read.width)),
            Ports::TrueDual { a, b, .. } => ("TRUE_DUAL", a.width, Some(b.width)),
        };
        let mut v = vec![
            ("MODE", Param::Str(mode)),
            ("WIDTH", Param::Int(a.bits() as u64)),
        ];
        if let Some(b) = b.filter(|&b| b != a) {
            v.push(("B_WIDTH", Param::Int(b.bits() as u64)));
        }
        if self.input_clock(Side::B) == Sel::One {
            v.push(("B_CLOCK", Param::Int(1)));
        }
        for (side, r) in self.read_ports() {
            if r.out_reg.is_some() {
                let (reg, clr) = out_reg_name(r.out_reg);
                let (kr, kc) = match side {
                    Side::A => ("A_OUTREG", "A_OUTCLR"),
                    Side::B => ("B_OUTREG", "B_OUTCLR"),
                };
                v.push((kr, Param::Str(reg)));
                if clr != "NONE" {
                    v.push((kc, Param::Str(clr)));
                }
            }
        }
        let legacy_base = match self.ports {
            Ports::Single { a } => a.width,
            Ports::SimpleDual { read, .. } => read.width,
            Ports::TrueDual { a, .. } => a.width,
        };
        if self.base != legacy_base || self.packing != Packing::Dense {
            v.push(("BASE", Param::Int(self.base.bits() as u64)));
        }
        if self.packing == Packing::Bytes {
            v.push(("PACKING", Param::Str("BYTES")));
        }
        v.push(("SLICE", Param::Int(self.slice as u64)));
        v
    }

    /// From the cell's parameters. `slice` is `SLICE`, or for a cell from
    /// the yosys flow the data bits its pins use.
    pub fn parse(
        str_param: impl Fn(&str) -> Option<String>,
        int_param: impl Fn(&str) -> Option<u64>,
        legacy_bits: impl FnOnce(Width) -> u8,
    ) -> Result<Shape> {
        let mode = str_param("MODE").context("MODE missing")?;
        let width = |k: &str| -> Result<Option<Width>> {
            int_param(k)
                .map(|b| Width::from_bits(b).with_context(|| format!("{k} {b}")))
                .transpose()
        };
        let a = width("WIDTH")?.context("WIDTH missing")?;
        let b = width("B_WIDTH")?.unwrap_or(a);
        let out = |side: &str| {
            parse_out_reg(
                str_param(&format!("{side}_OUTREG")).as_deref(),
                str_param(&format!("{side}_OUTCLR")).as_deref(),
            )
        };
        let read_clock = match int_param("B_CLOCK") {
            None | Some(0) => Sel::Zero,
            Some(1) => Sel::One,
            Some(c) => bail!("B_CLOCK {c}"),
        };
        let ports = match mode.trim() {
            "SINGLE" => {
                ensure!(out("B")?.is_none(), "a single port has no port B");
                Ports::Single {
                    a: ReadPort {
                        width: a,
                        out_reg: out("A")?,
                    },
                }
            }
            "SIMPLE_DUAL" => {
                ensure!(
                    out("A")?.is_none(),
                    "a simple dual port's write side has no output register"
                );
                Ports::SimpleDual {
                    write: a,
                    read: ReadPort {
                        width: b,
                        out_reg: out("B")?,
                    },
                    read_clock,
                }
            }
            "TRUE_DUAL" => Ports::TrueDual {
                b_clock: read_clock,
                a: ReadPort {
                    width: a,
                    out_reg: out("A")?,
                },
                b: ReadPort {
                    width: b,
                    out_reg: out("B")?,
                },
            },
            m => bail!("mode {m} is not supported"),
        };
        let packing = match str_param("PACKING").as_deref().map(str::trim) {
            None | Some("DENSE") => Packing::Dense,
            Some("BYTES") => Packing::Bytes,
            Some(p) => bail!("PACKING {p}"),
        };
        match (int_param("SLICE"), width("BASE")?) {
            (Some(slice), base) => {
                let base = base.unwrap_or(match ports {
                    Ports::Single { a } => a.width,
                    Ports::SimpleDual { read, .. } => read.width,
                    Ports::TrueDual { a, .. } => a.width,
                });
                let slice = u8::try_from(slice).context("SLICE")?;
                Shape::new(ports, base, slice, packing)
            }
            (None, None) => {
                ensure!(a == b, "mixed widths need SLICE");
                Shape::legacy(ports, legacy_bits(a))
            }
            (None, Some(_)) => bail!("BASE without SLICE"),
        }
    }
}

/// A logical port of the `CY2_M4K` cell.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum CellPort {
    /// `CLK` (clock 0) and `CLK1`.
    Clk(Sel),
    /// `ENA0`, `ENA1`: clock enables of everything on that clock.
    Ena(Sel),
    /// `CLR0`, `CLR1`: asynchronous clears, of output registers only.
    Clr(Sel),
    /// Address bit 0-11 (MSB-aligned).
    Addr(Side, u8),
    /// Data in by column: 0-35 on port A, 0-17 on port B.
    DataIn(Side, u8),
    We(Side),
    /// Byte enable j covers columns 9j..9j+8 of the port's word: 0-3 on
    /// port A, 0-1 on port B.
    ByteEna(Side, u8),
    /// Read column 0-35.
    DataOut(u8),
}

impl CellPort {
    pub fn parse(name: &str) -> Option<CellPort> {
        let p = match name {
            "CLK" => CellPort::Clk(Sel::Zero),
            "CLK1" => CellPort::Clk(Sel::One),
            "ENA0" => CellPort::Ena(Sel::Zero),
            "ENA1" => CellPort::Ena(Sel::One),
            "CLR0" => CellPort::Clr(Sel::Zero),
            "CLR1" => CellPort::Clr(Sel::One),
            "A_WE" => CellPort::We(Side::A),
            "B_WE" => CellPort::We(Side::B),
            _ => {
                let (base, index) = name.strip_suffix(']')?.split_once('[')?;
                let i: u8 = index.parse().ok()?;
                match base {
                    "A_ADDR" => CellPort::Addr(Side::A, i),
                    "B_ADDR" => CellPort::Addr(Side::B, i),
                    "A_DATAIN" => CellPort::DataIn(Side::A, i),
                    "B_DATAIN" => CellPort::DataIn(Side::B, i),
                    "A_BYTEENA" => CellPort::ByteEna(Side::A, i),
                    "B_BYTEENA" => CellPort::ByteEna(Side::B, i),
                    "DATAOUT" => CellPort::DataOut(i),
                    _ => return None,
                }
            }
        };
        p.exists().then_some(p)
    }

    fn exists(self) -> bool {
        match self {
            CellPort::Addr(_, i) => i < 12,
            CellPort::DataIn(Side::A, i) | CellPort::DataOut(i) => i < 36,
            CellPort::DataIn(Side::B, i) => i < 18,
            CellPort::ByteEna(Side::A, i) => i < 4,
            CellPort::ByteEna(Side::B, i) => i < 2,
            _ => true,
        }
    }

    pub fn is_output(self) -> bool {
        matches!(self, CellPort::DataOut(_))
    }

    /// The bel pin it is on.
    pub fn pin(self) -> Pin {
        match self {
            CellPort::Clk(Sel::Zero) => Pin::Clk0,
            CellPort::Clk(Sel::One) => Pin::Clk1,
            CellPort::Ena(s) => Pin::Ena(s),
            CellPort::Clr(s) => Pin::Clr(s),
            CellPort::Addr(side, p) => Pin::addr(side, p),
            CellPort::DataIn(side, c) => Shape::data_pin(side, c as usize),
            CellPort::We(Side::A) => Pin::We,
            CellPort::We(Side::B) => Pin::Re,
            CellPort::ByteEna(Side::A, j) => Pin::Be(j),
            CellPort::ByteEna(Side::B, j) => Pin::Be(2 + j),
            CellPort::DataOut(c) => Pin::Do(c),
        }
    }
}

impl fmt::Display for CellPort {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        let side = |s: &Side| match s {
            Side::A => 'A',
            Side::B => 'B',
        };
        match self {
            CellPort::Clk(Sel::Zero) => f.write_str("CLK"),
            CellPort::Clk(Sel::One) => f.write_str("CLK1"),
            CellPort::Ena(s) => write!(f, "ENA{}", *s as u8),
            CellPort::Clr(s) => write!(f, "CLR{}", *s as u8),
            CellPort::Addr(s, i) => write!(f, "{}_ADDR[{i}]", side(s)),
            CellPort::DataIn(s, i) => write!(f, "{}_DATAIN[{i}]", side(s)),
            CellPort::We(s) => write!(f, "{}_WE", side(s)),
            CellPort::ByteEna(s, i) => write!(f, "{}_BYTEENA[{i}]", side(s)),
            CellPort::DataOut(i) => write!(f, "DATAOUT[{i}]"),
        }
    }
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
    /// Byte enable 0-3 (`BE<k>`; 2-3 are port B's 0-1).
    Be(u8),
    /// The control pins, behind fast muxes from a control input or a row
    /// clock.
    Clk0,
    Clk1,
    Ena(Sel),
    Clr(Sel),
    /// Port A write enable.
    We,
    /// Port B write (or read) enable.
    Re,
    /// Data output 0-35 (`DO<k>`): column k of the read word.
    Do(u8),
}

impl Pin {
    /// Physical pin of address bit `p` (0-11) of port A or B. Some of the
    /// block's low address bits enter on data input pins (cyclone2-re
    /// `m4k.rs` `Input::addr`, by fuzzing).
    pub fn addr(side: Side, p: u8) -> Pin {
        debug_assert!(p < 12);
        match side {
            Side::A => match p {
                0..=3 => Pin::Di(12 + p),
                4 => Pin::Wa(11),
                _ => Pin::Wa(p - 5),
            },
            Side::B => match p {
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

    /// A clock pin: through a clock control, like an LE's clock.
    pub fn is_clock(self) -> bool {
        matches!(self, Pin::Clk0 | Pin::Clk1)
    }

    pub fn parse(name: &str) -> Option<Pin> {
        let num = |prefix: &str| name.strip_prefix(prefix)?.parse::<u8>().ok();
        let pin = match name {
            "CLK0" => Pin::Clk0,
            "CLK1" => Pin::Clk1,
            "ENA0" => Pin::Ena(Sel::Zero),
            "ENA1" => Pin::Ena(Sel::One),
            "CLR0" => Pin::Clr(Sel::Zero),
            "CLR1" => Pin::Clr(Sel::One),
            "WE" => Pin::We,
            "RE" => Pin::Re,
            _ => {
                if let Some(k) = num("DI") {
                    Pin::Di(k)
                } else if let Some(k) = num("WA") {
                    Pin::Wa(k)
                } else if let Some(k) = num("RA") {
                    Pin::Ra(k)
                } else if let Some(k) = num("BE") {
                    Pin::Be(k)
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
            Pin::Be(k) => k < 4,
            _ => true,
        }
    }
}

impl fmt::Display for Pin {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            Pin::Di(k) => write!(f, "DI{k}"),
            Pin::Wa(k) => write!(f, "WA{k}"),
            Pin::Ra(k) => write!(f, "RA{k}"),
            Pin::Be(k) => write!(f, "BE{k}"),
            Pin::Clk0 => f.write_str("CLK0"),
            Pin::Clk1 => f.write_str("CLK1"),
            Pin::Ena(s) => write!(f, "ENA{}", *s as u8),
            Pin::Clr(s) => write!(f, "CLR{}", *s as u8),
            Pin::We => f.write_str("WE"),
            Pin::Re => f.write_str("RE"),
            Pin::Do(k) => write!(f, "DO{k}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rp(width: Width) -> ReadPort {
        ReadPort {
            width,
            out_reg: None,
        }
    }

    #[test]
    fn address_pins_follow_the_uarch() {
        // nextpnr/cyclone2 `m4k_addr_pin`.
        let a: Vec<String> = (0..12).map(|p| Pin::addr(Side::A, p).to_string()).collect();
        assert_eq!(
            a,
            [
                "DI12", "DI13", "DI14", "DI15", "WA11", "WA0", "WA1", "WA2", "WA3", "WA4", "WA5",
                "WA6"
            ]
        );
        let b: Vec<String> = (0..12).map(|p| Pin::addr(Side::B, p).to_string()).collect();
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
        for name in [
            "DI0", "DI35", "WA6", "WA11", "RA9", "CLK0", "CLK1", "ENA1", "CLR0", "WE", "RE", "BE3",
            "DO35",
        ] {
            assert_eq!(Pin::parse(name).unwrap().to_string(), name);
        }
        for name in ["DI36", "WA7", "RA8", "DO", "CLK2", "BE4"] {
            assert_eq!(Pin::parse(name), None, "{name}");
        }
    }

    #[test]
    fn cell_port_names_round_trip() {
        for name in [
            "CLK",
            "CLK1",
            "ENA0",
            "CLR1",
            "A_WE",
            "B_WE",
            "A_ADDR[11]",
            "B_DATAIN[17]",
            "A_BYTEENA[3]",
            "B_BYTEENA[1]",
            "DATAOUT[35]",
        ] {
            assert_eq!(CellPort::parse(name).unwrap().to_string(), name);
        }
        for name in ["B_DATAIN[18]", "B_BYTEENA[2]", "A_ADDR[12]"] {
            assert_eq!(CellPort::parse(name), None, "{name}");
        }
    }

    #[test]
    fn unsupported_shapes_are_refused() {
        let single = |w| Ports::Single { a: rp(w) };
        assert!(Shape::legacy(single(Width::X36), 36).is_err());
        assert!(Shape::legacy(single(Width::X18), 18).is_ok());
        let tdp = Ports::TrueDual {
            a: rp(Width::X36),
            b: rp(Width::X1),
            b_clock: Sel::Zero,
        };
        assert!(Shape::new(tdp, Width::X1, 1, Packing::Dense).is_err());
        // A x1/x2 read address uses DI26 and DI35, ninth columns a x36
        // write over x1 base words leaves out (x32); a full x36 write
        // would need them.
        let sdp = |r| Ports::SimpleDual {
            write: Width::X36,
            read: rp(r),
            read_clock: Sel::Zero,
        };
        assert!(Shape::new(sdp(Width::X1), Width::X1, 1, Packing::Dense).is_ok());
        assert!(Shape::new(sdp(Width::X4), Width::X4, 4, Packing::Dense).is_ok());
        let tdp9 = Ports::TrueDual {
            a: rp(Width::X18),
            b: rp(Width::X1),
            b_clock: Sel::One,
        };
        assert!(Shape::new(tdp9, Width::X1, 1, Packing::Dense).is_ok());
        // Too many bits for the base word.
        assert!(Shape::new(single(Width::X18), Width::X18, 17, Packing::Bytes).is_err());
    }

    #[test]
    fn mixed_widths_agree_on_every_bit() {
        // x32 write, x8 read (fuzz d32x8): read word a bit s, and write word
        // a / 4 bit 8 * (a % 4) + s, are one linear position.
        let s = Shape::new(
            Ports::SimpleDual {
                write: Width::X36,
                read: rp(Width::X9),
                read_clock: Sel::Zero,
            },
            Width::X9,
            8,
            Packing::Dense,
        )
        .unwrap();
        assert_eq!(s.data_bits(Width::X36), 32);
        for a in 0..512 {
            for b in 0..8 {
                assert_eq!(
                    s.linear(Width::X9, a, b),
                    s.linear(Width::X36, a / 4, 8 * (a % 4) + b)
                );
            }
        }
        assert_eq!(s.column(Width::X36, 8), 9);
        // x16 under two byte enables: the second byte from column 9.
        let b = Shape::new(
            Ports::Single { a: rp(Width::X18) },
            Width::X18,
            16,
            Packing::Bytes,
        )
        .unwrap();
        assert_eq!(b.column(Width::X18, 8), 9);
        assert!(!b.holds(8) && b.holds(9) && !b.holds(17));
    }

    #[test]
    fn params_round_trip() {
        let shapes = [
            Shape::legacy(Ports::Single { a: rp(Width::X9) }, 8).unwrap(),
            Shape::new(
                Ports::SimpleDual {
                    write: Width::X36,
                    read: ReadPort {
                        width: Width::X9,
                        out_reg: Some(OutReg {
                            clock: Sel::One,
                            clear: Some(Sel::Zero),
                        }),
                    },
                    read_clock: Sel::One,
                },
                Width::X9,
                8,
                Packing::Dense,
            )
            .unwrap(),
            Shape::new(
                Ports::TrueDual {
                    a: rp(Width::X18),
                    b: rp(Width::X4),
                    b_clock: Sel::One,
                },
                Width::X4,
                4,
                Packing::Dense,
            )
            .unwrap(),
        ];
        for s in shapes {
            let p = s.params();
            let get = |k: &str| p.iter().find(|(n, _)| *n == k).map(|(_, v)| v.clone());
            let back = Shape::parse(
                |k| match get(k) {
                    Some(Param::Str(s)) => Some(s.to_string()),
                    _ => None,
                },
                |k| match get(k) {
                    Some(Param::Int(i)) => Some(i),
                    _ => None,
                },
                |_| unreachable!("SLICE is always written"),
            )
            .unwrap();
            assert_eq!(back, s);
        }
    }
}
