//! A clock management tile's PLL (PLLE2_ADV) or MMCM (MMCME2_ADV), UG472.
//!
//! A design asks for clocks by ratio, as on the DE2 (ADR-0008): `XC7_PLL`
//! and `XC7_MMCM` (`synth/cells.v`) take the input period and per output a
//! multiply/divide ratio, a phase shift and a duty cycle. [`Request::solve`]
//! finds the counters the way Vivado's clocking wizard (clk_wiz 6.0) picks
//! them, as far as its choices have been observed (`bench/cmt/README.md`),
//! and synthesis writes them as the unisim's attributes ([`Config::params`]),
//! which the packer, the FASM writer, the decoder and the Vivado export
//! read back ([`Config::from_params`]).
//!
//! [`Config`] is what a bitstream holds: the pre-divider, the feedback
//! multiplier and each output's divider, phase and duty cycle, in the
//! hardware's own steps (eighths of a VCO period for phase, half VCO
//! periods for the high time), so a config that can't be encoded can't be
//! built. Its DRP fields ([`Config::features`]) are XAPP888's; the lock
//! and loop-filter tables, which depend on the multiplier only, are
//! Vivado's (`cmt_tables.txt`, read out of Vivado's bitstreams).

use std::collections::{BTreeMap, BTreeSet};

use anyhow::{bail, ensure, Context, Result};
use chipsmith_netlist::{Const, ParamValue, State};

/// Which block.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Prim {
    Pll,
    Mmcm,
}

impl Prim {
    /// The unisim synthesis makes.
    pub fn cell(self) -> &'static str {
        match self {
            Prim::Pll => "PLLE2_ADV",
            Prim::Mmcm => "MMCME2_ADV",
        }
    }

    /// The cell a design asks for this block by.
    pub fn request_cell(self) -> &'static str {
        match self {
            Prim::Pll => "XC7_PLL",
            Prim::Mmcm => "XC7_MMCM",
        }
    }

    pub fn from_cell(ty: &str) -> Option<Prim> {
        match ty {
            "PLLE2_ADV" => Some(Prim::Pll),
            "MMCME2_ADV" => Some(Prim::Mmcm),
            _ => None,
        }
    }

    pub fn from_request_cell(ty: &str) -> Option<Prim> {
        match ty {
            "XC7_PLL" => Some(Prim::Pll),
            "XC7_MMCM" => Some(Prim::Mmcm),
            _ => None,
        }
    }

    /// CLKOUT0 to CLKOUT5 (PLL) or CLKOUT6 (MMCM).
    pub fn outputs(self) -> usize {
        match self {
            Prim::Pll => 6,
            Prim::Mmcm => 7,
        }
    }

    /// The xc7z010's speed grade -1 (DS187), in hertz: VCO, phase detector
    /// and input clock ranges.
    fn vco_hz(self) -> (u64, u64) {
        match self {
            Prim::Pll => (800_000_000, 1_600_000_000),
            Prim::Mmcm => (600_000_000, 1_200_000_000),
        }
    }

    fn pfd_hz(self) -> (u64, u64) {
        match self {
            Prim::Pll => (19_000_000, 450_000_000),
            Prim::Mmcm => (10_000_000, 450_000_000),
        }
    }

    fn input_hz(self) -> (u64, u64) {
        match self {
            Prim::Pll => (19_000_000, 800_000_000),
            Prim::Mmcm => (10_000_000, 800_000_000),
        }
    }

    fn divclk_max(self) -> u8 {
        match self {
            Prim::Pll => 56,
            Prim::Mmcm => 106,
        }
    }

    /// The site type's feature prefix.
    pub fn site_prefix(self) -> &'static str {
        self.cell()
    }
}

/// A divider or multiplier in eighths: the MMCM's CLKFBOUT_MULT_F and
/// CLKOUT0_DIVIDE_F have a fractional part, every other counter is whole.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Eighths(u16);

impl Eighths {
    pub fn whole(n: u16) -> Eighths {
        Eighths(n * 8)
    }

    pub fn eighths(self) -> u16 {
        self.0
    }

    pub fn int(self) -> u16 {
        self.0 / 8
    }

    pub fn frac(self) -> u16 {
        self.0 % 8
    }

    pub fn is_whole(self) -> bool {
        self.0 % 8 == 0
    }

    pub fn value(self) -> f64 {
        f64::from(self.0) / 8.0
    }

    /// From a decimal (`39.750`): a whole number of eighths, or an error.
    fn parse(name: &str, v: f64) -> Result<Eighths> {
        let e = (v * 8.0).round();
        ensure!(
            (v * 8.0 - e).abs() < 1e-6 && (0.0..=8.0 * 128.0).contains(&e),
            "{name} = {v}: not a multiple of 1/8"
        );
        Ok(Eighths(e as u16))
    }
}

/// One output counter: divide (eighths; whole unless it's the MMCM's
/// CLKOUT0), phase in eighths of a VCO period (`8 * DELAY_TIME +
/// PHASE_MUX`), and high time in half VCO periods (`2 * HIGH_TIME +
/// EDGE`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Output {
    divide: Eighths,
    phase: u16,
    high_halves: u16,
}

/// The longest phase delay a counter holds: 63 VCO periods and 7 eighths.
const PHASE_MAX: u16 = 8 * 63 + 7;

impl Output {
    /// Fractional dividers (MMCM CLKOUT0 only, checked by [`Config::new`])
    /// run at 50% with no phase: chipsmith doesn't build XAPP888's phase
    /// arithmetic for them.
    pub fn new(divide: Eighths, phase: u16, high_halves: u16) -> Result<Output> {
        let d = divide.eighths();
        ensure!(
            d == 8 || (16..=1024).contains(&d),
            "divide {}: 1, or 2 to 128",
            divide.value()
        );
        ensure!(
            phase <= PHASE_MAX,
            "phase {phase}/8 VCO periods: at most {PHASE_MAX}"
        );
        if !divide.is_whole() {
            // No half-period high time: the counter makes 50% itself.
            ensure!(
                phase == 0 && high_halves == 0,
                "fractional divide {}: 50% duty cycle and no phase only",
                divide.value()
            );
        } else if d == 8 {
            // No count: the VCO itself, high one period of two halves.
            ensure!(high_halves == 1, "divide 1: 50% duty cycle only");
            ensure!(phase < 8, "divide 1: phase under one VCO period");
        } else {
            let o = divide.int();
            ensure!(
                (2..2 * o).contains(&high_halves),
                "divide {o}: high time {high_halves}/2 VCO periods: 1 to {}",
                2 * o - 1
            );
            // HIGH_TIME and LOW_TIME are 6 bits (0 is 64). Vivado writes a
            // longer one cut to 6 bits (a 112 at 79% reads back as 48,
            // tests/cmt.rs), which chipsmith won't.
            let high = high_halves / 2;
            ensure!(
                high <= 64 && o - high <= 64,
                "divide {o}: high time {high} and low time {}: at most 64 VCO periods each",
                o - high
            );
        }
        Ok(Output {
            divide,
            phase,
            high_halves,
        })
    }

