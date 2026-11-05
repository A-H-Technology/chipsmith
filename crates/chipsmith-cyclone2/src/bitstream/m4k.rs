//! M4K RAM blocks: what a placed design configures in one, in physical
//! terms, and the CRAM bits that follow.
//!
//! Device side (asmdb block type 39, observed): 105 blocks, one per LAB row
//! in the M4K columns X13, X26 and X52. A block is 70 CRAM rows x 166
//! columns, based at `le::lab_row(y)` and the block's highest frame
//! (`le::lut_frame(x - 1) + 12 + 166`); mux bits are flat offsets from that
//! base, like a LAB's. Every feature below is an asmdb group of the block;
//! what fuzzing (README "M4K RAM blocks") supplied is which logical port pin
//! each physical input is, the memory content order, and the setting values.
//!
//! The memory is 128 rows x 36 columns; `RAM_CONTENT_YGR_MEAB` lists its
//! 4,608 bits column-major (bit `col * 128 + row`), stored as written (not
//! inverted). A port of physical width P (1, 2, 4, 9, 18, 36 bits) sees it
//! as 4608/P words (4096/P for P < 9: those widths skip every ninth column)
//! laid out row-major: word `a` bit `s` is linear bit `a*P + s` of the 36-
//! column rows.

use anyhow::{ensure, Context, Result};
use std::collections::{BTreeMap, HashMap};

use super::asmdb::{arch, Asmdb};

use super::lab::Feature;
use super::{addr, le};

pub const ROWS: usize = 128;
pub const COLS: usize = 36;
pub const BITS: usize = ROWS * COLS;
/// LAB-grid columns that hold M4K blocks.
pub const COLUMNS: [u32; 3] = [13, 26, 52];

/// Physical width of a port: how it addresses the 128 x 36 array. A port
/// configured x8, x16 or x32 is physically x9, x18, x36 with the ninth bits
/// unused (or used for data, when the fitter permutes bits into them).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Width {
    X1,
    X2,
    X4,
    X9,
    X18,
    X36,
}

impl Width {
    pub fn bits(self) -> usize {
        [1, 2, 4, 9, 18, 36][self.class()]
    }
    /// Address bits: 12 for x1 down to 7 for x36.
    pub fn addr_bits(self) -> u32 {
        12 - self.class() as u32
    }
    /// Index into the one-hot `RAM_DATA_WRITE_MODE` (fuzz: x1 bit 0 .. x36
    /// bit 5).
    fn class(self) -> usize {
        self as usize
    }
    pub fn from_bits(bits: u64) -> Option<Self> {
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
    pub fn from_addr_bits(n: u32) -> Option<Self> {
        Some(match n {
            12 => Width::X1,
            11 => Width::X2,
            10 => Width::X4,
            9 => Width::X9,
            8 => Width::X18,
            7 => Width::X36,
            _ => return None,
        })
    }
    /// The column of this port's word that linear position `l` falls in.
    pub fn column(self, l: usize) -> usize {
        let p = self.bits();
        if p < 9 {
            (l / 9 * 8 + l % 9) % p
        } else {
            l % p
        }
    }
    /// Linear position in the 36-column rows of bit `slot` of word `a`.
    pub fn linear(self, a: usize, slot: usize) -> usize {
        let p = self.bits();
        if p < 9 {
            let l = a * p + slot;
            l / 8 * 9 + l % 8
        } else {
            a * p + slot
        }
    }
}

/// The RAM contents in `RAM_CONTENT_YGR_MEAB` order (`col * 128 + row`).
#[derive(Clone, PartialEq, Eq)]
pub struct Content(Box<[u64; BITS / 64]>);

impl Default for Content {
    fn default() -> Self {
        Content(Box::new([0; BITS / 64]))
    }
}

impl std::fmt::Debug for Content {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let ones: u32 = self.0.iter().map(|w| w.count_ones()).sum();
        write!(f, "Content({ones} of {BITS} set)")
    }
}

impl Content {
    /// Set the bit at linear position `l` of the 36-column rows.
    pub fn set_linear(&mut self, l: usize) {
        let (row, col) = (l / COLS, l % COLS);
        let i = col * ROWS + row;
        self.0[i / 64] |= 1 << (i % 64);
    }
    /// The bit at linear position `l` of the 36-column rows.
    pub fn get_linear(&self, l: usize) -> bool {
        self.get(l % COLS * ROWS + l / COLS)
    }
    pub fn get(&self, i: usize) -> bool {
        self.0[i / 64] >> (i % 64) & 1 == 1
    }
}

