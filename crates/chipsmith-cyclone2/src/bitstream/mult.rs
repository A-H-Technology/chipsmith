//! Embedded multipliers: what a placed design configures in one, in
//! physical terms, and the CRAM bits that follow.
//!
//! Device side (asmdb block type 41, observed): 35 blocks, one per LAB row
//! in column X39, each 70 CRAM rows x 27 columns based at `le::lab_row(y)`
//! and flat column 1046. A block is one 18 x 18 multiplier or, in nine-bit
//! mode, two 9 x 9 ones ("rows" 0 and 1, Quartus' `DSPMULT_X39_Y<y>_N0` and
//! `_N1`). Every feature below is an asmdb group of the block; what fuzzing
//! (cyclone2-re README "Embedded multipliers") supplied is which operand bit
//! each data input is and the setting values.
//!
//! Inputs: 36 data inputs (`MAC_ROW_DATA_INPUT` k: operand A bit k for k <
//! 18, operand B bit k - 18 above; in nine-bit mode row 0 has bits 0-8 of
//! each, row 1 bits 9-17), each a two-level source mux over the block's 46
//! LAB lines. An input no line drives reads 1; `MAC_ROW_DATA_INVERT` k
//! turns that into the 0 an unused low bit of a narrow operand needs. Five
//! control inputs (clock, enable, clear, signa, signb: `MAC_PIA_TO_CLK`,
//! `_CE`, `_CLR`, `_SIGN` 0/1) each pick a LAB line. The signs also read 1
//! unconnected, so an unsigned operand needs a line driven 0. Outputs: the
//! 36 product bits on `DSP_BUFFER` 0-35, plain routing-graph elements.
//!
//! Registers: input registers per row and operand
//! (`MAC_ROW_IREG_LOWER_DATA`/`_UPPER_DATA` [operand], lower = row 0 = bits
//! 0-8), on each sign (`MAC_IREG_SIGN`), and on the product
//! (`MAC_ROW_OREG_UPPER_DATA` [row]; an 18-bit product sets both). One
//! clock, enable and clear serve them all. The clock and clear fast muxes
//! are one-hot: bit 0 takes the control input (fed from a line), bit 1 + g
//! row clock g. The enable only comes from a line.

use anyhow::{ensure, Context, Result};
use std::collections::{BTreeMap, HashMap};

use super::asmdb::Asmdb;
use super::lab::Feature;
use super::le;

/// The multiplier column.
pub const X: u32 = 39;
/// LAB lines (`LOCAL_INTERCONNECT`) of a multiplier tile.
pub const LINES: usize = 46;
/// Data inputs and product bits.
pub const DATA: usize = 36;

/// asmdb block type and arch types (libdb_asmdb.so's translator table
/// names them).
pub mod arch {
    pub const BLOCK_MULT: i32 = 41;
    pub const MAC_PIA_TO_CE: i32 = 2031;
    pub const MAC_PIA_TO_CLK: i32 = 2032;
    pub const MAC_PIA_TO_CLR: i32 = 2033;
    pub const MAC_PIA_TO_SIGN: i32 = 2034;
    pub const MAC_CLK_FAST_MUX: i32 = 2048;
    pub const MAC_CLR_FAST_MUX: i32 = 2049;
    pub const MAC_CE_INVERT: i32 = 2053;
    pub const MAC_CLK_INVERT: i32 = 2054;
    pub const MAC_CLR_INVERT: i32 = 2055;
    pub const MAC_IREG_SIGN: i32 = 2071;
    pub const MAC_USE_NINE_BIT_DATA_WIDTH_PER_ROW: i32 = 2138;
    pub const MAC_ROW_DATA_INPUT: i32 = 2139;
    pub const MAC_ROW_DATA_INVERT: i32 = 2140;
    pub const MAC_ROW_IREG_LOWER_DATA: i32 = 2143;
    pub const MAC_ROW_IREG_UPPER_DATA: i32 = 2145;
    pub const MAC_ROW_OREG_UPPER_DATA: i32 = 2151;
}

/// A LAB line of the multiplier tile (0..46).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Line(u8);

