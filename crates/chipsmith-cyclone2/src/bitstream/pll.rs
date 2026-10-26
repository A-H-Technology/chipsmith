//! PLLs: the four `cycloneii_pll` blocks of the EP2C35 and their bits.
//!
//! A PLL's settings are asmdb block type 43. Its counters, loop and lock
//! settings are CFF bits (the replicated frames) at the instance's base;
//! its input selects and the muxes onto its control inputs are core bits
//! with one asmdb path per instance. Every value is written as a code, LSB
//! first in the asmdb mux's bit order, read off the PLL fuzz designs
//! (cyclone2-re `tools/gen_clk.py`, README "PLLs"): a counter's high and
//! low counts are their own codes (16 as 0), a counter's mode is 0 even,
//! 2 odd, 3 bypass, and so on, as each field below says.

use anyhow::{bail, ensure, Context, Result};
use std::collections::HashMap;

use super::asmdb::{ArchBody, Asmdb};
use super::io::cff_bit;
use super::lab::Feature;
use super::rbf::RepBit;

/// Block type of a PLL.
pub const BLOCK_PLL: i32 = 43;

/// One of the EP2C35's four PLLs, by its Quartus name `PLL_<n>`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Site {
    Pll1,
    Pll2,
    Pll3,
    Pll4,
}

impl Site {
    pub const ALL: [Site; 4] = [Site::Pll1, Site::Pll2, Site::Pll3, Site::Pll4];

    /// Quartus' location name.
    pub fn name(self) -> &'static str {
        match self {
            Site::Pll1 => "PLL_1",
            Site::Pll2 => "PLL_2",
            Site::Pll3 => "PLL_3",
            Site::Pll4 => "PLL_4",
        }
    }

    pub fn from_name(s: &str) -> Option<Site> {
        Site::ALL.into_iter().find(|p| p.name() == s)
    }

    /// The routing tile its outputs (`PLL_BUFFER`) and pins sit in: the
    /// corners, PLL_1 bottom left (fuzz p1-p9: fed from CLK0, its outputs
    /// on the left clock controls), PLL_3 top left (CLK11, p10), PLL_2 top
    /// right (CLK7, p11), PLL_4 bottom right.
    pub fn xy(self) -> (u32, u32) {
        match self {
            Site::Pll1 => (0, 1),
            Site::Pll2 => (65, 35),
            Site::Pll3 => (0, 35),
            Site::Pll4 => (65, 1),
        }
    }

    /// The asmdb instance (block list position, path of the core-bit
    /// groups): bases 9120, 0, 6192, 2928 in that list are PLL_1, PLL_3,
    /// PLL_4, PLL_2 (PLL_1, _2 and _3 seen in fuzz designs; PLL_4 is the
    /// one left).
    fn instance(self) -> usize {
        match self {
            Site::Pll1 => 0,
            Site::Pll3 => 1,
            Site::Pll4 => 2,
            Site::Pll2 => 3,
        }
    }
}

/// A divide-by counter: `high` VCO cycles high, `low` low, each 1-16. Odd
/// mode makes an odd total count's duty cycle 50% by moving the edge half
/// a VCO cycle (Quartus' `odd` mode, `high = low + 1`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Divider {
    high: u8,
    low: u8,
    odd: bool,
}

impl Divider {
    pub fn new(high: u8, low: u8, odd: bool) -> Result<Self> {
        ensure!(
            (1..=16).contains(&high) && (1..=16).contains(&low),
            "counter high {high} / low {low}: each must be 1 to 16"
        );
        Ok(Divider { high, low, odd })
    }
    /// The division ratio.
    pub fn count(self) -> u32 {
        u32::from(self.high) + u32::from(self.low)
    }
    pub fn high(self) -> u8 {
        self.high
    }
    pub fn low(self) -> u8 {
        self.low
    }
    pub fn odd(self) -> bool {
        self.odd
    }
}

/// A post-scale counter in use: its divider (none: bypass, divide by 1),
/// and when its first edge comes after the VCO starts, `initial` whole VCO
/// cycles (1-16) plus `phase` eighths of one (the VCO tap, 0-7).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Output {
    pub divider: Option<Divider>,
    pub initial: u8,
    pub phase: u8,
}

/// How the feedback loop runs: compensating the global clock network's
/// delay to the registers (`normal`, altpll's default), or not at all.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Feedback {
    Normal,
    NoCompensation,
}

