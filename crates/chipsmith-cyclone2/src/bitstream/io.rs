//! I/O pins, from first principles: package pin -> pad -> IOE (ddb `pkg`
//! and `pad`), IOE -> the bits of every IOE feature (ddb `asmdb` groups of
//! block types 5 = row IOE, 6 = column IOE, 60/61 = dedicated clock input
//! p/n), and per pin state -> feature values (learned from the I/O fuzz
//! designs, `tools/gen_io.py`; README "I/O pins").
//!
//! An IOE's features live in two places. Core CRAM bits are plain flat
//! addresses. Drive strength, pull-up, bus hold, I/O standard and pin mode
//! are "CFF" bits (asmdb bit value `0xa0000000 | k`) that sit in the 6
//! replicated frames at the front of the stream (`rbf::RepBit`).
//!
//! Assembly writes every bit of every IOE's footprint, so the result
//! doesn't depend on which pins the base image happened to use. The one
//! dual-purpose configuration pins (ASDO, nCSO: inputs with pull-up; nCEO:
//! an output), which Quartus configures from device options when the
//! design doesn't claim them, at their bank's I/O standard.

use anyhow::{bail, ensure, Context, Result};
use std::collections::{BTreeMap, HashMap};

use super::asmdb::{ArchBody, Asmdb, MuxId};

use super::addr;
use super::rbf::{FrameBit, Rbf, RepBit};

/// asmdb block types of the IOE-like blocks.
pub const BLOCK_ROW: i32 = 5;
pub const BLOCK_COL: i32 = 6;
pub const BLOCK_CLK_P: i32 = 60;
pub const BLOCK_CLK_N: i32 = 61;

/// `ASMDB_ARCH_GROUP_TYPE` values of the features the pin model sets
/// (names from the libdb_asmdb.so translator table).
mod arch {
    pub const IO_PIN_MODE: i32 = 84;
    pub const IO_DATA_INPUT: i32 = 85;
    pub const IO_SELECT_IOST_DIRT: i32 = 96;
    pub const IO_PIA_TO_OE: i32 = 105;
    pub const IO_REG_FEEDS_PIN_OUT_YGR: i32 = 109;
    pub const IO_INREG_POWERS_UP_HIGH: i32 = 113;
    pub const IO_OEREG_POWERS_UP_HIGH: i32 = 114;
    pub const IO_OUTREG_POWERS_UP_HIGH: i32 = 115;
    pub const IO_INREG_CLK_FAST_MUX: i32 = 89;
    pub const IO_OUTREG_CLK_FAST_MUX: i32 = 93;
    pub const IO_PIA_TO_CLR: i32 = 101;
    pub const IO_PIA_TO_SCLR: i32 = 102;
    pub const IO_PIA_TO_INREG_CE: i32 = 103;
    pub const IO_PIA_TO_OEREG_CE: i32 = 106;
    pub const IO_CLR_INVERT: i32 = 116;
    pub const IO_SCLR_INVERT: i32 = 117;
    pub const IO_INREG_CE_INVERT: i32 = 119;
    pub const IO_INREG_CLK_INVERT: i32 = 120;
    pub const IO_OEREG_CE_INVERT: i32 = 124;
    pub const IO_OUTREG_CLK_INVERT: i32 = 126;
    pub const IO_USE_SCLR_INREG: i32 = 167;
    pub const IO_USE_SCLR_OEREG: i32 = 168;
    pub const IO_USE_SCLR_OUTREG: i32 = 169;
    pub const IO_DATA_INVERT: i32 = 118;
    pub const IO_CORE_PLL_SELECT: i32 = 146;
    pub const IO_USE_CLR_INREG: i32 = 164;
    pub const IO_USE_CLR_OEREG: i32 = 165;
    pub const IO_USE_CLR_OUTREG: i32 = 166;
    pub const IO_OE_INVERT: i32 = 122;
    pub const IO_SET_CE_TO_OEREG_TIE_OFF: i32 = 130;
    pub const IO_SET_CE_TO_OUTREG_TIE_OFF: i32 = 131;
    pub const IO_SET_PAD_TO_CORE_DUAL_FUNC_DELAY_MAG: i32 = 139;
    pub const IO_SET_DRIVE_STRENGTH_ARMSTRONG: i32 = 161;
    pub const IO_USE_BUS_HOLD: i32 = 163;
    pub const IO_USE_PULL_UP: i32 = 176;
    pub const IO_USE_OUTPUT_DUTY_CYCLE: i32 = 177;
}

/// A configuration bit an IOE owns.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum IoBit {
    Core(FrameBit),
    Rep(RepBit),
}

impl IoBit {
    pub fn get(self, r: &Rbf) -> bool {
        match self {
            IoBit::Core(b) => r.get(b),
            IoBit::Rep(b) => r.rep_get(b),
        }
    }
    pub fn set(self, r: &mut Rbf, v: bool) {
        match self {
            IoBit::Core(b) => r.set(b, v),
            IoBit::Rep(b) => r.rep_set(b, v),
        }
    }
}