    pub fn divide(&self) -> Eighths {
        self.divide
    }

    pub fn phase_steps(&self) -> u16 {
        self.phase
    }

    /// Phase in degrees of this output's period, Vivado's CLKOUT<k>_PHASE.
    pub fn phase_degrees(&self) -> f64 {
        f64::from(self.phase) * 360.0 / f64::from(self.divide.eighths())
    }

    pub fn duty(&self) -> f64 {
        if self.divide.is_whole() {
            f64::from(self.high_halves) * 4.0 / f64::from(self.divide.eighths())
        } else {
            0.5
        }
    }

    /// 50%, no phase: a whole divider's high time is half its count (an
    /// odd one's in half periods), a fractional one has none of its own.
    pub fn even(divide: Eighths) -> Result<Output> {
        let halves = if divide.is_whole() { divide.int() } else { 0 };
        Output::new(divide, 0, halves)
    }

    pub fn high_halves(&self) -> u16 {
        self.high_halves
    }
}

/// Where CLKFBIN comes from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Compensation {
    /// CLKFBOUT through a BUFG back to CLKFBIN, so the outputs' clock tree
    /// delay is taken out (Vivado's ZHOLD, its wizard's default).
    Zhold,
    /// CLKFBOUT to CLKFBIN inside the tile (Vivado's INTERNAL).
    Internal,
}

impl Compensation {
    pub fn name(self) -> &'static str {
        match self {
            Compensation::Zhold => "ZHOLD",
            Compensation::Internal => "INTERNAL",
        }
    }
}

/// What a PLL's or MMCM's bits configure.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Config {
    prim: Prim,
    divclk: u8,
    mult: Eighths,
    /// [`Prim::outputs`] long; none for an output that's off.
    outputs: Vec<Option<Output>>,
    compensation: Compensation,
}

impl Config {
    pub fn new(
        prim: Prim,
        divclk: u8,
        mult: Eighths,
        outputs: Vec<Option<Output>>,
        compensation: Compensation,
    ) -> Result<Config> {
        let name = prim.cell();
        ensure!(
            (1..=prim.divclk_max()).contains(&divclk),
            "{name}: DIVCLK_DIVIDE {divclk}: 1 to {}",
            prim.divclk_max()
        );
        ensure!(
            (16..=512).contains(&mult.eighths()),
            "{name}: CLKFBOUT_MULT {}: 2 to 64",
            mult.value()
        );
        ensure!(
            prim == Prim::Mmcm || mult.is_whole(),
            "{name}: CLKFBOUT_MULT {}: whole on a PLL",
            mult.value()
        );
        ensure!(
            outputs.len() == prim.outputs(),
            "{name}: {} outputs, not {}",
            prim.outputs(),
            outputs.len()
        );
        for (k, o) in outputs.iter().enumerate() {
            if let Some(o) = o {
                ensure!(
                    o.divide.is_whole() || (prim == Prim::Mmcm && k == 0),
                    "{name}: CLKOUT{k}_DIVIDE {}: only the MMCM's CLKOUT0 divides by a fraction",
                    o.divide.value()
                );
            }
        }
        Ok(Config {
            prim,
            divclk,
            mult,
            outputs,
            compensation,
        })
    }

    pub fn prim(&self) -> Prim {
        self.prim
    }

    pub fn divclk(&self) -> u8 {
        self.divclk
    }

    pub fn mult(&self) -> Eighths {
        self.mult
    }

    pub fn outputs(&self) -> &[Option<Output>] {
        &self.outputs
    }

    pub fn compensation(&self) -> Compensation {
        self.compensation
    }

    /// The VCO's frequency over the input's.
    pub fn vco_ratio(&self) -> f64 {
        self.mult.value() / f64::from(self.divclk)
    }

    /// Output `k`'s frequency over the input's.
    pub fn output_ratio(&self, k: usize) -> Option<f64> {
        self.outputs[k].map(|o| self.vco_ratio() / o.divide.value())
    }

    /// One line for the build's log: the counters, and what they make of a
    /// `clkin_period_ps` input.
    pub fn summary(&self, clkin_period_ps: u32) -> String {
        let fin = 1e6 / f64::from(clkin_period_ps);
        let mut out = format!(
            "CLKFBOUT_MULT {}, DIVCLK_DIVIDE {}, VCO {:.3} MHz",
            self.mult.value(),
            self.divclk,
            fin * self.vco_ratio()
        );
        for (k, o) in self.outputs.iter().enumerate() {
            let Some(o) = o else { continue };
            out += &format!(
                "; CLKOUT{k} /{} {:.3} MHz",
                o.divide.value(),
                fin * self.output_ratio(k).unwrap_or(0.0)
            );
            if o.phase != 0 {
                out += &format!(" {:.3} deg", o.phase_degrees());
            }
            if o.divide.is_whole() && o.high_halves != o.divide.int() {
                out += &format!(" {:.1}% high", o.duty() * 100.0);
            }
        }
        out
    }