/// The loop's analog settings as Quartus' atom parameters name them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Loop {
    /// `charge_pump_current` (µA).
    pub charge_pump: u16,
    /// `loop_filter_r` (kΩ) times 10.
    pub filter_r_tenths: u16,
    /// `loop_filter_c` (pF).
    pub filter_c: u16,
}

impl Loop {
    /// What the fitter picks for a feedback count of `m` (m times the VCO
    /// post-scale), read off the m = 4-32 sweep and the altpll designs
    /// (VCO 600-800 MHz): 53 µA up to m = 7, then 80 µA; 1 kΩ up to
    /// m = 12, then 2.5 kΩ; always 3 pF.
    pub fn for_m(m: u32) -> Loop {
        Loop {
            charge_pump: if m <= 7 { 53 } else { 80 },
            filter_r_tenths: if m <= 12 { 10 } else { 25 },
            filter_c: 3,
        }
    }
}

/// Everything a PLL is set to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Config {
    /// Pre-divider n (1-4) and feedback divider m. A count of 1 is bypass.
    pub n: u8,
    pub m: Divider,
    pub m_initial: u8,
    pub m_phase: u8,
    /// The VCO runs at twice the PFD times m and is divided by 2 before the
    /// counters (Quartus' `vco_post_scale` 2, for a VCO below 500 MHz).
    pub vco_div2: bool,
    /// c0, c1, c2; physical output k is counter ck.
    pub outputs: [Option<Output>; 3],
    pub feedback: Feedback,
    pub r#loop: Loop,
}

/// A PLL in use.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pll {
    pub site: Site,
    pub config: Config,
    /// The dedicated clock pin feeding inclk0, by its index (`z`) among
    /// the four clock pins of the PLL's side.
    pub inclk_pin: u8,
    /// The PLL tile's LAB line driving `areset` (reset, active high) and
    /// `pfdena` (PFD enable, active high), when driven.
    pub areset: Option<u8>,
    pub pfdena: Option<u8>,
}

/// What the bitstream needs of the asmdb's PLL groups.
pub struct PllDb {
    /// (arch type, group index) -> the mux bits of the feature (one level;
    /// CFF offsets from the instance base).
    cff: HashMap<(i32, i32), Vec<i32>>,
    /// (arch type, group index) -> per instance the core bits (one level).
    core: HashMap<(i32, i32), Feature>,
    /// Block base of each instance, in CFF bit space.
    bases: Vec<u32>,
}

mod arch {
    pub const ENABLE_PLL: i32 = 689;
    pub const USE_ENABLE_PIN: i32 = 694;
    pub const M_HIGH: i32 = 695;
    pub const M_LOW: i32 = 696;
    pub const M_PRESET: i32 = 699;
    pub const M_MODE: i32 = 697;
    pub const M_TAB_SELECT: i32 = 701;
    pub const N_MODE: i32 = 704;
    pub const N_DIVIDED_BY: i32 = 708;
    pub const GLOCK_COUNTER_ENABLE: i32 = 766;
    pub const LOCK_SEL_GATE_CONF_DONE: i32 = 795;
    pub const C_MODE: i32 = 711;
    pub const C_HIGH: i32 = 712;
    pub const C_LOW: i32 = 713;
    pub const C_PRESET: i32 = 717;
    pub const C_PHASE: i32 = 718;
    pub const LOCK_WINDOW: i32 = 691;
    pub const LOCK_WINDOW_FB: i32 = 692;
    pub const LOCK_MODE: i32 = 690;
    pub const CHARGE_PUMP: i32 = 768;
    pub const LOOP_R: i32 = 769;
    pub const LOOP_C: i32 = 770;
    pub const VCO_PHASE_CONTROL: i32 = 771;
    pub const USAGE_MODE: i32 = 693;
    pub const VCO_DIV2: i32 = 728;
    pub const FB_COMP_DELAY: i32 = 686;
    pub const CLOCK_SELECT: i32 = 778;
    pub const FEEDBACK_SELECT: i32 = 739;
    pub const ARESET_INPUT: i32 = 745;
    pub const PFD_ENA_INPUT: i32 = 746;
}

/// A bit a PLL sets: in the core (flat address) or in the replicated
/// frames.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PllBit {
    Core(u32),
    Rep(RepBit),
}