/// CFF bit k -> replicated-frame bit. CFF bits come in blocks of 24 (one
/// per IOE, PLL, ...); block j sits in bit-in-byte lane `lane` of the
/// replicated frames at linear byte slot `u = k - off`, with 311 usable
/// bytes per frame starting at byte 1. Fit to the unused-pin pull-up design
/// (455 IOE pull-up bits) and the 16 dedicated clock inputs; every IOE bit
/// of every I/O fuzz design lands where this puts it. `(lane, first
/// block, off)`. Three lanes start with a PLL (8 blocks, asmdb bases 2928,
/// 6192 and 9120), which the IOE data left unplaced in the gaps: the PLL
/// fuzz designs put them at the start of lanes 5, 3 and 1 (each would run
/// past the end of the lane before).
const CFF_LANES: [(u16, u32, i64); 8] = [
    (7, 0, -216),
    (6, 64, 1176),
    (5, 122, 2808),
    (4, 190, 4440),
    (3, 258, 6024),
    (2, 324, 7368),
    (1, 380, 9120),
    (0, 452, 10704),
];

pub fn cff_bit(k: u32) -> Result<RepBit> {
    let j = k / 24;
    let &(lane, _, off) = CFF_LANES.iter().rev().find(|(_, j0, _)| j >= *j0).unwrap();
    let u = k as i64 - off;
    ensure!(
        (0..6 * 311).contains(&u),
        "CFF bit {k} falls outside the replicated frames"
    );
    Ok(RepBit {
        frame: (u / 311) as u8,
        bit: ((u % 311 + 1) * 8) as u16 + lane,
    })
}

/// asmdb bit value -> bit, `None` for the "no such bit" sentinel.
fn bit_of(v: i32) -> Result<Option<IoBit>> {
    if v >= 0 {
        return Ok(Some(IoBit::Core(addr::frame_bit(v as u32))));
    }
    // -3: the slot exists in the group's layout but not on this IOE.
    if v == -3 {
        return Ok(None);
    }
    ensure!(
        v as u32 >> 24 == 0xa0,
        "asmdb IOE bit {v:#x} is neither a flat address nor a CFF bit"
    );
    let k = v as u32 & 0xff_ffff;
    Ok(Some(IoBit::Rep(cff_bit(k)?)))
}

/// Which asmdb instance a package pin's IOE is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub struct Ioe {
    pub block: i32,
    /// Path index in that block type's groups.
    pub path: usize,
}

/// A two-level source mux (output data, output enable): per source, the
/// first- and second-level bit index (asmdb `aux` / `next_mux_selection`),
/// and per path the bits of each level.
struct SourceMux {
    sel: Vec<(i32, i32)>,
    levels: Vec<[Vec<i32>; 2]>,
}

impl SourceMux {
    fn bits(&self, path: usize, source: u32) -> Result<[i32; 2]> {
        let &(a, n) = self
            .sel
            .get(source as usize)
            .context("source out of range")?;
        ensure!(a >= 0 && n >= 0, "source {source} can't reach this mux");
        let l = &self.levels[path];
        Ok([l[0][a as usize], l[1][n as usize]])
    }
}

/// The two-level source muxes from the LAB-local lines: output data,
/// output enable, and the register controls.
const SOURCE_MUXES: [i32; 6] = [
    arch::IO_DATA_INPUT,
    arch::IO_PIA_TO_OE,
    arch::IO_PIA_TO_INREG_CE,
    arch::IO_PIA_TO_OEREG_CE,
    arch::IO_PIA_TO_CLR,
    arch::IO_PIA_TO_SCLR,
];

/// One IOE's bits of a feature, in level order (`None`: a sentinel slot
/// the hardware doesn't have).
type FeatureBits = Vec<Option<IoBit>>;

pub struct IoDb {
    pub pins: HashMap<String, Pin>,
    /// (block, arch type, group index) -> per path, the feature's bits.
    features: BTreeMap<(i32, i32, i32), Vec<FeatureBits>>,
    muxes: HashMap<(i32, i32), SourceMux>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Pin {
    pub name: String,
    pub ioe: Ioe,
    pub xyz: (u16, u16, u8),
    pub bank: i32,
    pub dual_purpose: Option<DualPurpose>,
}

/// Configuration pins that double as user I/O.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum DualPurpose {
    /// ASDO, nCSO: active serial flash interface, inputs with pull-up.
    SerialFlash,
    /// nCEO: chip-enable out, an output.
    Nceo,
}

fn mux_bits(db: &Asmdb, m: &Option<MuxId>) -> Vec<i32> {
    m.map(|m| db.mux(m).bits.clone()).unwrap_or_default()
}

impl IoDb {
    pub fn new(pins: &[Pin], db: &Asmdb) -> Result<Self> {
        let pins = pins.iter().map(|p| (p.name.clone(), p.clone())).collect();
        let mut features = BTreeMap::new();
        let mut muxes = HashMap::new();
        for g in db
            .arch_groups
            .iter()
            .filter(|g| [BLOCK_ROW, BLOCK_COL, BLOCK_CLK_P, BLOCK_CLK_N].contains(&g.block_type))
        {
            let per_path: Vec<FeatureBits> = match &g.body {
                // A dumb group lists one bit per path, in path order
                // (checked: its pull-up/bus-hold blocks agree with the
                // muxed groups' drive-strength blocks on all 459 IOEs).
                ArchBody::Dumb { bits, .. } => bits
                    .iter()
                    .map(|&b| Ok(vec![bit_of(b)?]))
                    .collect::<Result<_>>()?,
                _ => g
                    .paths()
                    .iter()
                    .map(|p| {
                        p.iter()
                            .flat_map(|m| mux_bits(db, m))
                            .map(bit_of)
                            .collect::<Result<_>>()
                    })
                    .collect::<Result<_>>()?,
            };
            ensure!(
                features
                    .insert((g.block_type, g.arch_type, g.group_index), per_path)
                    .is_none(),
                "duplicate IOE group {} {}",
                g.block_type,
                g.arch_type
            );
            if SOURCE_MUXES.contains(&g.arch_type) {
                let d = g.data().first().context("source mux without data")?;
                let (aux, next) = (mux_bits(db, &d.aux), mux_bits(db, &d.next_selection));
                let levels = g
                    .paths()
                    .iter()
                    .map(|p| match p.as_slice() {
                        [a, b] => Ok([mux_bits(db, a), mux_bits(db, b)]),
                        _ => bail!("source mux path is not two levels"),
                    })
                    .collect::<Result<_>>()?;
                muxes.insert(
                    (g.block_type, g.arch_type),
                    SourceMux {
                        sel: aux.into_iter().zip(next).collect(),
                        levels,
                    },
                );
            }
        }
        Ok(IoDb {
            pins,
            features,
            muxes,
        })
    }

