//! What a DE2 pin is electrically: its I/O standard and drive current, a
//! weak pull-up or bus hold, which of its I/O-cell registers it uses, and
//! what the pins nothing uses do.
//!
//! Only what the DE2 can do is representable. The board powers all eight
//! I/O banks at 3.3 V, so a pin drives at 3.3 V; Quartus takes a 2.5-V
//! input in a 3.3-V bank but not a 1.8 or 1.5-V one (error 169029, tried
//! 2026-10-26). Currents are the ones cyclone2-re has decoded.
//!
//! The manifest parses into these types. The placer writes a pin's
//! settings onto its I/O cell as the Quartus assignments that ask for them
//! (`PinSettings::params`), and the unused-pin mode onto the top module,
//! so a routed design says everything its bitstream needs: the bitstream
//! (`bitstream::nextpnr`) reads them back, and cyclone2-re's
//! `cram quartus-export` hands them to Quartus as they are.

use std::collections::HashMap;
use std::fmt;

use anyhow::{bail, Context, Result};

use crate::bitstream::io as ioe;

/// 3.3-V LVTTL output currents, all of which Cyclone II has (cyclone2-re
/// decoded every one on row and column IOEs).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum LvttlCurrent {
    Ma4,
    Ma8,
    Ma12,
    Ma16,
    Ma20,
    /// Quartus' default.
    Ma24,
}

impl LvttlCurrent {
    pub const ALL: [LvttlCurrent; 6] = [
        LvttlCurrent::Ma4,
        LvttlCurrent::Ma8,
        LvttlCurrent::Ma12,
        LvttlCurrent::Ma16,
        LvttlCurrent::Ma20,
        LvttlCurrent::Ma24,
    ];

    pub fn ma(self) -> u32 {
        match self {
            LvttlCurrent::Ma4 => 4,
            LvttlCurrent::Ma8 => 8,
            LvttlCurrent::Ma12 => 12,
            LvttlCurrent::Ma16 => 16,
            LvttlCurrent::Ma20 => 20,
            LvttlCurrent::Ma24 => 24,
        }
    }

    pub fn from_ma(ma: u32) -> Option<Self> {
        Self::ALL.into_iter().find(|c| c.ma() == ma)
    }
}

/// The I/O standards a pin in a 3.3-V bank can have.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Standard {
    /// 3.3-V LVTTL, the default, at a chosen current (default 24 mA).
    Lvttl(LvttlCurrent),
    /// 3.3-V LVCMOS at its default current: Quartus picks 12 mA on a row
    /// IOE, 24 mA on a column IOE. The other currents' drive codes aren't
    /// decoded.
    Lvcmos,
    /// 2.5 V, as an input only: a 3.3-V bank can't drive at 2.5 V. An
    /// input's buffer is the same as at 3.3 V.
    V25Input,
}

impl Default for Standard {
    fn default() -> Self {
        Standard::Lvttl(LvttlCurrent::Ma24)
    }
}

/// Quartus' names (`IO_STANDARD`) of every single-ended standard
/// Cyclone II has without a reference voltage, with the supply it needs.
pub const QUARTUS_STANDARDS: [(&str, Vccio); 5] = [
    ("3.3-V LVTTL", Vccio::V33),
    ("3.3-V LVCMOS", Vccio::V33),
    ("2.5 V", Vccio::V25),
    ("1.8 V", Vccio::V18),
    ("1.5 V", Vccio::V15),
];

/// An I/O bank's supply voltage.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Vccio {
    V33,
    V25,
    V18,
    V15,
}

impl fmt::Display for Vccio {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Vccio::V33 => "3.3 V",
            Vccio::V25 => "2.5 V",
            Vccio::V18 => "1.8 V",
            Vccio::V15 => "1.5 V",
        })
    }
}

impl Standard {
    pub fn quartus_name(self) -> &'static str {
        match self {
            Standard::Lvttl(_) => "3.3-V LVTTL",
            Standard::Lvcmos => "3.3-V LVCMOS",
            Standard::V25Input => "2.5 V",
        }
    }

    /// Whether a pin of this standard can drive its pad.
    pub fn drives(self) -> bool {
        !matches!(self, Standard::V25Input)
    }

    /// The IOE model's standard and current (`None`: the standard's
    /// default for the IOE).
    pub fn ioe(self) -> (ioe::Standard, Option<u32>) {
        match self {
            Standard::Lvttl(c) => (ioe::Standard::Lvttl33, Some(c.ma())),
            Standard::Lvcmos => (ioe::Standard::Lvcmos33, None),
            Standard::V25Input => (ioe::Standard::V25, None),
        }
    }
}