    /// The unisim's attributes, as Vivado names them (UG953).
    /// `clkin_period_ps`: CLKIN1_PERIOD, where known (a decoded block's
    /// isn't in its bits).
    pub fn params(&self, clkin_period_ps: Option<u32>) -> BTreeMap<String, ParamValue> {
        let s = |v: String| ParamValue::Str(v);
        let mut p = BTreeMap::new();
        p.insert("BANDWIDTH".into(), s("OPTIMIZED".into()));
        p.insert("COMPENSATION".into(), s(self.compensation.name().into()));
        if let Some(ps) = clkin_period_ps {
            p.insert(
                "CLKIN1_PERIOD".into(),
                s(format!("{:.3}", f64::from(ps) / 1000.0)),
            );
        }
        p.insert("DIVCLK_DIVIDE".into(), int_param(u64::from(self.divclk)));
        match self.prim {
            Prim::Pll => p.insert(
                "CLKFBOUT_MULT".into(),
                int_param(u64::from(self.mult.int())),
            ),
            Prim::Mmcm => p.insert(
                "CLKFBOUT_MULT_F".into(),
                s(format!("{:.3}", self.mult.value())),
            ),
        };
        p.insert("CLKFBOUT_PHASE".into(), s("0.000".into()));
        for (k, o) in self.outputs.iter().enumerate() {
            let Some(o) = o else { continue };
            if self.prim == Prim::Mmcm && k == 0 {
                p.insert(
                    "CLKOUT0_DIVIDE_F".into(),
                    s(format!("{:.3}", o.divide.value())),
                );
            } else {
                p.insert(
                    format!("CLKOUT{k}_DIVIDE"),
                    int_param(u64::from(o.divide.int())),
                );
            }
            p.insert(
                format!("CLKOUT{k}_PHASE"),
                s(format!("{:.3}", o.phase_degrees())),
            );
            p.insert(
                format!("CLKOUT{k}_DUTY_CYCLE"),
                s(format!("{:.6}", o.duty())),
            );
        }
        p
    }

    /// From the attributes [`Config::params`] writes (a cell's `used`
    /// outputs are the ones it drives).
    pub fn from_params(
        prim: Prim,
        params: &BTreeMap<String, ParamValue>,
        used: &[bool],
    ) -> Result<Config> {
        let name = prim.cell();
        let num = |k: &str, default: f64| -> Result<f64> {
            match params.get(k) {
                None => Ok(default),
                Some(ParamValue::Str(v)) => v
                    .trim()
                    .trim_matches('"')
                    .parse()
                    .with_context(|| format!("{name} {k} = {v:?}")),
                Some(ParamValue::Bits(c)) => Ok(c.0.iter().enumerate().fold(0u64, |v, (i, b)| {
                    if *b == State::One {
                        v | 1 << i
                    } else {
                        v
                    }
                }) as f64),
            }
        };
        let compensation = match params.get("COMPENSATION") {
            None => Compensation::Zhold,
            Some(ParamValue::Str(v)) => match v.trim_matches('"') {
                "ZHOLD" => Compensation::Zhold,
                "INTERNAL" => Compensation::Internal,
                v => bail!("{name} COMPENSATION {v:?}: ZHOLD or INTERNAL"),
            },
            Some(v) => bail!("{name} COMPENSATION {v:?}"),
        };
        if let Some(ParamValue::Str(v)) = params.get("BANDWIDTH") {
            ensure!(
                v.trim_matches('"') == "OPTIMIZED",
                "{name} BANDWIDTH {v}: only OPTIMIZED"
            );
        }
        let mult_name = if prim == Prim::Pll {
            "CLKFBOUT_MULT"
        } else {
            "CLKFBOUT_MULT_F"
        };
        let mult = Eighths::parse(mult_name, num(mult_name, 5.0)?)?;
        ensure!(
            num("CLKFBOUT_PHASE", 0.0)? == 0.0,
            "{name} CLKFBOUT_PHASE: only 0"
        );
        let divclk = num("DIVCLK_DIVIDE", 1.0)?;
        ensure!(
            divclk.fract() == 0.0 && divclk >= 1.0 && divclk <= 255.0,
            "{name} DIVCLK_DIVIDE {divclk}"
        );
        let mut outputs = Vec::new();
        for k in 0..prim.outputs() {
            if !used.get(k).copied().unwrap_or(false) {
                outputs.push(None);
                continue;
            }
            let dname = if prim == Prim::Mmcm && k == 0 {
                "CLKOUT0_DIVIDE_F".to_string()
            } else {
                format!("CLKOUT{k}_DIVIDE")
            };
            let divide = Eighths::parse(&dname, num(&dname, 1.0)?)?;
            let degrees = num(&format!("CLKOUT{k}_PHASE"), 0.0)?.rem_euclid(360.0);
            let steps = degrees * f64::from(divide.eighths()) / 360.0;
            ensure!(
                (steps - steps.round()).abs() * 360.0 / f64::from(divide.eighths()) < 2e-3,
                "{name} CLKOUT{k}_PHASE {degrees}: not a multiple of {:.4} degrees",
                360.0 / f64::from(divide.eighths())
            );
            let steps = steps.round() as u16 % divide.eighths().max(1);
            let duty = num(&format!("CLKOUT{k}_DUTY_CYCLE"), 0.5)?;
            let halves = if divide.is_whole() {
                duty * f64::from(divide.eighths()) / 4.0
            } else {
                0.0
            };
            ensure!(
                (halves - halves.round()).abs() < 1e-3
                    && (divide.is_whole() || (duty - 0.5).abs() < 1e-6),
                "{name} CLKOUT{k}_DUTY_CYCLE {duty}: not a multiple of half a VCO period"
            );
            outputs.push(Some(
                Output::new(divide, steps, halves.round() as u16)
                    .with_context(|| format!("{name} CLKOUT{k}"))?,
            ));
        }
        Config::new(prim, divclk as u8, mult, outputs, compensation)
    }