    /// Every bit of one IOE, by feature.
    fn footprint(&self, ioe: Ioe) -> impl Iterator<Item = ((i32, i32), &[Option<IoBit>])> {
        self.features
            .range((ioe.block, i32::MIN, i32::MIN)..=(ioe.block, i32::MAX, i32::MAX))
            .filter_map(move |(&(_, a, i), v)| v.get(ioe.path).map(|b| ((a, i), b.as_slice())))
    }

    /// Bits of feature (arch, group index) for one IOE.
    fn feature(&self, ioe: Ioe, arch: i32, index: i32) -> Result<&[Option<IoBit>]> {
        self.features
            .get(&(ioe.block, arch, index))
            .and_then(|v| v.get(ioe.path))
            .map(Vec::as_slice)
            .with_context(|| format!("IOE {ioe:?} has no feature {arch}/{index}"))
    }
}

/// I/O standards the model knows (single-ended, no VREF).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Standard {
    Lvttl33,
    Lvcmos33,
    V25,
    V18,
    V15,
}

impl Standard {
    /// Row IOE default current (what Quartus gives nCEO).
    fn row_default_ma(self) -> u32 {
        match self {
            Standard::Lvttl33 => 24,
            Standard::Lvcmos33 => 24,
            Standard::V25 => 8,
            Standard::V18 => 12,
            Standard::V15 => 6,
        }
    }
    fn low_voltage_input(self) -> bool {
        matches!(self, Standard::V18 | Standard::V15)
    }

    /// The current the fitter gives an output that asks for none: on
    /// every standard but 3.3-V LVTTL it differs between row and column
    /// IOEs (std fuzz designs; README "I/O pins").
    fn default_ma(self, block: i32) -> u32 {
        match (self, block == BLOCK_COL) {
            (Standard::Lvttl33, _) => 24,
            (Standard::Lvcmos33, false) => 12,
            (Standard::Lvcmos33, true) => 24,
            (Standard::V25, false) => 8,
            (Standard::V25, true) => 16,
            (Standard::V18, _) => 12,
            (Standard::V15, false) => 6,
            (Standard::V15, true) => 8,
        }
    }
}

/// What each I/O bank is powered at, which an unclaimed dual-purpose pin
/// follows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Vccio {
    /// What the design's pins in it ask for, as Quartus infers it (the
    /// last pin's standard; a bank's pins agree on VCCIO in Quartus' own
    /// netlists). An empty bank is 3.3 V.
    FromPins,
    /// Every bank at the voltage of this standard, as a board powers them.
    Board(Standard),
}

/// A register control into an I/O cell: a LAB-local line through the
/// cell's source mux for it, optionally inverted in the cell.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IoControl {
    pub line: u32,
    pub inverted: bool,
}

/// How a side's registers are clocked. The input register has its own
/// clock; the output and OE registers share one clock mux, clock inverter
/// and clock-enable line (each register still picks whether it uses the
/// enable: `IO_SET_CE_TO_*REG_TIE_OFF`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IoClock {
    /// The I/O row clock buffer (`BLK_CLK_BUF` index).
    pub buffer: u32,
    /// Clocked on the falling edge (the netlist's `!clk`).
    pub inverted: bool,
    pub enable: Option<IoControl>,
}

/// An I/O-cell register.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IoReg {
    pub clock: IoClock,
    /// Power-up value 1 (`*_power_up = "high"`): what Quartus uses to
    /// move an inverter from after the register to before it.
    pub powers_up_high: bool,
    /// The cell's asynchronous reset, if this register uses it. It
    /// returns the register to its power-up value: a clear on one that
    /// powers up 0, a preset on one that powers up 1 (Quartus writes
    /// `*_async_reset = "preset"` with `*_power_up = "high"`, and the bits
    /// differ only in `*REG_POWERS_UP_HIGH`). The cell has one, shared by
    /// its three registers.
    pub areset: Option<IoControl>,
    /// The cell's synchronous clear, if this register uses it; also one
    /// per cell. Only seen on registers powering up 0.
    pub sclr: Option<IoControl>,
}

impl IoReg {
    /// A rising-edge register with no enable, reset or clear.
    pub fn plain(buffer: u32, powers_up_high: bool) -> Self {
        IoReg {
            clock: IoClock {
                buffer,
                inverted: false,
                enable: None,
            },
            powers_up_high,
            areset: None,
            sclr: None,
        }
    }
}

/// `a` and `b` where both are given must be the same: one of the cell's
/// shared controls.
fn agree<T: PartialEq + Copy + std::fmt::Debug>(
    what: &str,
    v: impl IntoIterator<Item = Option<T>>,
) -> Result<Option<T>> {
    let mut out = None;
    for x in v.into_iter().flatten() {
        match out {
            Some(o) => ensure!(
                o == x,
                "two different {what}s in one I/O cell ({o:?}, {x:?}): it has one"
            ),
            None => out = Some(x),
        }
    }
    Ok(out)
}