impl Line {
    pub fn new(l: u32) -> Result<Self> {
        ensure!((l as usize) < LINES, "multiplier line {l} out of range");
        Ok(Line(l as u8))
    }
    pub fn index(self) -> usize {
        self.0 as usize
    }
}

/// A data input: `MAC_ROW_DATA_INPUT` k, 0..36.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct DataPin(u8);

impl DataPin {
    pub fn new(k: u32) -> Result<Self> {
        ensure!((k as usize) < DATA, "multiplier data input {k}");
        Ok(DataPin(k as u8))
    }
    pub fn index(self) -> usize {
        self.0 as usize
    }
}

/// What a data input reads.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DataIn {
    /// A LAB line, inverted on the way in if so (`MAC_ROW_DATA_INVERT`,
    /// which quartus_map folds a lone inverter into).
    Line { line: Line, inverted: bool },
    /// Undriven and inverted: 0.
    Zero,
}

/// A sign input: 1 (signed) when undriven, else whatever its line carries.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Sign {
    #[default]
    Signed,
    Line(Line),
}

/// A clock or clear: a row clock, or a LAB line through the block's
/// control input.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ctl {
    Global(u8),
    Line(Line),
}

impl Ctl {
    pub fn global(g: u32) -> Result<Ctl> {
        ensure!(g < 6, "row clock {g}");
        Ok(Ctl::Global(g as u8))
    }
}

/// The block's registers and the one clock, enable and clear they share.
/// A block has these only if some register is in use.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Registers {
    pub clock: Ctl,
    pub enable: Option<Line>,
    /// Asynchronous, active high.
    pub clear: Option<Ctl>,
    /// [row][operand]: in 18-bit mode a registered operand registers both
    /// rows.
    pub input: [[bool; 2]; 2],
    pub sign: [bool; 2],
    /// [row].
    pub output: [bool; 2],
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Mult {
    pub y: u32,
    /// Two 9 x 9 multipliers instead of one 18 x 18.
    pub nine_bit: bool,
    /// Data inputs in use; the rest read 1.
    pub data: BTreeMap<DataPin, DataIn>,
    /// signa, signb.
    pub sign: [Sign; 2],
    pub registers: Option<Registers>,
}

/// The asmdb's multiplier tables.
pub struct MultDb {
    features: HashMap<(i32, i32), Feature>,
    bases: HashMap<u32, u32>,
}

impl MultDb {
    pub fn new(db: &Asmdb) -> Result<Self> {
        let features = db
            .arch_groups
            .iter()
            .filter(|g| g.block_type == arch::BLOCK_MULT)
            .map(|g| ((g.arch_type, g.group_index), Feature::from(db, g)))
            .collect();
        let bi = db
            .block_info(arch::BLOCK_MULT)
            .context("asmdb has no multiplier block info")?;
        let cols = bi.num_ram_cols as u32;
        let mut bases = HashMap::new();
        for &packed in bi.block_lists.values().flatten() {
            let (row, col) = (packed >> 16, packed & 0xffff);
            let y = 35 - (row - le::lab_row(35)) / le::LAB_ROWS;
            ensure!(
                le::lab_row(y) == row,
                "asmdb multiplier at row {row} is not a LAB row start"
            );
            bases.insert(y, row * cols + col);
        }
        ensure!(bases.len() == 35, "{} multiplier blocks", bases.len());
        Ok(MultDb { features, bases })
    }

    pub fn feature(&self, t: i32, i: i32) -> Result<&Feature> {
        self.features
            .get(&(t, i))
            .with_context(|| format!("asmdb multiplier group {t}/{i} missing"))
    }

    /// Lines the source mux (arch type `t`, group `i`) can select.
    pub fn sources(&self, t: i32, i: i32) -> Result<Vec<Line>> {
        let f = self.feature(t, i)?;
        let aux = f.aux.as_ref().context("not a source mux")?;
        Ok((0..aux.len().min(LINES))
            .filter(|&s| f.source_bits(0, s).is_ok())
            .map(|s| Line(s as u8))
            .collect())
    }