    /// The FASM features of the block's site (relative to
    /// `<tile>.<PLLE2_ADV|MMCME2_ADV>.`), one per set bit: `NAME[i]` for a
    /// field, `NAME` for prjxray's single-bit features. `reset`: RST is
    /// routed from a signal.
    ///
    /// Inputs tied to a constant are left unrouted, as Vivado leaves them:
    /// an unrouted pin reads 1, and its inverter (its `ZINV_` bit; set is
    /// inverted, nextpnr's reading too) makes that the 0 that RST, PWRDWN,
    /// PSEN and PSINCDEC are tied to. CLKINSEL reads 1 un-inverted: CLKIN1.
    pub fn features(&self, reset: bool) -> Result<Vec<String>> {
        let mut f = Features::default();
        f.scalar("IN_USE");
        f.scalar("ZINV_PWRDWN");
        if !reset {
            f.scalar("ZINV_RST");
        }
        if self.prim == Prim::Mmcm {
            f.scalar("ZINV_PSEN");
            f.scalar("ZINV_PSINCDEC");
        }
        let tables = tables()?;
        let key = (self.prim, self.mult.int());
        let t = tables.get(&key).with_context(|| {
            format!(
                "no lock and filter table for {} at CLKFBOUT_MULT {}",
                self.prim.cell(),
                self.mult.int()
            )
        })?;
        for (feature, width, value) in t {
            f.vector(feature, *width, *value);
        }
        for c in constant_features(self.prim) {
            f.0.insert(c.to_string());
        }
        f.scalar(compensation_feature(self.prim, self.compensation));
        // DIVCLK: no phase, 50%.
        let d = u16::from(self.divclk);
        let div = Counter::whole(d, d, 0);
        f.vector("DIVCLK_DIVCLK_HIGH_TIME", 6, u64::from(div.high));
        f.vector("DIVCLK_DIVCLK_LOW_TIME", 6, u64::from(div.low));
        f.flag("DIVCLK_DIVCLK_EDGE", div.edge);
        f.flag("DIVCLK_DIVCLK_NO_COUNT", div.no_count);
        // CLKFBOUT: the multiplier, 50%, no phase.
        let fb = Output::even(self.mult)?;
        self.counter(&mut f, "CLKFBOUT", Some(&fb));
        for k in 0..self.prim.outputs() {
            self.counter(&mut f, &format!("CLKOUT{k}"), self.outputs[k].as_ref());
        }
        Ok(f.0.into_iter().collect())
    }

    fn counter(&self, f: &mut Features, name: &str, o: Option<&Output>) {
        // An MMCM's CLKOUT5 and CLKOUT6 second registers are shared with
        // CLKOUT0's and CLKFBOUT's fractional counters: prjxray names them
        // FRACTIONAL_.
        let fractional_reg2 = self.prim == Prim::Mmcm && (name == "CLKOUT5" || name == "CLKOUT6");
        let reg2 = |field: &str| {
            if fractional_reg2 {
                format!("{name}_CLKOUT2_FRACTIONAL_{field}")
            } else {
                format!("{name}_CLKOUT2_{field}")
            }
        };
        let Some(o) = o else {
            // Off: divide by 1, not counting, as Vivado leaves it.
            f.vector(&format!("{name}_CLKOUT1_HIGH_TIME"), 6, 1);
            f.vector(&format!("{name}_CLKOUT1_LOW_TIME"), 6, 1);
            f.flag(&reg2("NO_COUNT"), true);
            return;
        };
        f.bit(&format!("{name}_CLKOUT1_OUTPUT_ENABLE"));
        if o.divide.is_whole() {
            let c = Counter::whole(o.divide.int(), o.high_halves, o.phase);
            f.vector(&format!("{name}_CLKOUT1_HIGH_TIME"), 6, u64::from(c.high));
            f.vector(&format!("{name}_CLKOUT1_LOW_TIME"), 6, u64::from(c.low));
            f.vector(
                &format!("{name}_CLKOUT1_PHASE_MUX"),
                3,
                u64::from(c.phase_mux),
            );
            f.flag(&reg2("EDGE"), c.edge);
            f.flag(&reg2("NO_COUNT"), c.no_count);
            f.vector(&reg2("DELAY_TIME"), 6, u64::from(c.delay));
            // An MMCM's CLKOUT0 and CLKFBOUT copy their phase tap into the
            // fractional counter's falling-edge tap, as Vivado writes them.
            if self.prim == Prim::Mmcm && (name == "CLKOUT0" || name == "CLKFBOUT") {
                let shared = if name == "CLKOUT0" {
                    "CLKOUT5"
                } else {
                    "CLKOUT6"
                };
                f.vector(
                    &format!("{shared}_CLKOUT2_FRACTIONAL_PHASE_MUX_F"),
                    3,
                    u64::from(c.phase_mux),
                );
            }
        } else {
            // XAPP888's fractional counter, at phase 0: the integer part
            // split in a high and a low time, the eighths in FRAC, the
            // falling edge's eighths in the shared register.
            let int = o.divide.int();
            let frac = o.divide.frac();
            let even = int / 2;
            let odd = int - 2 * even;
            let odd_and_frac = 8 * odd + frac;
            let low = even - u16::from(odd_and_frac <= 9);
            let high = even - u16::from(odd_and_frac <= 8);
            let pm_fall = (4 * odd + frac / 2) & 7;
            let wf_fall = (2..=9).contains(&odd_and_frac) || (frac == 1 && int == 2);
            let wf_rise = (1..=8).contains(&odd_and_frac);
            f.vector(&format!("{name}_CLKOUT1_HIGH_TIME"), 6, u64::from(high));
            f.vector(&format!("{name}_CLKOUT1_LOW_TIME"), 6, u64::from(low));
            f.vector(&format!("{name}_CLKOUT2_FRAC"), 3, u64::from(frac));
            f.flag(&format!("{name}_CLKOUT2_EDGE"), odd == 1);
            f.bit(&format!("{name}_CLKOUT2_FRAC_EN"));
            f.flag(&format!("{name}_CLKOUT2_FRAC_WF_R"), wf_rise);
            let shared = if name == "CLKOUT0" {
                "CLKOUT5"
            } else {
                "CLKOUT6"
            };
            f.vector(
                &format!("{shared}_CLKOUT2_FRACTIONAL_PHASE_MUX_F"),
                3,
                u64::from(pm_fall),
            );
            f.flag(&format!("{shared}_CLKOUT2_FRACTIONAL_FRAC_WF_F"), wf_fall);
        }
    }
}