/// An output's data: a LAB-local line into the IOE's data mux (optionally
/// inverted in the IOE, optionally through the output register), a
/// constant, or a PLL's clock.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutData {
    Line {
        line: u32,
        inverted: bool,
        reg: Option<IoReg>,
    },
    Const(bool),
    /// A PLL's counter 2 through its external clock output's clock control
    /// (rcf `IO_RAW_DATAOUT`), on that PLL's PLL<n>_OUT pin only. It
    /// bypasses the data mux, the inverter and the register
    /// (`IO_CORE_PLL_SELECT`; observed on PLL_1's, PIN_AA7).
    PllClock,
}

/// When the output buffer drives the pad.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutputEnable {
    /// Always (the netlist's `oe` is vcc): a plain output.
    Always,
    /// Open drain (`open_drain_output`): the buffer only ever pulls low; a
    /// data 1 releases the pad. Quartus also makes an inout that is never
    /// driven an open drain with data 1.
    OpenDrain,
    /// Never (`oe` is gnd: the enable folded to a constant 0).
    Never,
    /// Tri-state: a LAB-local line into the IOE's OE mux, optionally
    /// inverted in the IOE, optionally through the OE register (which
    /// shares the output register's clock).
    Line {
        line: u32,
        inverted: bool,
        reg: Option<IoReg>,
    },
}

/// Core-to-pad side of a pin.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Output {
    /// mA; `None`: the standard's default.
    pub drive: Option<u32>,
    pub data: OutData,
    pub enable: OutputEnable,
}

impl Output {
    pub fn data_reg(&self) -> Option<IoReg> {
        match self.data {
            OutData::Line { reg, .. } => reg,
            OutData::Const(_) | OutData::PllClock => None,
        }
    }

    pub fn oe_reg(&self) -> Option<IoReg> {
        match self.enable {
            OutputEnable::Line { reg, .. } => reg,
            _ => None,
        }
    }

    /// The output-side registers' clock buffer and edge. The output and OE
    /// registers share one clock mux (`IO_OUTREG_CLK_FAST_MUX`), clock
    /// inverter and enable line, so two different ones are an error.
    pub fn clock(&self) -> Result<Option<(u32, bool)>> {
        let regs = [self.data_reg(), self.oe_reg()];
        agree(
            "clock-enable line",
            regs.map(|r| r.and_then(|r| r.clock.enable)),
        )?;
        agree(
            "output-side clock",
            regs.map(|r| r.map(|r| (r.clock.buffer, r.clock.inverted))),
        )
    }
}

impl Dir {
    /// The cell's registers: input, output, OE.
    pub fn regs(&self) -> [Option<IoReg>; 3] {
        let o = self.output();
        [
            self.input().and_then(|i| i.reg).map(|r| r.reg),
            o.and_then(Output::data_reg),
            o.and_then(Output::oe_reg),
        ]
    }

    /// The cell's asynchronous reset and synchronous clear lines, which
    /// its registers share.
    pub fn resets(&self) -> Result<(Option<IoControl>, Option<IoControl>)> {
        let r = self.regs();
        Ok((
            agree("asynchronous reset", r.map(|r| r.and_then(|r| r.areset)))?,
            agree("synchronous clear", r.map(|r| r.and_then(|r| r.sclr)))?,
        ))
    }
}

/// Pad-to-core side of a pin.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Input {
    /// Which of the two pad-to-core paths (`IO_DATAIN` index) carry the
    /// pad itself into the core.
    pub comb: [bool; 2],
    pub reg: Option<InReg>,
}

/// The input register and the pad-to-core paths that carry its output.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InReg {
    pub reg: IoReg,
    pub paths: [bool; 2],
}

/// What a used pin does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Dir {
    In(Input),
    /// Output only: the input buffer is off.
    Out(Output),
    Bidir(Input, Output),
}

impl Dir {
    pub fn input(&self) -> Option<&Input> {
        match self {
            Dir::In(i) | Dir::Bidir(i, _) => Some(i),
            Dir::Out(_) => None,
        }
    }
    pub fn output(&self) -> Option<&Output> {
        match self {
            Dir::Out(o) | Dir::Bidir(_, o) => Some(o),
            Dir::In(_) => None,
        }
    }
}

/// How the design uses one pin.
#[derive(Debug, Clone)]
pub struct PinUse {
    pub pin: String,
    pub port: String,
    pub standard: Standard,
    pub pull_up: bool,
    pub bus_hold: bool,
    pub dir: Dir,
}

/// One feature value: bits in the feature's level order.
type Setting = (i32, i32, &'static [u8]);

/// What unused user I/O pins do (`RESERVE_ALL_UNUSED_PINS`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnusedPins {
    InputTriStated,
    InputBusHold,
    InputPullUp,
    /// Also what "AS OUTPUT DRIVING AN UNSPECIFIED SIGNAL" gives: the
    /// two came out bit-identical.
    OutputGround,
}

