//! A pin's electrical settings on a high-range (IOB33) bank: I/O standard,
//! drive, slew and pull, as Vivado's `IOSTANDARD`, `DRIVE`, `SLEW` and
//! `PULLUP`/`PULLDOWN`/`KEEPER` set them, and the IOB features each comes to
//! ([`Settings::features`]). The features are what Vivado's bitstreams read
//! back as (bench/zybo-io/sweep.py, `tests/data/iostandard.tsv`), not
//! nextpnr's table, which has LVCMOS33's 12 and 16 mA the wrong way round.

use std::fmt;

/// The single-ended standards an IOB33 bank takes, by the bank voltage
/// they need.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Standard {
    Lvcmos33,
    Lvttl,
    Lvcmos25,
    Lvcmos18,
    Lvcmos15,
    Lvcmos12,
}

impl Standard {
    pub const ALL: [Standard; 6] = [
        Standard::Lvcmos33,
        Standard::Lvttl,
        Standard::Lvcmos25,
        Standard::Lvcmos18,
        Standard::Lvcmos15,
        Standard::Lvcmos12,
    ];

    /// Vivado's name, which the manifest takes too.
    pub fn name(self) -> &'static str {
        match self {
            Standard::Lvcmos33 => "LVCMOS33",
            Standard::Lvttl => "LVTTL",
            Standard::Lvcmos25 => "LVCMOS25",
            Standard::Lvcmos18 => "LVCMOS18",
            Standard::Lvcmos15 => "LVCMOS15",
            Standard::Lvcmos12 => "LVCMOS12",
        }
    }

    pub fn parse(s: &str) -> Option<Standard> {
        Standard::ALL.into_iter().find(|t| t.name() == s)
    }

    /// The bank's VCCO in millivolts: every pin of a bank shares it.
    pub fn vcco_mv(self) -> u32 {
        match self {
            Standard::Lvcmos33 | Standard::Lvttl => 3300,
            Standard::Lvcmos25 => 2500,
            Standard::Lvcmos18 => 1800,
            Standard::Lvcmos15 => 1500,
            Standard::Lvcmos12 => 1200,
        }
    }

    /// The drives it takes on an IOB33 bank (UG471, table 1-56).
    pub fn drives(self) -> &'static [Drive] {
        use Drive::*;
        match self {
            Standard::Lvcmos33 | Standard::Lvcmos25 | Standard::Lvcmos15 => &[Ma4, Ma8, Ma12, Ma16],
            Standard::Lvttl | Standard::Lvcmos18 => &[Ma4, Ma8, Ma12, Ma16, Ma24],
            Standard::Lvcmos12 => &[Ma4, Ma8, Ma12],
        }
    }
}

impl fmt::Display for Standard {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

/// An output's drive strength.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Drive {
    Ma4,
    Ma8,
    Ma12,
    Ma16,
    Ma24,
}

impl Drive {
    pub fn ma(self) -> u32 {
        match self {
            Drive::Ma4 => 4,
            Drive::Ma8 => 8,
            Drive::Ma12 => 12,
            Drive::Ma16 => 16,
            Drive::Ma24 => 24,
        }
    }

    pub fn from_ma(ma: u32) -> Option<Drive> {
        [
            Drive::Ma4,
            Drive::Ma8,
            Drive::Ma12,
            Drive::Ma16,
            Drive::Ma24,
        ]
        .into_iter()
        .find(|d| d.ma() == ma)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Slew {
    Slow,
    Fast,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Pull {
    None,
    Up,
    Down,
    /// Holds the last driven level (Vivado's `KEEPER`).
    Keeper,
}

/// A pin's settings; the drive is always one its standard has
/// ([`Settings::new`]). The default is Vivado's: LVCMOS33, 12 mA, slow, no
/// pull.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Settings {
    standard: Standard,
    drive: Drive,
    slew: Slew,
    pull: Pull,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            standard: Standard::Lvcmos33,
            drive: Drive::Ma12,
            slew: Slew::Slow,
            pull: Pull::None,
        }
    }
}

/// A drive the standard doesn't have.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BadDrive {
    pub standard: Standard,
    pub drive: Drive,
}