/// The M4K's control pins and the fast mux (arch type, group) feeding each:
/// sources 0-7 are the 8 control inputs (`CTL<k>`), 8-13 the row clocks.
/// The pin names are the chip database's (cyclone2-re `chipdb.rs`).
pub const CONTROL_PINS: [(&str, i32, i32); 8] = [
    ("CLK0", arch::RAM_CLOCK_FAST_MUX, 0),
    ("CLK1", arch::RAM_CLOCK_FAST_MUX, 1),
    ("ENA0", arch::RAM_CE_FAST_MUX, 0),
    ("ENA1", arch::RAM_CE_FAST_MUX, 1),
    ("CLR0", arch::RAM_CLEAR_FAST_MUX, 0),
    ("CLR1", arch::RAM_CLEAR_FAST_MUX, 1),
    ("WE", arch::RAM_WE_FAST_MUX, -1),
    ("RE", arch::RAM_RE_FAST_MUX, -1),
];

/// One of the block's two clocks / clears.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Sel {
    Zero,
    One,
}

/// A port that reads: its width and its optional output register.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReadPort {
    pub width: Width,
    pub out_reg: Option<OutReg>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OutReg {
    pub clock: Sel,
    pub clear: Option<Sel>,
}

/// How the block's ports are used. Port A is always clocked by clock 0.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// Port A reads and writes (a ROM is this without a write enable).
    Single { a: ReadPort },
    /// Port A writes, port B reads; port B's address register on `read_clock`.
    /// Without `write`: a x36 ROM, which Quartus builds as a read-only port B.
    SimpleDual {
        write: Option<Width>,
        read: ReadPort,
        read_clock: Sel,
    },
    /// Both ports read and write, port B's input registers on `b_clock`.
    /// Widths up to x18 (`Mode::true_dual` checks).
    TrueDual {
        a: ReadPort,
        b: ReadPort,
        b_clock: Sel,
    },
    /// Two x18 single ports on one clock, port A on words 128-255 and port
    /// B on 0-127 of a 256 x 18 block: how Quartus builds a 128 x 32/36
    /// single port.
    Packed {
        a_out: Option<OutReg>,
        b_out: Option<OutReg>,
    },
}

impl Mode {
    pub fn true_dual(a: ReadPort, b: ReadPort, b_clock: Sel) -> Result<Mode> {
        ensure!(
            a.width <= Width::X18 && b.width <= Width::X18,
            "true dual port wider than x18"
        );
        Ok(Mode::TrueDual { a, b, b_clock })
    }
}

/// A control signal reaching one of the block's control fast muxes: one of
/// the 8 control inputs (`RAM_PIA_TO_INPUT_YGR`, fed from a LAB line) or a
/// row clock (`BLK_CLK_BUF` 0-5).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CtlSrc {
    Control(u8),
    Global(u8),
}

impl CtlSrc {
    /// Source index in the fast muxes' asmdb maps: 0-7 control inputs,
    /// 8-13 row clocks (observed on the clock, clear and write-enable muxes).
    fn index(self) -> usize {
        match self {
            CtlSrc::Control(k) => k as usize,
            CtlSrc::Global(g) => 8 + g as usize,
        }
    }
}

/// The block's physical input pins, named by the asmdb group that selects
/// their LAB line. `DATA_INPUT` 18-35 are the same muxes as `DATA_INPUT_B`
/// 0-17, `BYTE_ENABLE_INPUT` 2-3 as `BYTE_ENABLE_INPUT_B` 0-1 (identical bit
/// lists), so those groups don't appear here.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Input {
    /// 0-35: data in, port A's columns 0-17 (36 for x36), port B's 18-35.
    Data(u8),
    /// 0-6 and 11.
    WriteAddr(u8),
    /// 0-6, 9, 10, 11.
    ReadAddr(u8),
    ByteEna(u8),
}

impl Input {
    /// Every physical input pin, in chip database order.
    pub fn all() -> Vec<Input> {
        let mut v: Vec<Input> = (0..36).map(Input::Data).collect();
        v.extend((0..7).chain([11]).map(Input::WriteAddr));
        v.extend((0..7).chain([9, 10, 11]).map(Input::ReadAddr));
        v.extend((0..4).map(Input::ByteEna));
        v
    }