/// Feature values of an unused IOE, observed on every unused IOE of the
/// fuzz designs (one design per mode) apart from the dual-purpose pins.
/// Features not listed are 0.
fn unused_settings(block: i32, mode: UnusedPins) -> Vec<Setting> {
    use arch::*;
    let mut v: Vec<Setting> = vec![(IO_SELECT_IOST_DIRT, -1, &[1, 1, 0])];
    if matches!(block, BLOCK_ROW | BLOCK_COL) {
        v.extend([
            (IO_PIN_MODE, -1, &[1u8, 0][..]),
            (IO_REG_FEEDS_PIN_OUT_YGR, -1, &[1, 0][..]),
        ]);
        match mode {
            UnusedPins::OutputGround => v.push((
                IO_SET_DRIVE_STRENGTH_ARMSTRONG,
                -1,
                drive_code(block, Standard::Lvttl33, 24).unwrap(),
            )),
            _ => v.push((IO_OE_INVERT, -1, &[1])),
        }
        match mode {
            UnusedPins::InputBusHold => v.push((IO_USE_BUS_HOLD, -1, &[1])),
            UnusedPins::InputPullUp => v.push((IO_USE_PULL_UP, -1, &[1])),
            _ => {}
        }
    }
    v
}

/// An unclaimed dual-purpose pin at its bank's standard. Observed on the
/// fuzz designs at every standard (nCEO is a row IOE).
fn dual_purpose_settings(block: i32, d: DualPurpose, std: Standard) -> Result<Vec<Setting>> {
    use arch::*;
    let mut v: Vec<Setting> = vec![
        (IO_PIN_MODE, -1, &[1, 0]),
        (IO_SET_CE_TO_OEREG_TIE_OFF, -1, &[1]),
        (IO_SET_CE_TO_OUTREG_TIE_OFF, -1, &[1]),
    ];
    match d {
        DualPurpose::SerialFlash => {
            v.push((IO_OE_INVERT, -1, &[1]));
            v.push((IO_USE_PULL_UP, -1, &[1]));
            v.push((
                IO_SELECT_IOST_DIRT,
                -1,
                if std.low_voltage_input() {
                    &[0, 1, 0]
                } else {
                    &[0, 0, 0]
                },
            ));
        }
        DualPurpose::Nceo => {
            v.push((IO_DATA_INVERT, -1, &[1]));
            v.push((IO_USE_OUTPUT_DUTY_CYCLE, -1, &[1, 1]));
            v.push((IO_SELECT_IOST_DIRT, -1, &[1, 1, 0]));
            // nCEO follows the bank's VCCIO, not its standard: in a 3.3-V
            // LVCMOS bank it still gets the LVTTL 24 mA code.
            let std = if std == Standard::Lvcmos33 {
                Standard::Lvttl33
            } else {
                std
            };
            let code = drive_code(block, std, std.row_default_ma())
                .with_context(|| format!("nCEO drive at {std:?}"))?;
            v.push((IO_SET_DRIVE_STRENGTH_ARMSTRONG, -1, code));
        }
    }
    Ok(v)
}

/// Drive strength codes (`IO_SET_DRIVE_STRENGTH_ARMSTRONG`), observed on
/// the fuzz designs: every current of 3.3-V LVTTL, the default current of
/// the other standards. The code depends on the standard, not only the
/// current. Row IOEs have 8 bits, column IOEs 10.
fn drive_code(block: i32, std: Standard, ma: u32) -> Option<&'static [u8]> {
    use Standard::*;
    Some(match (block, std, ma) {
        (BLOCK_ROW, Lvttl33, 4) => &[1, 0, 0, 0, 1, 0, 0, 0],
        (BLOCK_ROW, Lvttl33, 8) => &[0, 1, 0, 0, 0, 1, 0, 0],
        (BLOCK_ROW, Lvttl33, 12) => &[1, 1, 0, 0, 1, 1, 0, 0],
        (BLOCK_ROW, Lvttl33, 16) => &[1, 0, 1, 0, 0, 0, 1, 0],
        (BLOCK_ROW, Lvttl33, 20) => &[0, 1, 1, 0, 1, 0, 1, 0],
        (BLOCK_ROW, Lvttl33, 24) => &[1, 1, 1, 0, 0, 1, 1, 0],
        (BLOCK_ROW, Lvcmos33, 12) => &[1, 1, 1, 0, 1, 1, 1, 0],
        (BLOCK_ROW, V25, 8) => &[1, 1, 0, 0, 0, 1, 1, 0],
        (BLOCK_ROW, V18, 12) => &[1, 1, 1, 0, 1, 1, 1, 0],
        (BLOCK_ROW, V15, 6) => &[0, 1, 1, 0, 0, 1, 1, 0],
        (BLOCK_COL, Lvttl33, 4) => &[1, 0, 0, 0, 0, 1, 0, 0, 0, 0],
        (BLOCK_COL, Lvttl33, 8) => &[0, 1, 0, 0, 0, 0, 1, 0, 0, 0],
        (BLOCK_COL, Lvttl33, 12) => &[1, 1, 0, 0, 0, 1, 1, 0, 0, 0],
        (BLOCK_COL, Lvttl33, 16) => &[1, 0, 1, 0, 0, 0, 0, 1, 0, 0],
        (BLOCK_COL, Lvttl33, 20) => &[0, 1, 1, 0, 0, 1, 0, 1, 0, 0],
        (BLOCK_COL, Lvttl33, 24) => &[1, 1, 1, 0, 0, 0, 1, 1, 0, 0],
        (BLOCK_COL, Lvcmos33, 24) => &[1, 1, 1, 1, 1, 1, 1, 0, 1, 1],
        (BLOCK_COL, V25, 16) => &[0, 1, 1, 0, 0, 0, 0, 1, 1, 0],
        (BLOCK_COL, V18, 12) => &[1, 1, 1, 0, 0, 1, 1, 1, 0, 0],
        (BLOCK_COL, V15, 8) => &[0, 0, 0, 1, 0, 0, 0, 0, 1, 0],
        _ => return None,
    })
}