impl Config {
    /// From a site's set bits (`NAME[i]`, `NAME`, as [`Config::features`]
    /// writes them), by the hardware's rules: each counter's high and low
    /// times, edge and phase. Where CLKFBIN comes from is the routing's
    /// business, so `compensation` is given. Returns whether RST is used
    /// (`ZINV_RST`). Bits that aren't exactly what [`Config::features`]
    /// writes for the counters found, the lock and filter tables included,
    /// are an error: a block chipsmith can't vouch for.
    ///
    /// `routed`: the compensation the feedback's routing makes. A PLL's
    /// bits say it too, and must agree; an MMCM's are the same for both.
    pub fn from_features(
        prim: Prim,
        bits: &BTreeSet<String>,
        routed: Compensation,
    ) -> Result<(Config, bool)> {
        let compensation = routed;
        let field = |name: &str, width: u32| -> u16 {
            (0..width)
                .filter(|i| bits.contains(&format!("{name}[{i}]")))
                .fold(0, |v, i| v | 1 << i)
        };
        let flag = |name: &str| bits.contains(&format!("{name}[0]"));
        let reg2 = |c: &str, f: &str| {
            if prim == Prim::Mmcm && (c == "CLKOUT5" || c == "CLKOUT6") {
                format!("{c}_CLKOUT2_FRACTIONAL_{f}")
            } else {
                format!("{c}_CLKOUT2_{f}")
            }
        };
        // A 6-bit count of 0 is 64.
        let count = |v: u16| if v == 0 { 64 } else { v };
        let counter = |c: &str| -> Result<Output> {
            let high = field(&format!("{c}_CLKOUT1_HIGH_TIME"), 6);
            let low = field(&format!("{c}_CLKOUT1_LOW_TIME"), 6);
            if flag(&format!("{c}_CLKOUT2_FRAC_EN")) {
                let frac = field(&format!("{c}_CLKOUT2_FRAC"), 3);
                // The whole part whose split is this high and low time, odd
                // where EDGE is set.
                let odd = flag(&format!("{c}_CLKOUT2_EDGE"));
                let int = (2..=128u16)
                    .filter(|int| (int % 2 == 1) == odd)
                    .find(|&int| {
                        let even = int / 2;
                        let oaf = 8 * (int - 2 * even) + frac;
                        even.checked_sub(u16::from(oaf <= 8)) == Some(high)
                            && even.checked_sub(u16::from(oaf <= 9)) == Some(low)
                    })
                    .with_context(|| format!("{c}: fractional high {high}, low {low}"))?;
                return Output::even(Eighths(int * 8 + frac)).with_context(|| c.to_string());
            }
            let phase =
                field(&reg2(c, "DELAY_TIME"), 6) * 8 + field(&format!("{c}_CLKOUT1_PHASE_MUX"), 3);
            if flag(&reg2(c, "NO_COUNT")) {
                return Output::new(Eighths::whole(1), phase, 1).with_context(|| c.to_string());
            }
            let (h, l) = (count(high), count(low));
            let halves = 2 * h + u16::from(flag(&reg2(c, "EDGE")));
            Output::new(Eighths::whole(h + l), phase, halves).with_context(|| c.to_string())
        };
        let divclk = if flag("DIVCLK_DIVCLK_NO_COUNT") {
            1
        } else {
            count(field("DIVCLK_DIVCLK_HIGH_TIME", 6)) + count(field("DIVCLK_DIVCLK_LOW_TIME", 6))
        };
        let mult = counter("CLKFBOUT")?.divide;
        let mut outputs = Vec::new();
        for k in 0..prim.outputs() {
            let c = format!("CLKOUT{k}");
            outputs.push(if flag(&format!("{c}_CLKOUT1_OUTPUT_ENABLE")) {
                Some(counter(&c)?)
            } else {
                None
            });
        }
        let config = Config::new(
            prim,
            u8::try_from(divclk).context("DIVCLK")?,
            mult,
            outputs,
            compensation,
        )?;
        let reset = !bits.contains("ZINV_RST");
        let want: BTreeSet<String> = config.features(reset)?.into_iter().collect();
        if &want != bits {
            let extra: Vec<&String> = bits.difference(&want).take(6).collect();
            let missing: Vec<&String> = want.difference(bits).take(6).collect();
            bail!(
                "{}: bits other than chipsmith writes for these counters: set {extra:?}, not set {missing:?}",
                prim.cell()
            );
        }
        Ok((config, reset))
    }
}

/// The compensation's bit. Vivado sets an MMCM's COMP.Z_ZHOLD for ZHOLD
/// as for INTERNAL (bench/cmt's f0-f2), never prjxray's COMP.ZHOLD.
fn compensation_feature(prim: Prim, c: Compensation) -> &'static str {
    match (prim, c) {
        (Prim::Pll, Compensation::Zhold) => "COMPENSATION.ZHOLD_NO_CLKIN_BUF",
        (Prim::Pll, Compensation::Internal) => "COMPENSATION.Z_ZHOLD_OR_CLKIN_BUF",
        (Prim::Mmcm, _) => "COMP.Z_ZHOLD",
    }
}

fn int_param(v: u64) -> ParamValue {
    ParamValue::Bits(Const(
        (0..32).map(|i| State::from(v >> i & 1 == 1)).collect(),
    ))
}

/// A whole counter's DRP fields.
struct Counter {
    high: u16,
    low: u16,
    edge: bool,
    no_count: bool,
    phase_mux: u16,
    delay: u16,
}

impl Counter {
    fn whole(divide: u16, high_halves: u16, phase: u16) -> Counter {
        if divide == 1 {
            return Counter {
                high: 1,
                low: 1,
                edge: false,
                no_count: true,
                phase_mux: phase % 8,
                delay: phase / 8,
            };
        }
        let high = high_halves / 2;
        Counter {
            high,
            low: divide - high,
            edge: high_halves % 2 == 1,
            no_count: false,
            phase_mux: phase % 8,
            delay: phase / 8,
        }
    }
}

/// Set bits, as `NAME[i]`.
#[derive(Default)]
struct Features(BTreeSet<String>);

impl Features {
    fn scalar(&mut self, name: &str) {
        self.0.insert(name.to_string());
    }

    fn bit(&mut self, name: &str) {
        self.0.insert(format!("{name}[0]"));
    }

    fn flag(&mut self, name: &str, on: bool) {
        if on {
            self.bit(name);
        }
    }

    /// A 64 in a 6-bit count is 0, the hardware's 64.
    fn vector(&mut self, name: &str, width: u32, value: u64) {
        for i in 0..width {
            if value >> i & 1 == 1 {
                self.0.insert(format!("{name}[{i}]"));
            }
        }
    }
}