    /// Bel pin name in the chip database (`chipdb.rs`), which the nextpnr
    /// uarch's packer targets.
    pub fn pin_name(self) -> String {
        match self {
            Input::Data(k) => format!("DI{k}"),
            Input::WriteAddr(k) => format!("WA{k}"),
            Input::ReadAddr(k) => format!("RA{k}"),
            Input::ByteEna(k) => format!("BE{k}"),
        }
    }

    pub fn from_pin_name(s: &str) -> Option<Input> {
        let num = |p: &str| s.strip_prefix(p)?.parse::<u8>().ok();
        let i = if let Some(k) = num("DI") {
            Input::Data(k)
        } else if let Some(k) = num("WA") {
            Input::WriteAddr(k)
        } else if let Some(k) = num("RA") {
            Input::ReadAddr(k)
        } else {
            Input::ByteEna(num("BE")?)
        };
        Input::all().contains(&i).then_some(i)
    }

    /// The asmdb source mux (arch type, group) that picks its LAB line.
    pub fn group(self) -> (i32, i32) {
        match self {
            Input::Data(k) => (arch::RAM_MS_DATA_INPUT, k as i32),
            Input::WriteAddr(k) => (arch::RAM_MS_WRITE_ADDRESS_INPUT, k as i32),
            Input::ReadAddr(k) => (arch::RAM_MS_READ_ADDRESS_INPUT, k as i32),
            Input::ByteEna(k) => (arch::RAM_MS_BYTE_ENABLE_INPUT, k as i32),
        }
    }

    /// Physical address pin `p` (0-11, LSB first; a port of width w uses
    /// pins 12 - addr_bits(w) ..= 11) of port A / B. Fuzz: port A's low pins
    /// are data inputs 12-15 (unused below x18), port B's are data inputs
    /// 26 and 35 and read address 9-10.
    pub fn addr(port_b: bool, p: u32) -> Result<Input> {
        ensure!(p < 12, "address pin {p}");
        Ok(match (port_b, p) {
            (false, 0..=3) => Input::Data(12 + p as u8),
            (false, 4) => Input::WriteAddr(11),
            (false, _) => Input::WriteAddr(p as u8 - 5),
            (true, 0) => Input::Data(26),
            (true, 1) => Input::Data(35),
            (true, 2..=4) => Input::ReadAddr(7 + p as u8),
            (true, _) => Input::ReadAddr(p as u8 - 5),
        })
    }
}

/// The block's control fast muxes and the LAB lines feeding its 8 control
/// inputs.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Controls {
    /// LAB line driving control input k (`RAM_PIA_TO_INPUT_YGR` k).
    pub pia: [Option<u8>; 8],
    pub clock: [Option<CtlSrc>; 2],
    pub enable: [Option<CtlSrc>; 2],
    /// Clock enable k is active low (`RAM_CE_INVERT` k). Only meaningful
    /// with `enable[k]`.
    pub enable_inverted: [bool; 2],
    pub clear: [Option<CtlSrc>; 2],
    /// Port A write enable.
    pub write_enable: Option<CtlSrc>,
    /// Port B write enable (`portbrewe`; true dual port only).
    pub b_write_enable: Option<CtlSrc>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct M4k {
    pub x: u32,
    pub y: u32,
    pub mode: Mode,
    pub controls: Controls,
    /// Which LAB line feeds each used input pin.
    pub inputs: BTreeMap<Input, u8>,
    pub content: Content,
}

/// The asmdb's M4K tables.
pub struct M4kDb {
    features: HashMap<(i32, i32), Feature>,
    content: Vec<i32>,
    bases: HashMap<(u32, u32), u32>,
}

impl M4kDb {
    pub fn new(db: &Asmdb) -> Result<Self> {
        let mut features = HashMap::new();
        for g in db
            .arch_groups
            .iter()
            .filter(|g| g.block_type == arch::BLOCK_M4K)
        {
            features.insert((g.arch_type, g.group_index), Feature::from(db, g));
        }
        let content = features
            .get(&(arch::RAM_CONTENT_YGR_MEAB, -1))
            .context("asmdb has no M4K content group")?
            .paths[0][0]
            .clone();
        ensure!(
            content.len() == BITS,
            "M4K content has {} bits",
            content.len()
        );
        let bi = db
            .block_info(arch::BLOCK_M4K)
            .context("asmdb has no M4K block info")?;
        let cols = bi.num_ram_cols as u32;
        ensure!(
            cols == addr::COLUMNS,
            "asmdb row width {cols} != {}",
            addr::COLUMNS
        );
        let mut bases = HashMap::new();
        for &packed in bi.block_lists.values().flatten() {
            let (row, col) = (packed >> 16, packed & 0xffff);
            let y = 35 - (row - le::lab_row(35)) / le::LAB_ROWS;
            ensure!(
                le::lab_row(y) == row,
                "asmdb M4K at row {row} is not a LAB row start"
            );
            let x = *COLUMNS
                .iter()
                .find(|&&x| block_column(x) == Some(col))
                .with_context(|| format!("asmdb M4K at column {col} is in no M4K column"))?;
            bases.insert((x, y), row * cols + col);
        }
        ensure!(bases.len() == 105, "{} M4K blocks", bases.len());
        Ok(M4kDb {
            features,
            content,
            bases,
        })
    }