/// The weak circuits that hold a pad that nothing drives.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Hash)]
pub enum Pull {
    #[default]
    None,
    /// The weak pull-up resistor.
    Up,
    /// Bus hold: keeps the last value driven. A pin has one or the other.
    BusHold,
}

/// One pin's electrical settings.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Hash)]
pub struct PinSettings {
    pub standard: Standard,
    pub pull: Pull,
}

/// The Quartus assignments (`set_instance_assignment -name`) a pin's
/// settings are written as.
pub mod assignment {
    pub const IO_STANDARD: &str = "IO_STANDARD";
    pub const CURRENT_STRENGTH: &str = "CURRENT_STRENGTH_NEW";
    pub const WEAK_PULL_UP: &str = "WEAK_PULL_UP_RESISTOR";
    pub const BUS_HOLD: &str = "ENABLE_BUS_HOLD_CIRCUITRY";
    /// The global assignment of the unused-pin mode.
    pub const UNUSED_PINS: &str = "RESERVE_ALL_UNUSED_PINS";
    /// Every one a pin's settings use, for readers that pass them on.
    pub const PIN: [&str; 4] = [IO_STANDARD, CURRENT_STRENGTH, WEAK_PULL_UP, BUS_HOLD];
}

impl PinSettings {
    /// The assignments that differ from Quartus' defaults (3.3-V LVTTL,
    /// 24 mA, neither pull-up nor bus hold), as (name, value).
    pub fn params(&self) -> Vec<(&'static str, String)> {
        let mut out = Vec::new();
        if !matches!(self.standard, Standard::Lvttl(_)) {
            out.push((
                assignment::IO_STANDARD,
                self.standard.quartus_name().to_string(),
            ));
        }
        if let Standard::Lvttl(c) = self.standard {
            if c != LvttlCurrent::Ma24 {
                out.push((assignment::CURRENT_STRENGTH, format!("{}MA", c.ma())));
            }
        }
        match self.pull {
            Pull::None => {}
            Pull::Up => out.push((assignment::WEAK_PULL_UP, "ON".into())),
            Pull::BusHold => out.push((assignment::BUS_HOLD, "ON".into())),
        }
        out
    }

    /// The settings `params` wrote, from a cell's parameters.
    pub fn from_params(params: &HashMap<String, String>) -> Result<Self> {
        let get = |k: &str| params.get(k).map(String::as_str);
        let current = get(assignment::CURRENT_STRENGTH)
            .map(|v| {
                v.strip_suffix("MA")
                    .and_then(|ma| ma.parse().ok())
                    .and_then(LvttlCurrent::from_ma)
                    .with_context(|| format!("{}: {v:?}", assignment::CURRENT_STRENGTH))
            })
            .transpose()?;
        let standard = match (get(assignment::IO_STANDARD), current) {
            (None | Some("3.3-V LVTTL"), c) => Standard::Lvttl(c.unwrap_or(LvttlCurrent::Ma24)),
            (Some("3.3-V LVCMOS"), None) => Standard::Lvcmos,
            (Some("2.5 V"), None) => Standard::V25Input,
            (Some(s), None) => bail!("I/O standard {s:?} is not one a 3.3-V bank takes"),
            (Some(s), Some(_)) => {
                bail!("a current strength on {s}, of which only the default is decoded")
            }
        };
        let on = |k: &str| -> Result<bool> {
            match get(k) {
                None => Ok(false),
                Some("ON") => Ok(true),
                Some(v) => bail!("{k}: {v:?}"),
            }
        };
        let pull = match (on(assignment::WEAK_PULL_UP)?, on(assignment::BUS_HOLD)?) {
            (false, false) => Pull::None,
            (true, false) => Pull::Up,
            (false, true) => Pull::BusHold,
            (true, true) => bail!("both a weak pull-up and bus hold"),
        };
        Ok(PinSettings { standard, pull })
    }
}

/// What pins the design doesn't use do. Driving them to ground, Quartus'
/// other choice, isn't one: on the DE2 that shorts every chip output wired
/// to an unused FPGA pin.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum UnusedPins {
    #[default]
    TriStated,
    PullUp,
    BusHold,
}