impl PllDb {
    pub fn new(db: &Asmdb) -> Result<Self> {
        let bi = db
            .block_info(BLOCK_PLL)
            .context("asmdb has no PLL block info")?;
        let bases: Vec<u32> = bi.block_lists.values().flatten().map(|&p| p & 0xffff).collect();
        ensure!(bases.len() == 4, "{} PLL instances", bases.len());
        let mut cff = HashMap::new();
        let mut core = HashMap::new();
        for g in db.arch_groups.iter().filter(|g| g.block_type == BLOCK_PLL) {
            if matches!(g.body, ArchBody::Dumb { .. }) {
                continue;
            }
            let key = (g.arch_type, g.group_index);
            if g.paths().len() == 4 {
                core.insert(key, Feature::from(db, g));
            } else if let Some(Some(m)) = g.paths().first().and_then(|p| p.first()) {
                cff.insert(key, db.mux(*m).bits.clone());
            }
        }
        Ok(PllDb { cff, core, bases })
    }

    /// `code`, LSB first, onto the CFF feature `key` of `site`.
    fn put(&self, site: Site, key: (i32, i32), code: u32, out: &mut Vec<(PllBit, bool)>) -> Result<()> {
        let bits = self
            .cff
            .get(&key)
            .with_context(|| format!("asmdb has no PLL feature {key:?}"))?;
        ensure!(
            code >> bits.len() == 0,
            "PLL feature {key:?}: code {code} wider than {} bits",
            bits.len()
        );
        let base = self.bases[site.instance()];
        for (i, &b) in bits.iter().enumerate() {
            ensure!(b as u32 >> 24 == 0xa0, "PLL feature {key:?}: bit {b:#x} is not CFF");
            let k = base + (b as u32 & 0xff_ffff);
            out.push((PllBit::Rep(cff_bit(k)?), code >> i & 1 == 1));
        }
        Ok(())
    }

    /// One-hot `index` of the instance's path of core feature `key`.
    fn put_core(&self, site: Site, key: (i32, i32), code: u32, out: &mut Vec<(PllBit, bool)>) -> Result<()> {
        let f = self
            .core
            .get(&key)
            .with_context(|| format!("asmdb has no PLL feature {key:?}"))?;
        let bits = f.paths[site.instance()].first().context("PLL core feature without a mux")?;
        for (i, &b) in bits.iter().enumerate() {
            ensure!(b >= 0, "PLL feature {key:?}: no core bit");
            out.push((PllBit::Core(b as u32), code >> i & 1 == 1));
        }
        Ok(())
    }