/// Features Vivado sets on every block of the kind, whatever its
/// configuration (`cmt_tables.txt`).
fn constant_features(prim: Prim) -> Vec<&'static str> {
    let text = include_str!("cmt_tables.txt");
    let tag = match prim {
        Prim::Pll => "PLL",
        Prim::Mmcm => "MMCM",
    };
    text.lines()
        .filter_map(|l| {
            let mut w = l.split_whitespace();
            (w.next() == Some("const") && w.next() == Some(tag))
                .then(|| w.next())
                .flatten()
        })
        .collect()
}

type Tables = BTreeMap<(Prim, u16), Vec<(String, u32, u64)>>;

/// Per block kind and whole multiplier: the multi-bit features that depend
/// on it (LKTABLE, TABLE, ...), as `table <PLL|MMCM> <mult> <feature>
/// <width> <hex>` lines in `cmt_tables.txt`.
fn tables() -> Result<&'static Tables> {
    static T: std::sync::OnceLock<Result<Tables, String>> = std::sync::OnceLock::new();
    T.get_or_init(|| {
        let mut t = Tables::new();
        for l in include_str!("cmt_tables.txt").lines() {
            let w: Vec<&str> = l.split_whitespace().collect();
            if w.first() != Some(&"table") {
                continue;
            }
            let [_, prim, mult, feature, width, hex] = w[..] else {
                return Err(format!("cmt_tables.txt: {l:?}"));
            };
            let prim = match prim {
                "PLL" => Prim::Pll,
                "MMCM" => Prim::Mmcm,
                _ => return Err(format!("cmt_tables.txt: {l:?}")),
            };
            let parse = || -> Option<(u16, u32, u64)> {
                Some((
                    mult.parse().ok()?,
                    width.parse().ok()?,
                    u64::from_str_radix(hex, 16).ok()?,
                ))
            };
            let (mult, width, value) = parse().ok_or_else(|| format!("cmt_tables.txt: {l:?}"))?;
            t.entry((prim, mult))
                .or_default()
                .push((feature.to_string(), width, value));
        }
        Ok(t)
    })
    .as_ref()
    .map_err(|e| anyhow::anyhow!("{e}"))
}

/// One output as asked for: `f_in * multiply / divide`, shifted by
/// `phase_ps` (negative: earlier), high for `duty_percent` of the period.
/// CY2_PLL's terms (ADR-0008).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OutputRequest {
    pub multiply: u32,
    pub divide: u32,
    pub phase_ps: i32,
    pub duty_percent: u32,
}

/// An `XC7_PLL` or `XC7_MMCM` cell's parameters.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Request {
    pub prim: Prim,
    pub inclk_period_ps: u32,
    /// [`Prim::outputs`] long; none for an output nothing uses.
    pub outputs: Vec<Option<OutputRequest>>,
    pub compensation: Compensation,
}

/// What [`Request::solve`] found, and what it makes.
#[derive(Debug, Clone, PartialEq)]
pub struct Solved {
    pub config: Config,
    pub vco_hz: f64,
    /// Each output's frequency.
    pub hz: Vec<Option<f64>>,
    /// Each output's shift as made, in picoseconds (0 to a period).
    pub phase_ps: Vec<Option<f64>>,
}

fn num_param(params: &BTreeMap<String, ParamValue>, k: &str) -> Result<Option<i64>> {
    Ok(match params.get(k) {
        None => None,
        Some(ParamValue::Bits(c)) => {
            // yosys' integers are 32-bit two's complement.
            let v =
                c.0.iter().enumerate().fold(
                    0u64,
                    |v, (i, b)| if *b == State::One { v | 1 << i } else { v },
                );
            Some(if c.0.len() == 32 {
                v as u32 as i32 as i64
            } else {
                v as i64
            })
        }
        Some(ParamValue::Str(s)) => Some(
            s.trim()
                .trim_matches('"')
                .parse()
                .with_context(|| format!("{k} = {s:?} is not a whole number"))?,
        ),
    })
}

impl Request {
    /// From an `XC7_PLL` or `XC7_MMCM` cell: its parameters, and which of
    /// its outputs (`CLK0`, ...) drive something.
    pub fn from_params(
        prim: Prim,
        params: &BTreeMap<String, ParamValue>,
        used: &[bool],
    ) -> Result<Request> {
        let name = prim.request_cell();
        let get = |k: &str, default: i64| -> Result<i64> {
            Ok(num_param(params, k)
                .with_context(|| format!("{name}"))?
                .unwrap_or(default))
        };
        let positive = |k: &str, default: i64| -> Result<u32> {
            let v = get(k, default)?;
            ensure!(
                v > 0 && v <= i64::from(u32::MAX),
                "{name} {k} = {v}: positive"
            );
            Ok(v as u32)
        };
        let mut outputs = Vec::new();
        for k in 0..prim.outputs() {
            if !used.get(k).copied().unwrap_or(false) {
                outputs.push(None);
                continue;
            }
            let phase = get(&format!("CLK{k}_PHASE_SHIFT"), 0)?;
            ensure!(phase.abs() < 1 << 30, "{name} CLK{k}_PHASE_SHIFT {phase}");
            let duty = positive(&format!("CLK{k}_DUTY_CYCLE"), 50)?;
            ensure!(duty < 100, "{name} CLK{k}_DUTY_CYCLE {duty}%");
            outputs.push(Some(OutputRequest {
                multiply: positive(&format!("CLK{k}_MULTIPLY_BY"), 1)?,
                divide: positive(&format!("CLK{k}_DIVIDE_BY"), 1)?,
                phase_ps: phase as i32,
                duty_percent: duty,
            }));
        }
        let compensation = match params.get("COMPENSATION") {
            None => Compensation::Zhold,
            Some(ParamValue::Str(v)) => match v.trim_matches('"') {
                "ZHOLD" => Compensation::Zhold,
                "INTERNAL" => Compensation::Internal,
                v => bail!("{name} COMPENSATION {v:?}: ZHOLD or INTERNAL"),
            },
            Some(v) => bail!("{name} COMPENSATION {v:?}: ZHOLD or INTERNAL"),
        };
        Ok(Request {
            prim,
            inclk_period_ps: positive("INCLK0_INPUT_FREQUENCY", 8_000)?,
            outputs,
            compensation,
        })
    }