    /// Y of every block.
    pub fn rows(&self) -> Vec<u32> {
        let mut v: Vec<u32> = self.bases.keys().copied().collect();
        v.sort();
        v
    }

    fn flat(&self, y: u32, off: i32) -> Result<u32> {
        let b = self
            .bases
            .get(&y)
            .with_context(|| format!("no multiplier at X{X} Y{y}"))?;
        Ok((*b as i64 + off as i64) as u32)
    }
}

impl Mult {
    /// Every bit this block fixes, as (flat CRAM address, value). Bits not
    /// listed keep the unused-block value of the base image (all 0).
    pub fn bits(&self, db: &MultDb) -> Result<Vec<(u32, bool)>> {
        let mut out = Vec::new();
        let mut put = |off: i32, v: bool| -> Result<()> {
            out.push((db.flat(self.y, off)?, v));
            Ok(())
        };
        let one = |t: i32, i: i32| -> Result<i32> { Ok(db.feature(t, i)?.bit(0)) };
        put(
            one(arch::MAC_USE_NINE_BIT_DATA_WIDTH_PER_ROW, -1)?,
            self.nine_bit,
        )?;
        for (&pin, &d) in &self.data {
            let k = pin.index() as i32;
            match d {
                DataIn::Line { line: l, inverted } => {
                    for b in db
                        .feature(arch::MAC_ROW_DATA_INPUT, k)?
                        .source_bits(0, l.index())
                        .with_context(|| format!("data input {k} from line {}", l.0))?
                    {
                        put(b, true)?;
                    }
                    if inverted {
                        put(one(arch::MAC_ROW_DATA_INVERT, k)?, true)?;
                    }
                }
                DataIn::Zero => put(one(arch::MAC_ROW_DATA_INVERT, k)?, true)?,
            }
        }
        for (k, s) in self.sign.iter().enumerate() {
            if let Sign::Line(l) = s {
                for b in db
                    .feature(arch::MAC_PIA_TO_SIGN, k as i32)?
                    .source_bits(0, l.index())
                    .with_context(|| format!("sign {k} from line {}", l.0))?
                {
                    put(b, true)?;
                }
            }
        }
        if let Some(r) = &self.registers {
            let put_line =
                |t: i32, l: Line, put: &mut dyn FnMut(i32, bool) -> Result<()>| -> Result<()> {
                    for b in db.feature(t, 0)?.source_bits(0, l.index())? {
                        put(b, true)?;
                    }
                    Ok(())
                };
            let fast = |t: i32,
                        pia: i32,
                        c: Ctl,
                        put: &mut dyn FnMut(i32, bool) -> Result<()>|
             -> Result<()> {
                let bits = &db.feature(t, 0)?.paths[0][0];
                let k = match c {
                    Ctl::Line(l) => {
                        put_line(pia, l, put)?;
                        0
                    }
                    Ctl::Global(g) => 1 + g as usize,
                };
                put(*bits.get(k).context("fast mux input")?, true)
            };
            fast(
                arch::MAC_CLK_FAST_MUX,
                arch::MAC_PIA_TO_CLK,
                r.clock,
                &mut put,
            )?;
            if let Some(c) = r.clear {
                fast(arch::MAC_CLR_FAST_MUX, arch::MAC_PIA_TO_CLR, c, &mut put)?;
                // Set for an active-high clear (fck, fcl, t1).
                put(one(arch::MAC_CLR_INVERT, 0)?, true)?;
            }
            if let Some(l) = r.enable {
                put_line(arch::MAC_PIA_TO_CE, l, &mut put)?;
            }
            for (row, t) in [arch::MAC_ROW_IREG_LOWER_DATA, arch::MAC_ROW_IREG_UPPER_DATA]
                .into_iter()
                .enumerate()
            {
                for op in 0..2 {
                    put(one(t, op as i32)?, r.input[row][op])?;
                }
            }
            for k in 0..2 {
                put(one(arch::MAC_IREG_SIGN, k as i32)?, r.sign[k])?;
                put(one(arch::MAC_ROW_OREG_UPPER_DATA, k as i32)?, r.output[k])?;
            }
        }
        Ok(out)
    }
}