impl UnusedPins {
    pub const ALL: [UnusedPins; 3] = [
        UnusedPins::TriStated,
        UnusedPins::PullUp,
        UnusedPins::BusHold,
    ];

    /// The value of Quartus' `RESERVE_ALL_UNUSED_PINS`.
    pub fn quartus_name(self) -> &'static str {
        match self {
            UnusedPins::TriStated => "AS INPUT TRI-STATED",
            UnusedPins::PullUp => "AS INPUT TRI-STATED WITH WEAK PULL-UP",
            UnusedPins::BusHold => "AS INPUT TRI-STATED WITH BUS-HOLD",
        }
    }

    pub fn from_quartus_name(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|u| u.quartus_name() == s)
    }

    pub fn ioe(self) -> ioe::UnusedPins {
        match self {
            UnusedPins::TriStated => ioe::UnusedPins::InputTriStated,
            UnusedPins::PullUp => ioe::UnusedPins::InputPullUp,
            UnusedPins::BusHold => ioe::UnusedPins::InputBusHold,
        }
    }
}

/// Which of a pin's I/O-cell registers synthesis is asked to fill
/// (Quartus' `FAST_INPUT_REGISTER`, `FAST_OUTPUT_REGISTER`,
/// `FAST_OUTPUT_ENABLE_REGISTER`). Any combination is a request; whether
/// the design has the register to move is synthesis' to check.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Hash)]
pub struct IoRegisters {
    pub input: bool,
    pub output: bool,
    pub output_enable: bool,
}

impl IoRegisters {
    pub fn any(self) -> bool {
        self.input || self.output || self.output_enable
    }
}

/// What an I/O-cell register holds at power-up.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PowerUp {
    Low,
    High,
}

/// The cell parameters (on `CY2_IBUF`, `CY2_OBUF`, `CY2_IOBUF`) that say a
/// pin buffer's register is used, valued `PowerUp::param`; absent, the
/// register isn't.
pub mod register_param {
    pub const INPUT: &str = "INPUT_REGISTER";
    pub const OUTPUT: &str = "OUTPUT_REGISTER";
    pub const OE: &str = "OE_REGISTER";
}

impl PowerUp {
    pub fn param(self) -> &'static str {
        match self {
            PowerUp::Low => "low",
            PowerUp::High => "high",
        }
    }

    pub fn from_param(v: &str) -> Option<Self> {
        match v {
            "low" => Some(PowerUp::Low),
            "high" => Some(PowerUp::High),
            _ => None,
        }
    }

    pub fn high(self) -> bool {
        self == PowerUp::High
    }
}

/// The register a buffer's parameter `name` says it uses, if any.
pub fn register_of(params: &HashMap<String, String>, name: &str) -> Result<Option<PowerUp>> {
    params
        .get(name)
        .map(|v| PowerUp::from_param(v).with_context(|| format!("{name} = {v:?}")))
        .transpose()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settings_survive_their_params() {
        let mut all = Vec::new();
        for pull in [Pull::None, Pull::Up, Pull::BusHold] {
            for c in LvttlCurrent::ALL {
                all.push(PinSettings {
                    standard: Standard::Lvttl(c),
                    pull,
                });
            }
            for standard in [Standard::Lvcmos, Standard::V25Input] {
                all.push(PinSettings { standard, pull });
            }
        }
        for s in all {
            let params: HashMap<String, String> = s
                .params()
                .into_iter()
                .map(|(k, v)| (k.to_string(), v))
                .collect();
            assert_eq!(PinSettings::from_params(&params).unwrap(), s, "{params:?}");
        }
        // The defaults are no assignments at all.
        assert!(PinSettings::default().params().is_empty());
    }

    #[test]
    fn contradictory_params_are_rejected() {
        for bad in [
            &[
                ("WEAK_PULL_UP_RESISTOR", "ON"),
                ("ENABLE_BUS_HOLD_CIRCUITRY", "ON"),
            ][..],
            &[
                ("IO_STANDARD", "3.3-V LVCMOS"),
                ("CURRENT_STRENGTH_NEW", "8MA"),
            ],
            &[("IO_STANDARD", "1.8 V")],
            &[("CURRENT_STRENGTH_NEW", "2MA")],
        ] {
            let params = bad
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect();
            assert!(PinSettings::from_params(&params).is_err(), "{bad:?}");
        }
    }
}