    /// The counters: every output exactly `f_in * multiply / divide`, the
    /// VCO, phase detector and input inside the device's ranges. Of those,
    /// as Vivado's clocking wizard picks (bench/cmt/README.md):
    ///
    /// - the shifts and duty cycles nearest the request (each rounded to
    ///   the hardware's step, the errors summed);
    /// - PLL: the lowest VCO. Vivado passes over a VCO of exactly 800 MHz
    ///   where an output divides it by an even count, for any VCO up to
    ///   1200 MHz (12 requests; it took 800 over 1600). Its reason is its
    ///   jitter model, which isn't public; this ranks 800 as 1200.5 MHz.
    /// - MMCM: the VCO nearest 1000 MHz, fractional counters allowed.
    ///   Vivado's jitter model departs from that on some low frequencies
    ///   (25 MHz from 125: 912.5 MHz, not 1000), where chipsmith's choice
    ///   makes the same frequency with other counters;
    /// - then the smallest pre-divider.
    pub fn solve(&self) -> Result<Solved> {
        let name = self.prim.request_cell();
        let prim = self.prim;
        ensure!(
            self.outputs.len() == prim.outputs(),
            "{name}: {} outputs",
            prim.outputs()
        );
        ensure!(
            self.outputs.iter().any(Option::is_some),
            "{name}: no output is used"
        );
        let p = u128::from(self.inclk_period_ps);
        const PS: u128 = 1_000_000_000_000;
        let (ilo, ihi) = prim.input_hz();
        // f_in = PS / p.
        ensure!(
            u128::from(ilo) * p <= PS && PS <= u128::from(ihi) * p,
            "{name}: input {:.3} MHz outside {}-{} MHz",
            1e6 / self.inclk_period_ps as f64,
            ilo / 1_000_000,
            ihi / 1_000_000
        );
        let (vlo, vhi) = prim.vco_hz();
        let (plo, phi) = prim.pfd_hz();
        let m_step: u16 = if prim == Prim::Pll { 8 } else { 1 };
        #[derive(Clone)]
        struct Cand {
            key: (u64, u64, u64, u8, u64),
            config: Config,
        }
        let mut best: Option<Cand> = None;
        for d in 1..=prim.divclk_max() {
            let d128 = u128::from(d);
            // Phase detector: PS / (p d).
            if !(u128::from(plo) * p * d128 <= PS && PS <= u128::from(phi) * p * d128) {
                continue;
            }
            let mut m8 = 16u16;
            while m8 <= 512 {
                let m = u128::from(m8);
                // VCO = PS m8 / (8 p d).
                let vco_ok = u128::from(vlo) * 8 * p * d128 <= PS * m
                    && PS * m <= u128::from(vhi) * 8 * p * d128;
                if vco_ok {
                    if let Some(c) = self.candidate(d, m8) {
                        if best.as_ref().is_none_or(|b| c.0 < b.key) {
                            best = Some(Cand {
                                key: c.0,
                                config: c.1,
                            });
                        }
                    }
                }
                m8 += m_step;
            }
        }
        let Some(best) = best else {
            let asked: Vec<String> = self
                .outputs
                .iter()
                .enumerate()
                .filter_map(|(k, o)| {
                    o.map(|o| {
                        format!(
                            "CLK{k} {:.3} MHz",
                            1e6 / self.inclk_period_ps as f64 * f64::from(o.multiply)
                                / f64::from(o.divide)
                        )
                    })
                })
                .collect();
            bail!(
                "{name}: no counters make {} from {:.3} MHz exactly (VCO {}-{} MHz)",
                asked.join(", "),
                1e6 / self.inclk_period_ps as f64,
                vlo / 1_000_000,
                vhi / 1_000_000
            );
        };
        let config = best.config;
        let fin = 1e12 / f64::from(self.inclk_period_ps);
        let vco_hz = fin * config.vco_ratio();
        let hz = (0..prim.outputs())
            .map(|k| config.output_ratio(k).map(|r| r * fin))
            .collect();
        let phase_ps = config
            .outputs
            .iter()
            .map(|o| o.map(|o| f64::from(o.phase) / 8.0 * 1e12 / vco_hz))
            .collect();
        Ok(Solved {
            config,
            vco_hz,
            hz,
            phase_ps,
        })
    }