    pub fn bits(&self, p: &Pll) -> Result<Vec<(PllBit, bool)>> {
        use arch::*;
        let c = &p.config;
        let s = p.site;
        let mut out = Vec::new();
        let count = |x: u8| u32::from(x) % 16;
        for f in [ENABLE_PLL, USE_ENABLE_PIN, GLOCK_COUNTER_ENABLE, LOCK_SEL_GATE_CONF_DONE] {
            self.put(s, (f, -1), 1, &mut out)?;
        }
        self.put(s, (M_HIGH, -1), count(c.m.high), &mut out)?;
        self.put(s, (M_LOW, -1), count(c.m.low), &mut out)?;
        self.put(s, (M_MODE, -1), if c.m.odd { 2 } else { 0 }, &mut out)?;
        self.put(s, (M_PRESET, -1), count(c.m_initial), &mut out)?;
        self.put(s, (M_TAB_SELECT, -1), u32::from(c.m_phase), &mut out)?;
        // n = 1 is bypass; 2, 3, 4 divide, coded 3, 1, 0 (the asmdb map;
        // sweep designs n2m10, n2m16, n3m24, n4m32).
        let (n_mode, n_code) = match c.n {
            1 => (1, 0),
            2 => (0, 3),
            3 => (0, 1),
            4 => (0, 0),
            n => bail!("PLL n = {n}: n is 1 to 4"),
        };
        self.put(s, (N_MODE, -1), n_mode, &mut out)?;
        self.put(s, (N_DIVIDED_BY, -1), n_code, &mut out)?;
        let mut taps = 1u32 << c.m_phase;
        for (k, o) in c.outputs.iter().enumerate() {
            let g = k as i32;
            match o {
                Some(o) => {
                    // Bypass: mode 1, counts 0 (sweep designs, whose c1 and
                    // c2 are bypassed outputs).
                    let (mode, high, low) = match o.divider {
                        Some(d) => (if d.odd { 2 } else { 0 }, count(d.high), count(d.low)),
                        None => (1, 0, 0),
                    };
                    self.put(s, (C_MODE, g), mode, &mut out)?;
                    self.put(s, (C_HIGH, g), high, &mut out)?;
                    self.put(s, (C_LOW, g), low, &mut out)?;
                    self.put(s, (C_PRESET, g), count(o.initial), &mut out)?;
                    self.put(s, (C_PHASE, g), u32::from(o.phase), &mut out)?;
                    taps |= 1 << o.phase;
                }
                // An unused counter: bypass, 15/15, no preset.
                None => {
                    self.put(s, (C_MODE, g), 3, &mut out)?;
                    self.put(s, (C_HIGH, g), 15, &mut out)?;
                    self.put(s, (C_LOW, g), 15, &mut out)?;
                    self.put(s, (C_PRESET, g), 0, &mut out)?;
                    self.put(s, (C_PHASE, g), 0, &mut out)?;
                }
            }
        }
        // The VCO taps the counters use, one bit each (p16: c1 on tap 6).
        self.put(s, (VCO_PHASE_CONTROL, -1), taps, &mut out)?;
        let l = c.r#loop;
        let m_eff = c.m.count() * if c.vco_div2 { 2 } else { 1 };
        let lock = lock_window(m_eff);
        self.put(s, (LOCK_WINDOW, -1), lock, &mut out)?;
        self.put(s, (LOCK_WINDOW_FB, -1), lock, &mut out)?;
        self.put(s, (LOCK_MODE, -1), 3, &mut out)?;
        self.put(s, (CHARGE_PUMP, -1), charge_pump_code(l.charge_pump)?, &mut out)?;
        self.put(s, (LOOP_R, -1), loop_r_code(l.filter_r_tenths)?, &mut out)?;
        self.put(s, (LOOP_C, -1), loop_c_code(l.filter_c)?, &mut out)?;
        self.put(s, (VCO_DIV2, -1), u32::from(c.vco_div2), &mut out)?;
        let normal = c.feedback == Feedback::Normal;
        self.put(s, (USAGE_MODE, -1), u32::from(normal), &mut out)?;
        // The feedback path's compensation delay: 44 in normal mode (the
        // atom's pll_compensation_delay 5370 ps), none without.
        self.put(s, (FB_COMP_DELAY, -1), if normal { 44 } else { 0 }, &mut out)?;
        self.put_core(s, (FEEDBACK_SELECT, -1), u32::from(normal), &mut out)?;
        ensure!(p.inclk_pin < 4, "PLL clock pin index {}", p.inclk_pin);
        self.put_core(s, (CLOCK_SELECT, 0), 1 << p.inclk_pin, &mut out)?;
        // Control inputs from the core: a LAB line through a two-level
        // source mux, source = line (p2: areset on line 6, p13: pfdena on
        // line 35).
        for (key, line) in [(ARESET_INPUT, p.areset), (PFD_ENA_INPUT, p.pfdena)] {
            if let Some(line) = line {
                let f = &self.core[&(key, -1)];
                for b in f.source_bits(s.instance(), line as usize)? {
                    out.push((PllBit::Core(b as u32), true));
                }
            }
        }
        Ok(out)
    }
}

/// `CHARGE_PUMP_CURRENT` code of `charge_pump_current`.
fn charge_pump_code(ua: u16) -> Result<u32> {
    Ok(match ua {
        53 => 4,
        80 => 7,
        _ => bail!("PLL charge pump current {ua} µA: code not known"),
    })
}

/// `LOOP_FILTER_R` code of `loop_filter_r`.
fn loop_r_code(tenths: u16) -> Result<u32> {
    Ok(match tenths {
        10 => 0,
        25 => 3,
        _ => bail!("PLL loop filter R {}.{} kΩ: code not known", tenths / 10, tenths % 10),
    })
}

/// `LOOP_FILTER_C` code of `loop_filter_c`.
fn loop_c_code(pf: u16) -> Result<u32> {
    Ok(match pf {
        3 => 0,
        _ => bail!("PLL loop filter C {pf} pF: code not known"),
    })
}

/// `SELECT_LOCK_WINDOW` code (one of its asmdb map's 1, 2, 3, 4, 7, 8, 11,
/// 15) for a feedback count of `m`: 3 at m = 4, 4 to m = 6, 7 to 8, 8 to
/// 10, 11 to 16, 15 above (the m = 4-32 sweep; n doesn't change it). Below
/// m = 4 never seen.
fn lock_window(m: u32) -> u32 {
    match m {
        0..=4 => 3,
        5..=6 => 4,
        7..=8 => 7,
        9..=10 => 8,
        11..=16 => 11,
        _ => 15,
    }
}