    pub fn feature(&self, t: i32, i: i32) -> Result<&Feature> {
        self.features
            .get(&(t, i))
            .with_context(|| format!("asmdb M4K group {t}/{i} missing"))
    }

    fn flat(&self, x: u32, y: u32, off: i32) -> Result<u32> {
        let b = self
            .bases
            .get(&(x, y))
            .with_context(|| format!("no M4K at X{x} Y{y}"))?;
        Ok((*b as i64 + off as i64) as u32)
    }

    /// Base flat address of block (x, y); feature offsets count from it.
    pub fn base(&self, x: u32, y: u32) -> Result<u32> {
        self.flat(x, y, 0)
    }

    /// (x, y) of every block.
    pub fn locs(&self) -> Vec<(u32, u32)> {
        let mut v: Vec<_> = self.bases.keys().copied().collect();
        v.sort();
        v
    }
}

/// Flat column of an M4K block's base: the last frame of the 166 between
/// LAB columns x-1 and x+1 (observed: asmdb columns 2083, 1493, 456).
fn block_column(x: u32) -> Option<u32> {
    Some(addr::LAST_FRAME - (le::lut_frame(x - 1)? + 12 + 166))
}

impl M4k {
    /// Every bit this block fixes, as (flat CRAM address, value). Bits not
    /// listed keep the unused-block value of the base image.
    pub fn bits(&self, db: &M4kDb) -> Result<Vec<(u32, bool)>> {
        let mut out = Vec::new();
        let (x, y) = (self.x, self.y);
        let mut put = |off: i32, v: bool| -> Result<()> {
            out.push((db.flat(x, y, off)?, v));
            Ok(())
        };
        let one = |t: i32| -> Result<i32> { Ok(db.feature(t, -1)?.bit(0)) };
        // RAM_ENABLE in every used block; the extended write pulse in all
        // but a true dual port whose port A never writes (fuzz: d8k; a ROM
        // keeps it).
        let a_reads_only =
            matches!(self.mode, Mode::TrueDual { .. }) && self.controls.write_enable.is_none();
        for (t, on) in [
            (arch::RAM_USE_EXTENDED_WRITE_PULSE_MAGELLAN, !a_reads_only),
            (arch::RAM_ENABLE, true),
        ] {
            for &b in &db.feature(t, -1)?.paths[0][0] {
                put(b, on)?;
            }
        }
        for t in [
            arch::RAM_ENABLE_CE_INPUT,
            arch::RAM_ENABLE_CE_INPUT_B,
            arch::RAM_ENABLE_CE_OUTPUT,
            arch::RAM_ENABLE_CE_OUTPUT_B,
        ] {
            put(one(t)?, true)?;
        }
        let write_mode = |t: i32, w: Width| -> Result<Vec<(i32, bool)>> {
            Ok(db.feature(t, -1)?.paths[0][0]
                .iter()
                .enumerate()
                .map(|(i, &b)| (b, i == w.class()))
                .collect())
        };
        let (a_out, b_out) = match self.mode {
            Mode::Single { a } => {
                // Single port is the true-dual-port block, with read-during-
                // write flow-through on both ports when it writes (fuzz; a
                // ROM has none).
                put(one(arch::RAM_USE_TRUE_DUAL_PORT_MODE)?, true)?;
                let writes = self.controls.write_enable.is_some();
                for t in [arch::RAM_FLOW_THROUGH_MODE, arch::RAM_FLOW_THROUGH_MODE_B] {
                    put(one(t)?, writes)?;
                }
                for (b, v) in write_mode(arch::RAM_DATA_WRITE_MODE, a.width)? {
                    put(b, v)?;
                }
                (a.out_reg, None)
            }
            Mode::SimpleDual {
                write,
                read,
                read_clock,
            } => {
                for (b, v) in write
                    .map(|w| write_mode(arch::RAM_DATA_WRITE_MODE, w))
                    .transpose()?
                    .unwrap_or_default()
                {
                    put(b, v)?;
                }
                for (b, v) in write_mode(arch::RAM_DATA_WRITE_MODE_B, read.width)? {
                    put(b, v)?;
                }
                put(
                    one(arch::RAM_SELECT_CLK_READ_ENABLE_READ_ADDRESS_B)?,
                    read_clock == Sel::One,
                )?;
                // x36 writes use port B's data-in registers for columns
                // 18-35, which then take port A's clock 0 (fuzz).
                if write == Some(Width::X36) {
                    put(one(arch::RAM_SELECT_CLK_BYTE_ENABLE_DATA_B)?, false)?;
                }
                // The read data leaves through the port-A output register
                // group (fuzz: a registered port B clears REG_DATA_OUT, not
                // REG_DATA_OUT_B). A x36 read uses both groups, columns 18-35
                // leaving through port B's (x6).
                (
                    read.out_reg,
                    if read.width == Width::X36 {
                        read.out_reg
                    } else {
                        None
                    },
                )
            }
            Mode::TrueDual { a, b, b_clock } => {
                for t in [
                    arch::RAM_USE_TRUE_DUAL_PORT_MODE,
                    arch::RAM_FLOW_THROUGH_MODE,
                    arch::RAM_FLOW_THROUGH_MODE_B,
                ] {
                    put(one(t)?, true)?;
                }
                for (bit, v) in write_mode(arch::RAM_DATA_WRITE_MODE, a.width)? {
                    put(bit, v)?;
                }
                for (bit, v) in write_mode(arch::RAM_DATA_WRITE_MODE_B, b.width)? {
                    put(bit, v)?;
                }
                put(
                    one(arch::RAM_SELECT_CLK_READ_ENABLE_READ_ADDRESS_B)?,
                    b_clock == Sel::One,
                )?;
                put(
                    one(arch::RAM_SELECT_CLK_BYTE_ENABLE_DATA_B)?,
                    b_clock == Sel::One,
                )?;
                (a.out_reg, b.out_reg)
            }
            Mode::Packed { a_out, b_out } => {
                // A true dual port, both x18 on clock 0, plus the packed
                // flag (s32, s36).
                for t in [
                    arch::RAM_USE_TRUE_DUAL_PORT_MODE,
                    arch::RAM_USE_PACKED_MODE,
                    arch::RAM_FLOW_THROUGH_MODE,
                    arch::RAM_FLOW_THROUGH_MODE_B,
                ] {
                    put(one(t)?, true)?;
                }
                for t in [arch::RAM_DATA_WRITE_MODE, arch::RAM_DATA_WRITE_MODE_B] {
                    for (bit, v) in write_mode(t, Width::X18)? {
                        put(bit, v)?;
                    }
                }
                put(one(arch::RAM_SELECT_CLK_READ_ENABLE_READ_ADDRESS_B)?, false)?;
                put(one(arch::RAM_SELECT_CLK_BYTE_ENABLE_DATA_B)?, false)?;
                (a_out, b_out)
            }
        };
        let regs = [
            (
                a_out,
                arch::RAM_REG_DATA_OUT,
                arch::RAM_SELECT_CLK_DATA_OUT,
                arch::RAM_USE_CLR_DATA_OUT,
                arch::RAM_SELECT_CLR_DATA_OUT,
            ),
            (
                b_out,
                arch::RAM_REG_DATA_OUT_B,
                arch::RAM_SELECT_CLK_DATA_OUT_B,
                arch::RAM_USE_CLR_DATA_OUT_B,
                arch::RAM_SELECT_CLR_DATA_OUT_B,
            ),
        ];
        for (reg, t_reg, t_clk, t_use_clr, t_clr) in regs {
            // REG_DATA_OUT reads 1 for an unregistered output (asmdb map
            // [1, 0]); the clock select's unused value is 1.
            put(one(t_reg)?, reg.is_none())?;
            if let Some(r) = reg {
                put(one(t_clk)?, r.clock == Sel::One)?;
                put(one(t_use_clr)?, r.clear.is_some())?;
                if let Some(c) = r.clear {
                    put(one(t_clr)?, c == Sel::One)?;
                }
            }
        }
        let c = &self.controls;
        if c.write_enable.is_some() {
            // Set for a non-inverted write enable (every fuzz design).
            put(one(arch::RAM_WRITE_ENABLE_INVERT)?, true)?;
        }
        for k in 0..2 {
            // Clear for a non-inverted enable (s8e), set for `!wren` (h2).
            if c.enable[k].is_some() {
                put(
                    db.feature(arch::RAM_CE_INVERT, k as i32)?.bit(0),
                    c.enable_inverted[k],
                )?;
            }
        }
        if c.b_write_enable.is_some() {
            put(one(arch::RAM_READ_ENABLE_INVERT)?, true)?;
        }
        let fast = [
            (arch::RAM_CLOCK_FAST_MUX, 0, c.clock[0]),
            (arch::RAM_CLOCK_FAST_MUX, 1, c.clock[1]),
            (arch::RAM_CE_FAST_MUX, 0, c.enable[0]),
            (arch::RAM_CE_FAST_MUX, 1, c.enable[1]),
            (arch::RAM_CLEAR_FAST_MUX, 0, c.clear[0]),
            (arch::RAM_CLEAR_FAST_MUX, 1, c.clear[1]),
            (arch::RAM_WE_FAST_MUX, -1, c.write_enable),
            (arch::RAM_RE_FAST_MUX, -1, c.b_write_enable),
        ];
        for (t, i, src) in fast {
            let Some(src) = src else { continue };
            let f = db.feature(t, i)?;
            // The map value is the mux's bit pattern (fuzz: clock from row
            // clock 5 = 130 = bits 1 and 7).
            let v = *f
                .aux
                .as_ref()
                .context("fast mux map")?
                .get(src.index())
                .context("fast mux source")?;
            ensure!(v >= 0, "{t}/{i}: source {src:?} not selectable");
            for (k, &b) in f.paths[0][0].iter().enumerate() {
                put(b, v >> k & 1 == 1)?;
            }
        }
        for (k, line) in c.pia.iter().enumerate() {
            if let Some(line) = line {
                for b in db
                    .feature(arch::RAM_PIA_TO_INPUT_YGR, k as i32)?
                    .source_bits(0, *line as usize)?
                {
                    put(b, true)?;
                }
            }
        }
        for (&inp, &line) in &self.inputs {
            let (t, i) = inp.group();
            for b in db
                .feature(t, i)?
                .source_bits(0, line as usize)
                .with_context(|| format!("{inp:?} from line {line}"))?
            {
                put(b, true)?;
            }
        }
        for (i, &off) in db.content.iter().enumerate() {
            put(off, self.content.get(i))?;
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Content index (`col * 128 + row`) of word `a` bit `s`.
    fn index(w: Width, a: usize, s: usize) -> usize {
        let l = w.linear(a, s);
        (l % COLS) * ROWS + l / COLS
    }

    #[test]
    fn content_layout_matches_x8_linear_fuzz() {
        // lin0-lin11 (512 x 8, one design per bit of a*8+b): content index
        // col*128 + row holds word 4*row + col/9, bit col%9; columns 8, 17,
        // 26, 35 (the ninth bits) are never set.
        assert_eq!(index(Width::X9, 0, 0), 0);
        assert_eq!(index(Width::X9, 4, 0), 1);
        assert_eq!(index(Width::X9, 0, 1), 128);
        assert_eq!(index(Width::X9, 1, 0), 9 * 128);
        assert_eq!(index(Width::X9, 2, 0), 18 * 128);
        // x1 skips the ninth columns too; x18 and x36 don't.
        assert_eq!(index(Width::X1, 8, 0), 9 * 128);
        assert_eq!(index(Width::X18, 1, 0), 18 * 128);
        assert_eq!(index(Width::X36, 1, 35), 35 * 128 + 1);
    }

    #[test]
    fn address_pins() {
        // Port A x8 (9 address bits): bit 0 on data input 15, bit 1 on write
        // address 11, bits 2-8 on write address 0-6 (a0, s8*, d8).
        assert_eq!(Input::addr(false, 3).unwrap(), Input::Data(15));
        assert_eq!(Input::addr(false, 4).unwrap(), Input::WriteAddr(11));
        assert_eq!(Input::addr(false, 11).unwrap(), Input::WriteAddr(6));
        // Port B x1 (d1): bits 0, 1 on data inputs 26, 35, then read
        // address 9, 10, 11, 0-6.
        assert_eq!(Input::addr(true, 0).unwrap(), Input::Data(26));
        assert_eq!(Input::addr(true, 1).unwrap(), Input::Data(35));
        assert_eq!(Input::addr(true, 2).unwrap(), Input::ReadAddr(9));
        assert_eq!(Input::addr(true, 5).unwrap(), Input::ReadAddr(0));
    }
}