/// Feature values of a used IOE, all observed on the fuzz designs.
fn used_settings(block: i32, u: &PinUse) -> Result<Vec<Setting>> {
    use arch::*;
    let mut v: Vec<Setting> = Vec::new();
    if matches!(block, BLOCK_ROW | BLOCK_COL) {
        // Pin mode, two bits (asmdb map 1,1,1,3,3,0,2): bit 0 is a
        // combinational OE (0: the OE register drives it), bit 1 open
        // drain. Inputs and unused pins read 10.
        let oe_reg = matches!(
            u.dir.output().map(|o| o.enable),
            Some(OutputEnable::Line { reg: Some(_), .. })
        );
        let open_drain = matches!(
            u.dir.output().map(|o| o.enable),
            Some(OutputEnable::OpenDrain)
        );
        v.push((
            IO_PIN_MODE,
            -1,
            if oe_reg {
                &[0, 0]
            } else if open_drain {
                &[1, 1]
            } else {
                &[1, 0]
            },
        ));
        // The output and OE registers' clock enables are tied on unless
        // the register uses the cell's enable line (the input register
        // has no tie-off: its enable mux, unselected, enables it).
        let [_, out_reg, oe_reg_] = u.dir.regs();
        let enabled = |r: Option<IoReg>| r.is_some_and(|r| r.clock.enable.is_some());
        if !enabled(oe_reg_) {
            v.push((IO_SET_CE_TO_OEREG_TIE_OFF, -1, &[1]));
        }
        if !enabled(out_reg) {
            v.push((IO_SET_CE_TO_OUTREG_TIE_OFF, -1, &[1]));
        }
        // An input's output buffer is held off by the inverter after its
        // (unselected) OE mux; a tri-state output inverts its OE there.
        let oe_invert = match u.dir.output().map(|o| o.enable) {
            None | Some(OutputEnable::Never) => true,
            Some(OutputEnable::Line { inverted, .. }) => inverted,
            Some(OutputEnable::Always | OutputEnable::OpenDrain) => false,
        };
        if oe_invert {
            v.push((IO_OE_INVERT, -1, &[1]));
        }
        // Set on every used pin except outputs below 3.3 V (observed on
        // 2.5, 1.8 and 1.5 V; the name suggests duty-cycle correction).
        let low_v = matches!(u.standard, Standard::V25 | Standard::V18 | Standard::V15);
        if matches!(u.dir, Dir::In(_)) || !low_v {
            v.push((IO_USE_OUTPUT_DUTY_CYCLE, -1, &[1, 1]));
        }
        if let Some(o) = u.dir.output() {
            let ma = o.drive.unwrap_or(u.standard.default_ma(block));
            let code = drive_code(block, u.standard, ma).with_context(|| {
                format!("{}: no drive code for {:?} at {ma} mA", u.pin, u.standard)
            })?;
            v.push((IO_SET_DRIVE_STRENGTH_ARMSTRONG, -1, code));
            match o.data {
                // Observed on the empty design's LED tied to 0.
                OutData::Const(false) => v.push((IO_DATA_INVERT, -1, &[1])),
                // An unselected data mux drives 0, so a 1 needs no
                // inversion; observed on bidirs whose data folded to vcc.
                OutData::Const(true) => {}
                OutData::Line { inverted, .. } => {
                    if inverted {
                        v.push((IO_DATA_INVERT, -1, &[1]));
                    }
                }
                OutData::PllClock => v.push((IO_CORE_PLL_SELECT, -1, &[1])),
            }
        }
        // Each pad-to-core path the input drives turns on its delay
        // setting (observed: 287 of 287 routed inputs; unrouted inputs 0):
        // 110 from the pad, 111 from the input register. A path carrying
        // the pad while the other carries the register stays 000 (observed
        // on 2 pins).
        if let Some(i) = u.dir.input() {
            let reg_paths = i.reg.map_or([false; 2], |r| r.paths);
            for (k, (&comb, &reg)) in i.comb.iter().zip(&reg_paths).enumerate() {
                let code: &[u8] = match (comb, reg) {
                    (_, true) => &[1, 1, 1],
                    (true, false) if i.reg.is_none() => &[1, 1, 0],
                    _ => continue,
                };
                v.push((IO_SET_PAD_TO_CORE_DUAL_FUNC_DELAY_MAG, k as i32, code));
            }
        }
        // Registers: clock from the row clock buffer. The clock fast mux
        // is one-hot, bit = buffer index + 1 (observed for buffers 3, 4
        // and 5).
        const ONE_HOT: [&[u8]; 6] = [
            &[0, 1, 0, 0, 0, 0, 0],
            &[0, 0, 1, 0, 0, 0, 0],
            &[0, 0, 0, 1, 0, 0, 0],
            &[0, 0, 0, 0, 1, 0, 0],
            &[0, 0, 0, 0, 0, 1, 0],
            &[0, 0, 0, 0, 0, 0, 1],
        ];
        let clk = |b: u32| {
            ONE_HOT
                .get(b as usize)
                .copied()
                .with_context(|| format!("{}: I/O clock buffer {b}", u.pin))
        };
        // Per register: `USE_CLR` reads 1 unless it uses the cell's
        // asynchronous reset, `USE_SCLR` 10 if it uses the synchronous
        // clear, `POWERS_UP_HIGH` for power-up 1, which is also the value
        // the reset restores (ioce fuzz designs e3, e4, e8, e16).
        let regs = u.dir.regs();
        let feats = [
            (IO_USE_CLR_INREG, IO_USE_SCLR_INREG, IO_INREG_POWERS_UP_HIGH),
            (
                IO_USE_CLR_OUTREG,
                IO_USE_SCLR_OUTREG,
                IO_OUTREG_POWERS_UP_HIGH,
            ),
            (IO_USE_CLR_OEREG, IO_USE_SCLR_OEREG, IO_OEREG_POWERS_UP_HIGH),
        ];
        for (r, (use_clr, use_sclr, high)) in regs.iter().zip(feats) {
            let Some(r) = r else { continue };
            ensure!(
                !(r.sclr.is_some() && r.powers_up_high),
                "{}: an I/O register powering up 1 with a synchronous clear (never seen)",
                u.pin
            );
            if r.areset.is_none() {
                v.push((use_clr, -1, &[1]));
            }
            if r.sclr.is_some() {
                v.push((use_sclr, -1, &[1, 0]));
            }
            if r.powers_up_high {
                v.push((high, -1, &[1]));
            }
        }
        if regs[1].is_some() {
            v.push((IO_REG_FEEDS_PIN_OUT_YGR, -1, &[1, 0]));
        }
        // The shared controls' inverters. The clear is active low in the
        // cell, so an active-high reset sets its inverter; the enables and
        // the synchronous clear are active high.
        let (areset, sclr) = u.dir.resets().with_context(|| u.pin.clone())?;
        if areset.is_some_and(|c| !c.inverted) {
            v.push((IO_CLR_INVERT, -1, &[1]));
        }
        if sclr.is_some_and(|c| c.inverted) {
            v.push((IO_SCLR_INVERT, -1, &[1]));
        }
        if let Some(r) = regs[0] {
            v.push((IO_INREG_CLK_FAST_MUX, -1, clk(r.clock.buffer)?));
            if r.clock.inverted {
                v.push((IO_INREG_CLK_INVERT, -1, &[1]));
            }
            if r.clock.enable.is_some_and(|c| c.inverted) {
                v.push((IO_INREG_CE_INVERT, -1, &[1]));
            }
        }
        // The output and OE registers share the output clock mux; an OE
        // register alone sets it too (observed on oereg pins).
        if let Some(o) = u.dir.output() {
            if let Some((b, inverted)) = o.clock().with_context(|| u.pin.clone())? {
                v.push((IO_OUTREG_CLK_FAST_MUX, -1, clk(b)?));
                if inverted {
                    v.push((IO_OUTREG_CLK_INVERT, -1, &[1, 1]));
                }
            }
            if agree(
                "clock-enable line",
                [o.data_reg(), o.oe_reg()].map(|r| r.and_then(|r| r.clock.enable)),
            )?
            .is_some_and(|c| c.inverted)
            {
                v.push((IO_OEREG_CE_INVERT, -1, &[1]));
            }
        }
    } else {
        ensure!(
            matches!(u.dir, Dir::In(_)),
            "{}: dedicated clock pin used as {:?}",
            u.pin,
            u.dir
        );
    }
    // Input buffer select: an output-only pin has it switched off the same
    // way an unused one does; 1.8 V and 1.5 V inputs use the low-voltage
    // buffer.
    v.push((
        IO_SELECT_IOST_DIRT,
        -1,
        match (u.dir, u.standard) {
            (Dir::Out(_), _) => &[1, 1, 0],
            (_, s) if s.low_voltage_input() => &[0, 1, 0],
            _ => &[0, 0, 0],
        },
    ));
    if u.pull_up {
        v.push((IO_USE_PULL_UP, -1, &[1]));
    }
    if u.bus_hold {
        v.push((IO_USE_BUS_HOLD, -1, &[1]));
    }
    Ok(v)
}