impl fmt::Display for BadDrive {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let ok: Vec<String> = self
            .standard
            .drives()
            .iter()
            .map(|d| d.ma().to_string())
            .collect();
        write!(
            f,
            "{} drives {} mA, not {} mA",
            self.standard,
            ok.join(", "),
            self.drive.ma()
        )
    }
}

impl std::error::Error for BadDrive {}

impl Settings {
    /// `drive` defaults to 12 mA, which every standard has.
    pub fn new(
        standard: Standard,
        drive: Option<Drive>,
        slew: Slew,
        pull: Pull,
    ) -> Result<Settings, BadDrive> {
        let drive = drive.unwrap_or(Drive::Ma12);
        if !standard.drives().contains(&drive) {
            return Err(BadDrive { standard, drive });
        }
        Ok(Settings {
            standard,
            drive,
            slew,
            pull,
        })
    }

    pub fn standard(&self) -> Standard {
        self.standard
    }
    pub fn drive(&self) -> Drive {
        self.drive
    }
    pub fn slew(&self) -> Slew {
        self.slew
    }
    pub fn pull(&self) -> Pull {
        self.pull
    }
}

/// Which of a pin's registers go into its I/O tile (ILOGIC, OLOGIC), as
/// Vivado's `IOB TRUE` asks: the register its input feeds, the one driving
/// its output, and the one driving its output enable.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct Registers {
    pub input: bool,
    pub output: bool,
    pub output_enable: bool,
}

/// A pin's settings in the manifest.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct PinIo {
    pub settings: Settings,
    pub registers: Registers,
}

/// What a pad does with its IOB, from the netlist.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Use {
    pub input: bool,
    pub output: bool,
}

const SLEW_SLOW: &str =
    "LVCMOS12_LVCMOS15_LVCMOS18_LVCMOS25_LVCMOS33_LVTTL_SSTL135_SSTL15.SLEW.SLOW";
const SLEW_FAST: &str = "LVCMOS12_LVCMOS15_LVCMOS18_LVCMOS25_LVCMOS33_LVTTL.SLEW.FAST";
const IN_ONLY: &str =
    "LVCMOS12_LVCMOS15_LVCMOS18_LVCMOS25_LVCMOS33_LVDS_25_LVTTL_SSTL135_SSTL15_TMDS_33.IN_ONLY";
/// A bank below 2.5 V: Vivado sets this on every IOB of the bank, used or
/// not, and [`BANK_STEPDOWN`] on the bank's HCLK_IOI3 tile.
pub const STEPDOWN: &str = "LVCMOS12_LVCMOS15_LVCMOS18_SSTL135_SSTL15.STEPDOWN";
pub const BANK_STEPDOWN: &str = "STEPDOWN";

impl Settings {
    /// Whether the bank needs its stepdown (VCCO under 2.5 V).
    pub fn stepdown(&self) -> bool {
        self.standard.vcco_mv() < 2500
    }