    /// The config for pre-divider `d` and multiplier `m8` eighths if every
    /// output divides exactly, with its rank (lower is better).
    fn candidate(&self, d: u8, m8: u16) -> Option<((u64, u64, u64, u8, u64), Config)> {
        let prim = self.prim;
        let p = u128::from(self.inclk_period_ps);
        let d128 = u128::from(d);
        let m = u128::from(m8);
        let mut outputs = Vec::new();
        let mut phase_err_fs: u64 = 0;
        let mut duty_err: u64 = 0;
        for (k, o) in self.outputs.iter().enumerate() {
            let Some(o) = o else {
                outputs.push(None);
                continue;
            };
            // Divide in eighths: 8 VCO / f_out = m8 divide / (d multiply).
            let num = m * u128::from(o.divide);
            let den = d128 * u128::from(o.multiply);
            if num % den != 0 {
                return None;
            }
            let o8 = num / den;
            let fractional_ok = prim == Prim::Mmcm && k == 0;
            if !(o8 == 8 || (16..=1024).contains(&o8)) || (o8 % 8 != 0 && !fractional_ok) {
                return None;
            }
            let o8 = o8 as u16;
            // Phase in eighths of a VCO period: phase_ps m8 / (d p),
            // wrapped into one output period (o8 eighths), rounded.
            let span = i128::from(o8);
            let exact_num = i128::from(o.phase_ps) * i128::try_from(m).ok()?;
            let exact_den = i128::try_from(d128 * p).ok()?;
            let mut steps = (2 * exact_num + exact_den).div_euclid(2 * exact_den);
            // Error in femtoseconds: |steps - exact| d p / m8 ps.
            let err_num = (steps * exact_den - exact_num).abs();
            phase_err_fs += (err_num * 1000 / i128::try_from(m).ok()?) as u64;
            steps = steps.rem_euclid(span);
            if steps > i128::from(PHASE_MAX) {
                return None;
            }
            let divide = Eighths(o8);
            // Duty in half VCO periods of the output's o8 / 4.
            let high_halves = if o8 == 8 || o8 % 8 != 0 {
                if o.duty_percent != 50 {
                    return None;
                }
                if o8 == 8 {
                    1
                } else {
                    0
                }
            } else {
                let halves2 = u32::from(o8 / 4) * o.duty_percent;
                let h = ((halves2 + 50) / 100).clamp(2, u32::from(o8 / 4) - 1) as u16;
                // |h / (o8/4) - duty/100| in parts per million.
                let e = (i64::from(h) * 100 - i64::from(halves2) / 1).abs() as u64 * 1_000_000
                    / (u64::from(o8 / 4) * 100);
                duty_err += e;
                h
            };
            if (steps != 0) && o8 % 8 != 0 {
                return None;
            }
            outputs.push(Some(Output::new(divide, steps as u16, high_halves).ok()?));
        }
        let config = Config::new(prim, d, Eighths(m8), outputs, self.compensation).ok()?;
        // VCO in millihertz, exactly enough to rank.
        let vco_mhz = (1_000_000_000_000u128 * 1000 * m / (8 * p * d128)) as u64;
        let rank = match prim {
            Prim::Pll => {
                let even = config
                    .outputs
                    .iter()
                    .flatten()
                    .any(|o| o.divide.int() % 2 == 0 && o.divide.int() > 1);
                if vco_mhz == 800_000_000_000 && even {
                    1_200_500_000_000
                } else {
                    vco_mhz
                }
            }
            Prim::Mmcm => vco_mhz.abs_diff(1_000_000_000_000),
        };
        Some(((phase_err_fs, duty_err, rank, d, vco_mhz), config))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn req(prim: Prim, period: u32, outs: &[(u32, u32, i32, u32)]) -> Request {
        let mut outputs = vec![None; prim.outputs()];
        for (k, &(multiply, divide, phase_ps, duty_percent)) in outs.iter().enumerate() {
            outputs[k] = Some(OutputRequest {
                multiply,
                divide,
                phase_ps,
                duty_percent,
            });
        }
        Request {
            prim,
            inclk_period_ps: period,
            outputs,
            compensation: Compensation::Zhold,
        }
    }

    fn summary(s: &Solved) -> (f64, u8, Vec<f64>) {
        (
            s.config.mult.value(),
            s.config.divclk,
            s.config
                .outputs
                .iter()
                .flatten()
                .map(|o| o.divide.value())
                .collect(),
        )
    }

    #[test]
    fn the_wizards_pll_choices() {
        // Vivado 2026.1's clk_wiz from 125 MHz (bench/cmt/README.md).
        let cases: &[(&[(u32, u32, i32, u32)], (f64, u8, &[f64]))] = &[
            (&[(1, 1, 0, 50)], (7.0, 1, &[7.0])),
            (&[(1, 2, 0, 50)], (13.0, 2, &[13.0])),
            (&[(4, 5, 0, 50)], (36.0, 5, &[9.0])),
            (&[(8, 5, 0, 50)], (8.0, 1, &[5.0])),
            (&[(2, 5, 0, 50)], (34.0, 5, &[17.0])),
            (&[(8, 25, 0, 50)], (8.0, 1, &[25.0])),
            (&[(32, 25, 0, 50)], (32.0, 5, &[5.0])),
            (&[(4, 5, 0, 50), (32, 25, 0, 50)], (32.0, 5, &[8.0, 5.0])),
            (&[(16, 25, 0, 50)], (48.0, 5, &[15.0])),
            (&[(1, 1, 0, 25)], (8.0, 1, &[8.0])),
            (
                &[(1, 1, 0, 50), (4, 5, 0, 50), (2, 5, 0, 50)],
                (8.0, 1, &[8.0, 10.0, 20.0]),
            ),
            // 62.5 MHz at 22.5 degrees, 31.25 MHz at -90.
            (
                &[(1, 2, 1000, 50), (1, 4, -8000, 50)],
                (7.0, 1, &[14.0, 28.0]),
            ),
        ];
        for (outs, want) in cases {
            let s = req(Prim::Pll, 8000, outs).solve().unwrap();
            let got = summary(&s);
            assert_eq!(
                (got.0, got.1, got.2.as_slice()),
                (want.0, want.1, want.2),
                "{outs:?}"
            );
        }
    }

    #[test]
    fn the_wizards_mmcm_choices() {
        let cases: &[(&[(u32, u32, i32, u32)], (f64, u8, &[f64]))] = &[
            (&[(1, 1, 0, 50)], (8.0, 1, &[8.0])),
            (&[(6, 5, 0, 50)], (39.75, 5, &[6.625])),
            (&[(3, 5, 0, 50)], (40.125, 5, &[13.375])),
            (&[(24, 25, 0, 50)], (39.0, 5, &[8.125])),
            (&[(5, 4, 0, 50), (5, 8, 0, 50)], (8.125, 1, &[6.5, 13.0])),
        ];
        for (outs, want) in cases {
            let s = req(Prim::Mmcm, 8000, outs).solve().unwrap();
            let got = summary(&s);
            assert_eq!(
                (got.0, got.1, got.2.as_slice()),
                (want.0, want.1, want.2),
                "{outs:?}"
            );
        }
    }

    #[test]
    fn inexact_requests_are_refused() {
        // 148.5 MHz from 125 on a PLL: no whole counters.
        assert!(req(Prim::Pll, 8000, &[(1188, 1000, 0, 50)])
            .solve()
            .is_err());
    }

    #[test]
    fn params_round_trip() {
        for outs in [
            &[(1u32, 1u32, 2000i32, 50u32), (1, 4, -3000, 30)][..],
            &[(6, 5, 0, 50)],
        ] {
            for prim in [Prim::Pll, Prim::Mmcm] {
                let Ok(s) = req(prim, 8000, outs).solve() else {
                    continue;
                };
                let used: Vec<bool> = s.config.outputs.iter().map(Option::is_some).collect();
                let back = Config::from_params(prim, &s.config.params(Some(8000)), &used).unwrap();
                assert_eq!(back, s.config);
            }
        }
    }
}