impl IoDb {
    /// Every IOE bit, for a design using `uses`. Unused dual-purpose pins
    /// are left out (they keep the base image).
    /// `serial_flash_claimed`: the serial-flash block drives ASDO and nCSO
    /// (an unclaimed one is an input with pull-up).
    pub fn bits(
        &self,
        uses: &[PinUse],
        unused: UnusedPins,
        vccio: Vccio,
        serial_flash_claimed: bool,
    ) -> Result<Vec<(IoBit, bool)>> {
        let by_pin: HashMap<&str, &PinUse> = uses.iter().map(|u| (u.pin.as_str(), u)).collect();
        let mut bank_std: HashMap<i32, Standard> = HashMap::new();
        for u in uses {
            let b = self
                .pins
                .get(&u.pin)
                .with_context(|| format!("{} ({}) is not a user I/O pin", u.pin, u.port))?
                .bank;
            if vccio == Vccio::FromPins {
                bank_std.insert(b, u.standard);
            }
        }
        let empty_bank = match vccio {
            Vccio::FromPins => Standard::Lvttl33,
            Vccio::Board(s) => s,
        };
        let mut out = Vec::new();
        let mut pins: Vec<&Pin> = self.pins.values().collect();
        pins.sort_by(|a, b| a.name.cmp(&b.name));
        for p in pins {
            let u = by_pin.get(p.name.as_str());
            let mut val: HashMap<IoBit, bool> = HashMap::new();
            for (_, bits) in self.footprint(p.ioe) {
                for b in bits.iter().flatten() {
                    val.insert(*b, false);
                }
            }
            let settings = match (u, p.dual_purpose) {
                (Some(u), _) => used_settings(p.ioe.block, u)?,
                (None, Some(d)) => {
                    let mut v = dual_purpose_settings(
                        p.ioe.block,
                        d,
                        bank_std.get(&p.bank).copied().unwrap_or(empty_bank),
                    )?;
                    if serial_flash_claimed && d == DualPurpose::SerialFlash {
                        v.retain(|s| s.0 != arch::IO_USE_PULL_UP);
                    }
                    v
                }
                (None, None) => unused_settings(p.ioe.block, unused),
            };
            for (a, i, v) in settings {
                let bits = self.feature(p.ioe, a, i)?;
                ensure!(
                    bits.len() == v.len(),
                    "{}: feature {a}/{i} has {} bits, setting {}",
                    p.name,
                    bits.len(),
                    v.len()
                );
                for (b, &x) in bits.iter().zip(v) {
                    if let Some(b) = b {
                        val.insert(*b, x == 1);
                    }
                }
            }
            if let Some(u) = u.filter(|_| matches!(p.ioe.block, BLOCK_ROW | BLOCK_COL)) {
                let o = u.dir.output();
                let data_line = o.and_then(|o| match o.data {
                    OutData::Line { line, .. } => Some(line),
                    OutData::Const(_) | OutData::PllClock => None,
                });
                let oe_line = o.and_then(|o| match o.enable {
                    OutputEnable::Line { line, .. } => Some(line),
                    _ => None,
                });
                let [in_reg, out_reg, oe_reg] = u.dir.regs();
                let line = |c: Option<IoControl>| c.map(|c| c.line);
                // The output side's enable line drives the OEREG_CE mux,
                // whichever of the two registers uses it (ioce fuzz e1:
                // output registers alone set it too).
                let out_ce = agree(
                    "clock-enable line",
                    [out_reg, oe_reg].map(|r| r.and_then(|r| r.clock.enable)),
                )?;
                let (areset, sclr) = u.dir.resets()?;
                for (a, line) in [
                    (arch::IO_DATA_INPUT, data_line),
                    (arch::IO_PIA_TO_OE, oe_line),
                    (
                        arch::IO_PIA_TO_INREG_CE,
                        line(in_reg.and_then(|r| r.clock.enable)),
                    ),
                    (arch::IO_PIA_TO_OEREG_CE, line(out_ce)),
                    (arch::IO_PIA_TO_CLR, line(areset)),
                    (arch::IO_PIA_TO_SCLR, line(sclr)),
                ] {
                    let Some(line) = line else { continue };
                    let m = self
                        .muxes
                        .get(&(p.ioe.block, a))
                        .with_context(|| format!("{}: no source mux {a}", p.name))?;
                    for f in m
                        .bits(p.ioe.path, line)
                        .with_context(|| format!("{}: line {line}", p.name))?
                    {
                        let b = bit_of(f)?.context("source mux bit is a sentinel")?;
                        val.insert(b, true);
                    }
                }
            }
            out.extend(val);
        }
        out.sort();
        Ok(out)
    }