    /// The drive feature: the bit groups overlap, so a name stands for
    /// the setting Vivado's bitstream reads back as, e.g. LVCMOS33 at 12 mA
    /// is `LVCMOS33_LVTTL.DRIVE.I12_I16` and at 16 mA `LVCMOS33.DRIVE.I16`.
    fn drive_feature(&self) -> &'static str {
        use Drive::*;
        use Standard::*;
        match (self.standard, self.drive) {
            (Lvcmos33 | Lvttl, Ma4) => "LVCMOS33_LVTTL.DRIVE.I4",
            (Lvcmos33 | Lvttl, Ma8) | (Lvttl, Ma12) => "LVCMOS33_LVTTL.DRIVE.I12_I8",
            (Lvcmos33, Ma12) | (Lvttl, Ma16) => "LVCMOS33_LVTTL.DRIVE.I12_I16",
            (Lvcmos33, Ma16) => "LVCMOS33.DRIVE.I16",
            (Lvttl, Ma24) => "LVTTL.DRIVE.I24",
            (Lvcmos25 | Lvcmos18 | Lvcmos15, Ma4) => "LVCMOS15_LVCMOS18_LVCMOS25.DRIVE.I4",
            (Lvcmos25 | Lvcmos12, Ma8) => "LVCMOS12_LVCMOS25.DRIVE.I8",
            (Lvcmos25, Ma12) => "LVCMOS25.DRIVE.I12",
            (Lvcmos25, Ma16) => "LVCMOS25.DRIVE.I16",
            (Lvcmos18, Ma8 | Ma12) => "LVCMOS18.DRIVE.I12_I8",
            (Lvcmos18, Ma16) => "LVCMOS18.DRIVE.I16",
            (Lvcmos18, Ma24) => "LVCMOS18.DRIVE.I24",
            (Lvcmos15, Ma8) => "LVCMOS15.DRIVE.I8",
            (Lvcmos15, Ma12) => "LVCMOS15.DRIVE.I12",
            (Lvcmos15, Ma16) => "LVCMOS15_SSTL15.DRIVE.I16_I_FIXED",
            (Lvcmos12, Ma4) => "LVCMOS12.DRIVE.I4",
            (Lvcmos12, Ma12) => "LVCMOS12.DRIVE.I12",
            (s, d) => unreachable!("{s} at {} mA: Settings::new refuses it", d.ma()),
        }
    }

    /// The IOB's features, each under its `IOB_Y<n>.` prefix; the bank's
    /// stepdown ([`STEPDOWN`]) aside.
    pub fn features(&self, u: Use) -> Vec<&'static str> {
        let mut f = Vec::new();
        if u.output {
            f.push(self.drive_feature());
            f.push(match self.slew {
                Slew::Slow => SLEW_SLOW,
                Slew::Fast => SLEW_FAST,
            });
        }
        if u.input {
            f.push(if self.stepdown() {
                "LVCMOS12_LVCMOS15_LVCMOS18.IN"
            } else {
                "LVCMOS25_LVCMOS33_LVTTL.IN"
            });
            if !u.output {
                f.push(IN_ONLY);
            }
        }
        f.push(match self.pull {
            Pull::None => "PULLTYPE.NONE",
            Pull::Up => "PULLTYPE.PULLUP",
            Pull::Down => "PULLTYPE.PULLDOWN",
            Pull::Keeper => "PULLTYPE.KEEPER",
        });
        f
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Vivado's bitstreams of bench/zybo-io/sweep.py, read back by
    /// bit2fasm: one pin per line, its settings and its IOB's features.
    const SWEEP: &str = include_str!("../tests/data/iostandard.tsv");

    /// Names bit2fasm reads for zero-encoded fields, set or not: a pin
    /// without an output has its slew bits clear, which read as fast.
    fn significant(f: &str, output: bool) -> bool {
        f != "IN_TERM.NONE" && (output || f != SLEW_FAST)
    }

    #[test]
    fn features_are_vivados() {
        let mut lines = 0;
        for line in SWEEP.lines().skip(1) {
            let c: Vec<&str> = line.split('\t').collect();
            let [_run, pin, dir, standard, drive, slew, pull, features] = c[..] else {
                panic!("{line:?}");
            };
            let u = Use {
                input: dir != "out",
                output: dir != "in",
            };
            let settings = Settings::new(
                Standard::parse(standard).unwrap(),
                drive.parse().ok().and_then(Drive::from_ma),
                if slew == "FAST" {
                    Slew::Fast
                } else {
                    Slew::Slow
                },
                match pull {
                    "NONE" => Pull::None,
                    "PULLUP" => Pull::Up,
                    "PULLDOWN" => Pull::Down,
                    "KEEPER" => Pull::Keeper,
                    p => panic!("{p}"),
                },
            )
            .unwrap();
            let mut ours: Vec<&str> = settings.features(u);
            if settings.stepdown() {
                ours.push(STEPDOWN);
            }
            ours.sort();
            let mut vivado: Vec<&str> = features
                .split(' ')
                .filter(|f| significant(f, u.output))
                .collect();
            vivado.sort();
            assert_eq!(
                ours, vivado,
                "{pin}: {dir} {standard} {drive} {slew} {pull}"
            );
            lines += 1;
        }
        assert_eq!(lines, 91, "pins in the sweep");
    }
}
