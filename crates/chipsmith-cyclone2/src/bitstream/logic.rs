//! What a placed design asks of its LEs and LABs, in physical terms, and
//! the CRAM bits that follow.
//!
//! A frontend (`nextpnr`, or cyclone2-re's Quartus one) fills these in:
//! LUT masks, carry and register wiring, which LAB line feeds which LE
//! input and LAB control, and which global clock buffer feeds a LAB. Bit positions
//! all come from the asmdb (`lab`); the setting *values* below were read
//! off fuzz designs (specimens/lab, README "LE and LAB configuration") and
//! say so where they are not simply "the selected mux input".

use anyhow::{ensure, Context, Result};
use std::collections::BTreeMap;

use super::asmdb::arch;

use super::lab::LabDb;

/// Number of LAB local interconnect lines; LE input mux sources 0..38 are
/// these, 38..54 the 16 LEs' own outputs (LOCAL_LINE), per the asmdb
/// LE_LUT_INPUT source maps (54 entries) and the counter design.
const LAB_LINES: u8 = 38;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LeSource {
    /// LOCAL_INTERCONNECT line of the LAB.
    Line(u8),
    /// LOCAL_LINE: an LE of the same LAB feeding back directly.
    Feedback(u8),
}

impl LeSource {
    fn index(self) -> usize {
        match self {
            LeSource::Line(l) => l as usize,
            LeSource::Feedback(k) => LAB_LINES as usize + k as usize,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Lut {
    /// Truth table over physical inputs, entry A + 2B + 4C + 8D.
    pub mask: u16,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Reg {
    /// LAB clock 0 or 1.
    pub clock: u8,
    /// LAB async clear line, if the register is cleared.
    pub aclr: Option<u8>,
    /// Takes the LAB's synchronous clear/load (one per-LE bit enables both).
    pub sync: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LeConfig {
    pub x: u32,
    pub y: u32,
    pub le: u32,
    pub lut: Option<Lut>,
    /// Physical inputs A..D.
    pub inputs: [Option<LeSource>; 4],
    /// LUT input C comes from the carry chain (`sum_lutc_input = cin` with a
    /// real carry-in net).
    pub carry_in: bool,
    /// LUT input C is the LE's own register (no routing).
    pub reg_feedback: bool,
    pub reg: Option<Reg>,
    /// Output (LE_BUFFER n, LE_BUFFER n+1, LOCAL_LINE) driven by the LUT
    /// rather than the register.
    pub comb_out: [bool; 3],
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GlobalClock {
    /// BLK_CLK_BUF index 0..5: which row clock the LAB taps.
    pub buf: u8,
    pub inverted: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Aclr {
    Pia(u8),
    Global(u8),
}

/// A LAB control signal arriving on control line `line`, possibly
/// inverted at the register port (the LAB has an invert bit for it).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Ctl {
    pub line: u8,
    pub inverted: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Sload {
    Pia(Ctl),
    /// sload tied high: registers load sdata (= their LE's datac) every
    /// cycle, how the fitter packs a register with an unrelated LUT.
    Const,
}

/// The LAB's synchronous clear and load; a LAB has one of each.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Sync {
    pub sclr: Option<Ctl>,
    pub sload: Option<Sload>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LabConfig {
    /// LAB_PIA_TO_INPUT k: what feeds LAB control line k (same source
    /// space as an LE input: local line or an LE's own output).
    pub pia: [Option<LeSource>; 4],
    pub clocks: [Option<GlobalClock>; 2],
    /// Clock enable of LAB clock j, as the control line carrying it.
    pub enables: [Option<Ctl>; 2],
    pub aclr: Option<Aclr>,
    pub sync: Sync,
}

/// Routing element of the muxes from a clock control tile's LAB lines into
/// its clock controls (and the PLLs' control inputs).
pub const BLOCK_INPUT_MUX: i16 = 532;
/// `CLKBUF_IN` index of a clock control's input from the core.
pub const CORE_INPUT: u8 = 4;

/// A global clock control block in use: GCLK `g` driven from its clock
/// input `input` (CLKBUF_IN index: 0/1 dedicated clock pins, ...).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ClkCtrl {
    pub g: u8,
    pub input: u8,
    /// For a core-driven input (rcf `LOCAL_INTERCONNECT` ->
    /// `BLOCK_INPUT_MUX` -> `CLKBUF_IN`): the LAB line it takes.
    pub core_line: Option<u8>,
    /// The clock enable: the LAB line it comes from (none: tied on).
    pub enable: Option<u8>,
    /// `ena_register_mode = "falling edge"` (the atom's default): the
    /// enable is registered on the clock's falling edge, so the clock is
    /// gated glitch-free. Otherwise ("none") it gates the clock directly.
    pub enable_registered: bool,
}

#[derive(Debug, Default)]
pub struct Logic {
    pub les: Vec<LeConfig>,
    pub clkctrls: Vec<ClkCtrl>,
    pub labs: BTreeMap<(u32, u32), LabConfig>,
}

impl Logic {
    /// Every bit this logic fixes, as (flat CRAM address, value). Bits not
    /// listed keep the empty design's value.
    pub fn bits(&self, db: &LabDb) -> Result<Vec<(u32, bool)>> {
        let mut out = Vec::new();
        for c in &self.les {
            le_bits(db, c, &mut out)?;
        }
        for c in &self.clkctrls {
            // Unused controls read select 0b111 / enable 0 in the empty
            // design; in use: the input index, LSB first, and enable 1
            // (counter and fuzz designs, G2/G3/G6/G8/G10/G11/G15).
            let sel = &db.clkctrl_select[c.g as usize];
            let code = clkctrl_select_code(c.input)
                .with_context(|| format!("clock control G{}: input {}", c.g, c.input))?;
            for (k, &b) in sel.iter().enumerate() {
                out.push((b as u32, code >> k & 1 == 1));
            }
            // CLKBUF_ENABLE_MODE is 1 for an unregistered enable, 0 for a
            // registered one (k5, k6: the atom default, enable tied on);
            // the first CLKBUF_ENABLE_AND_INVERT bit is 0 when the enable
            // comes from the core (k3, k4), its mux then taking the line.
            out.push((db.clkctrl_enable[c.g as usize] as u32, !c.enable_registered));
            out.push((db.clkctrl_and_invert[c.g as usize] as u32, c.enable.is_none()));
            if let Some(line) = c.enable {
                for b in db.clkctrl_enout[c.g as usize].source_bits(0, line as usize)? {
                    out.push((b as u32, true));
                }
            }
            if let Some(line) = c.core_line {
                // Source = the LAB line, like an LE input mux (v5: G0 line 24).
                for b in db.clkctrl_corein[c.g as usize].source_bits(0, line as usize)? {
                    out.push((b as u32, true));
                }
            }
        }
        for (&(x, y), l) in &self.labs {
            lab_bits(db, x, y, l, &mut out).with_context(|| format!("LAB X{x} Y{y} {l:?}"))?;
        }
        Ok(out)
    }
}

/// A register's clock domain in a LAB: (row-clock buffer, inverted, enable
/// control line).
pub type Domain = (u8, bool, Option<Ctl>);

/// `CLKBUF_CLK_INPUT_SELECT` code (LSB first) of clock-control input
/// `CLKBUF_IN` index. The dedicated clock pins (0/1) and a core signal
/// through `BLOCK_INPUT_MUX` (4: M4K designs v5, v11; clock fuzz k1, k2)
/// select as their index; a user pin through the clock delay control (6)
/// selects 5 (s8c). The other inputs were never seen, so they have no code
/// here.
pub fn clkctrl_select_code(input: u8) -> Option<u8> {
    match input {
        0..=4 => Some(input),
        6 => Some(5),
        _ => None,
    }
}

/// Which LAB clock (0/1) each of a LAB's clock domains takes. The bitstream
/// doesn't record the choice, so this reproduces the fitter's: CE 1 can
/// only come from control lines 0/1 and CE 0 from 2/3 (asmdb fast-mux
/// maps), and an LE's clock select picks clock and enable together, so an
/// enabled domain's line fixes its LAB clock; the rest fill in inverted
/// clocks first, then by row clock buffer in the order 5, 0, 1, 2, 3, 4.
/// Fit to the two-clock LABs of the LE fuzz designs (lowest buffer first in
/// all of them, buffers 2-4), M4K design h2 (buffer 5 before 2) and clock
/// design k2 (buffer 5 before 4 and before 2). Neither the global clock
/// index nor the netlist order fits all of them. **[inf]**: five LABs
/// decide where buffer 5 goes.
pub fn clock_slots(domains: &[Domain]) -> Result<[Option<Domain>; 2]> {
    let mut v = domains.to_vec();
    v.sort_by_key(|d| (!d.1, (d.0 + 1) % 6, d.2));
    v.dedup();
    ensure!(v.len() <= 2, "{} clock domains", v.len());
    let mut slot: [Option<Domain>; 2] = [None, None];
    for d in v.iter().filter(|d| d.2.is_some()) {
        let j = if d.2.unwrap().line < 2 { 1 } else { 0 };
        ensure!(slot[j].is_none(), "two enables want clock {j}");
        slot[j] = Some(*d);
    }
    for d in v.iter().filter(|d| d.2.is_none()) {
        let j = slot.iter().position(Option::is_none).unwrap();
        slot[j] = Some(*d);
    }
    Ok(slot)
}

fn le_bits(db: &LabDb, c: &LeConfig, out: &mut Vec<(u32, bool)>) -> Result<()> {
    let (x, y, l) = (c.x, c.y, c.le as usize);
    let mut put = |off: i32, v: bool| -> Result<()> {
        out.push((db.flat(x, y, off)?, v));
        Ok(())
    };
    if let Some(lut) = c.lut {
        // Setting i is truth-table entry 15 - i; stored inverted.
        let f = db.feature(arch::LE_LUT_FUNCTION, -1)?;
        for (i, &off) in f.paths[l][0].iter().enumerate() {
            put(off, lut.mask >> (15 - i) & 1 == 0)?;
        }
    }
    for (p, src) in c.inputs.iter().enumerate() {
        if let Some(src) = src {
            for off in db
                .feature(arch::LE_LUT_INPUT, p as i32)?
                .source_bits(l, src.index())?
            {
                put(off, true)?;
            }
        }
    }
    put(db.feature(arch::LE_USE_CARRY, -1)?.bit(l), c.carry_in)?;
    put(
        db.feature(arch::LE_USE_REGISTER_FEEDBACK, -1)?.bit(l),
        c.reg_feedback,
    )?;
    for (o, &comb) in c.comb_out.iter().enumerate() {
        put(db.feature(arch::LE_REG_OUT_YGR, o as i32)?.bit(l), comb)?;
    }
    if let Some(r) = c.reg {
        // Unused LEs read LE_CLOCK_SELECT 1 and LE_CLEAR_SELECT 0 in the
        // empty design; a register without a clear selects line 1 (fuzz).
        put(db.feature(arch::LE_CLOCK_SELECT, -1)?.bit(l), r.clock == 1)?;
        put(
            db.feature(arch::LE_CLEAR_SELECT, -1)?.bit(l),
            r.aclr.unwrap_or(1) == 1,
        )?;
        // A decision mux whose only real bit is the per-LE "use LAB sync
        // signals" enable; LE_USE_SYNC_LOAD names the same bit.
        let f = db.feature(arch::LE_USE_SYNC_CLEAR, -1)?;
        let bit = *f.paths[l][0]
            .iter()
            .find(|&&b| b >= 0)
            .context("sync decision mux has no bit")?;
        put(bit, r.sync)?;
    }
    Ok(())
}

/// Fuzz-read values of the LAB-wide groups. Where a group is a source mux
/// the setting is the asmdb's map of the control line; the polarities
/// (enable bits that go *low* when used, the 3-bit sync code) are observed.
fn lab_bits(db: &LabDb, x: u32, y: u32, l: &LabConfig, out: &mut Vec<(u32, bool)>) -> Result<()> {
    let mut put = |off: i32, v: bool| -> Result<()> {
        out.push((db.flat(x, y, off)?, v));
        Ok(())
    };
    for (k, src) in l.pia.iter().enumerate() {
        if let Some(src) = src {
            for off in db
                .feature(arch::LAB_PIA_TO_INPUT, k as i32)?
                .source_bits(0, src.index())?
            {
                put(off, true)?;
            }
        }
    }
    for (j, clk) in l.clocks.iter().enumerate() {
        let Some(clk) = clk else { continue };
        // Sources 4..9 of LAB_CLOCK_FAST_MUX are the six BLK_CLK_BUFs.
        let f = db.feature(arch::LAB_CLOCK_FAST_MUX, j as i32)?;
        put(f.source_bits(0, 4 + clk.buf as usize)?[0], true)?;
        // LAB_INVERT 0 / 2 (rows 32 and 37, column 0): clock inversion.
        put(
            db.feature(arch::LAB_INVERT, 2 * j as i32)?.bit(0),
            clk.inverted,
        )?;
    }
    for (j, en) in l.enables.iter().enumerate() {
        // LAB_ENABLE_CE j and LAB_INVERT 1 / 3 read 1 when clock j has no
        // enable and 0 when it has one (fuzz); the CE fast mux setting is
        // the control line's entry in the asmdb map.
        put(
            db.feature(arch::LAB_ENABLE_CE, j as i32)?.bit(0),
            en.is_none(),
        )?;
        put(
            db.feature(arch::LAB_INVERT, 2 * j as i32 + 1)?.bit(0),
            !en.is_some_and(|c| !c.inverted),
        )?;
        if let Some(c) = en {
            let f = db.feature(arch::LAB_CE_FAST_MUX, j as i32)?;
            put(f.bit(0), encoded_setting(f, c.line as usize)? == 1)?;
        }
    }
    match l.aclr {
        None => {}
        Some(Aclr::Pia(p)) => {
            // 2-bit encoded first level: setting s sets bit s - 1.
            let f = db.feature(arch::LAB_CLEAR_FAST_MUX, 0)?;
            let s = encoded_setting(f, p as usize)?;
            for (i, &off) in f.paths[0][0].iter().enumerate() {
                put(off, s == i + 1)?;
            }
        }
        Some(Aclr::Global(b)) => {
            // Both clear lines take the global and the pair inversion is
            // set (fuzz: X2Y26, X54Y16); sources 4..9 are BLK_CLK_BUFs.
            for j in 0..2 {
                let f = db.feature(arch::LAB_CLEAR_FAST_MUX, j)?;
                let s = encoded_setting(f, 4 + b as usize)?;
                for (i, &off) in f.paths[0][0].iter().enumerate() {
                    put(off, s == i + 1)?;
                }
                let n = f.next.as_ref().context("clear mux second level")?[4 + b as usize];
                ensure!(n >= 0, "global {b} unreachable");
                put(f.paths[0][1][n as usize], true)?;
            }
            for &off in &db.feature(arch::LAB_CLR_INVERT_PAIR, 6)?.paths[0][0] {
                put(off, true)?;
            }
        }
    }
    // LAB_ENABLE_SYNC_SIGNALS: 3 bits, MSB first, empty/none = 0b101.
    // Codes are the asmdb setting map [5, 7, 4, 6, 1, 0]; which setting is
    // which combination was read off fuzz LABs (sclr+sload: see README).
    let (sclr, sload) = (l.sync.sclr, l.sync.sload);
    let code = match (sclr, sload) {
        (None, None) => 0b101,
        (None, Some(Sload::Pia(_))) => 0b111,
        (None, Some(Sload::Const)) => 0b100,
        (Some(_), None) => 0b001,
        (Some(_), Some(Sload::Pia(_))) => 0b011,
        (Some(_), Some(Sload::Const)) => 0b000,
    };
    let sload = match sload {
        Some(Sload::Pia(p)) => Some(p),
        _ => None,
    };
    let f = db.feature(arch::LAB_ENABLE_SYNC_SIGNALS, -1)?;
    for (i, &off) in f.paths[0][0].iter().enumerate() {
        put(off, code >> (2 - i) & 1 == 1)?;
    }
    // LAB_INVERT 5 is set by a sync clear and LAB_INVERT 4 cleared by a
    // sync load, unless the register port inverts the signal (fuzz, and the
    // counter's `!KEY[0]` clear): the line itself is active low.
    put(
        db.feature(arch::LAB_INVERT, 5)?.bit(0),
        sclr.is_some_and(|c| !c.inverted),
    )?;
    put(
        db.feature(arch::LAB_INVERT, 4)?.bit(0),
        !sload.is_some_and(|c| !c.inverted),
    )?;
    let _ = &l.sync;
    if let Some(c) = sclr {
        let f = db.feature(arch::LAB_SYNC_CLR_FAST_MUX, 0)?;
        put(f.bit(0), encoded_setting(f, c.line as usize)? == 1)?;
    }
    if let Some(c) = sload {
        let f = db.feature(arch::LAB_SYNC_LOAD_FAST_MUX_MAG, 0)?;
        put(f.bit(0), encoded_setting(f, c.line as usize)? == 1)?;
    }
    Ok(())
}

/// Setting of an encoded fast mux for source `s` (its asmdb aux map).
fn encoded_setting(f: &super::lab::Feature, s: usize) -> Result<usize> {
    let a = *f
        .aux
        .as_ref()
        .context("no source map")?
        .get(s)
        .context("source out of range")?;
    ensure!(a >= 0, "source {s} not selectable (setting {a})");
    Ok(a as usize)
}