    /// LAB-local lines (of the IOE's output tile) the output data mux can
    /// select. Empty for blocks without one (dedicated clock inputs).
    pub fn data_sources(&self, p: &Pin) -> Vec<u32> {
        self.sources(p, arch::IO_DATA_INPUT)
    }

    /// LAB-local lines the output enable mux can select (the same tile's
    /// lines as the data mux: rcf `IO_OE_PIN` sits beside `IO_DATAOUT`).
    pub fn oe_sources(&self, p: &Pin) -> Vec<u32> {
        self.sources(p, arch::IO_PIA_TO_OE)
    }

    /// LAB-local lines each register control's source mux can select, by
    /// the pin buffer port it serves: the input register's clock enable,
    /// the output side's (the `OEREG_CE` mux, which both registers use),
    /// the asynchronous reset. The same tile's lines as the data mux (rcf
    /// `IO_CEIN_PIN`, `IO_CEOUT_PIN`, `IO_ACLR_PIN` sit beside
    /// `IO_DATAOUT`).
    pub fn control_sources(&self, p: &Pin) -> [(&'static str, Vec<u32>); 3] {
        [
            ("INCLKENA", self.sources(p, arch::IO_PIA_TO_INREG_CE)),
            ("OUTCLKENA", self.sources(p, arch::IO_PIA_TO_OEREG_CE)),
            ("ARESET", self.sources(p, arch::IO_PIA_TO_CLR)),
        ]
    }

    fn sources(&self, p: &Pin, arch: i32) -> Vec<u32> {
        let Some(m) = self.muxes.get(&(p.ioe.block, arch)) else {
            return vec![];
        };
        (0..m.sel.len() as u32)
            .filter(|&l| m.bits(p.ioe.path, l).is_ok())
            .collect()
    }

    /// Which pin, feature (arch type, group) and position in that feature
    /// own a bit (debugging aid).
    pub fn describe(&self, bit: IoBit) -> Option<(String, i32, i32, usize)> {
        for p in self.pins.values() {
            for ((a, i), bits) in self.footprint(p.ioe) {
                if let Some(k) = bits.iter().position(|b| *b == Some(bit)) {
                    return Some((p.name.clone(), a, i, k));
                }
            }
        }
        None
    }

    /// Every bit any IOE owns (for residual accounting).
    pub fn all_bits(&self) -> impl Iterator<Item = IoBit> + '_ {
        self.features
            .values()
            .flatten()
            .flatten()
            .flatten()
            .copied()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cff_lanes() {
        // Pull-up of the first row IOE (pad 0, E3) and of N2's IOE.
        let b = cff_bit(12446).unwrap();
        assert_eq!((b.frame, b.bit / 8, b.bit % 8), (5, 188, 0));
        let b = cff_bit(10908).unwrap();
        assert_eq!((b.frame, b.bit / 8, b.bit % 8), (0, 205, 0));
    }
}
