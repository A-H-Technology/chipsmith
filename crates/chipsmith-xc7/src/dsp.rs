//! A DSP48E1's configuration (UG479) as one type that synthesis writes as
//! the primitive's parameters and the packer, FASM writer and decoder read
//! back ([`Config::from_params`], [`Config::params`]).
//!
//! What a bitstream configures: the register stages (A and B of 0 to 2,
//! the others on or off), where A and B come from (the pins or the
//! cascade), the D port, the pins' inversions and the pattern detector's
//! MASK and PATTERN. OPMODE, ALUMODE, INMODE and CARRYINSEL are pins.
//! Only the 48-bit ALU (USE_SIMD "ONE48") is taken.

use std::collections::BTreeMap;

use anyhow::{bail, ensure, Context, Result};
use chipsmith_netlist::{Const, ParamValue, State};

/// The register stages a DSP48E1 has beside A and B, by parameter name.
pub const STAGES: [&str; 10] = [
    "MREG",
    "PREG",
    "CREG",
    "DREG",
    "ADREG",
    "INMODEREG",
    "OPMODEREG",
    "ALUMODEREG",
    "CARRYINREG",
    "CARRYINSELREG",
];

/// The pins a DSP48E1 can invert, by their IS_<pin>_INVERTED parameter,
/// with their widths.
pub const INVERTIBLE: [(&str, usize); 5] = [
    ("ALUMODE", 4),
    ("CARRYIN", 1),
    ("CLK", 1),
    ("INMODE", 5),
    ("OPMODE", 7),
];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Config {
    /// A's and B's register stages, 0 to 2, and their cascade outputs'
    /// (ACASCREG, BCASCREG).
    pub areg: u8,
    pub breg: u8,
    pub acascreg: u8,
    pub bcascreg: u8,
    /// [`STAGES`], in order.
    pub stages: [bool; 10],
    pub a_cascade: bool,
    pub b_cascade: bool,
    pub use_dport: bool,
    /// [`INVERTIBLE`]'s inversions, in order, low bit first.
    pub inverted: [u8; 5],
    pub mask: u64,
    pub pattern: u64,
}

fn int(params: &BTreeMap<String, ParamValue>, name: &str, default: i64) -> Result<i64> {
    Ok(match params.get(name) {
        None => default,
        Some(ParamValue::Bits(c)) => {
            c.0.iter().enumerate().fold(
                0i64,
                |v, (i, s)| if *s == State::One { v | 1 << i } else { v },
            )
        }
        Some(ParamValue::Str(s)) => s
            .trim()
            .parse()
            .with_context(|| format!("{name} = {s:?}"))?,
    })
}

fn string<'a>(params: &'a BTreeMap<String, ParamValue>, name: &str, default: &'a str) -> &'a str {
    match params.get(name) {
        Some(ParamValue::Str(s)) => s.as_str(),
        _ => default,
    }
}

fn bits(v: u64, width: usize) -> ParamValue {
    ParamValue::Bits(Const(
        (0..width).map(|i| State::from(v >> i & 1 == 1)).collect(),
    ))
}

impl Config {
    pub fn from_params(params: &BTreeMap<String, ParamValue>) -> Result<Config> {
        let stage = |name: &str| -> Result<u8> {
            let v = int(params, name, 1)?;
            ensure!((0..=2).contains(&v), "{name} = {v}");
            Ok(v as u8)
        };
        let mut stages = [false; 10];
        for (s, name) in stages.iter_mut().zip(STAGES) {
            *s = match int(params, name, 1)? {
                0 => false,
                1 => true,
                v => bail!("{name} = {v}"),
            };
        }
        let input = |name: &str| -> Result<bool> {
            Ok(match string(params, name, "DIRECT") {
                "DIRECT" => false,
                "CASCADE" => true,
                v => bail!("{name} {v}"),
            })
        };
        ensure!(
            string(params, "USE_SIMD", "ONE48") == "ONE48",
            "USE_SIMD {} isn't supported yet",
            string(params, "USE_SIMD", "ONE48")
        );
        let mut inverted = [0u8; 5];
        for (v, (pin, _)) in inverted.iter_mut().zip(INVERTIBLE) {
            *v = int(params, &format!("IS_{pin}_INVERTED"), 0)? as u8;
        }
        Ok(Config {
            areg: stage("AREG")?,
            breg: stage("BREG")?,
            acascreg: stage("ACASCREG")?,
            bcascreg: stage("BCASCREG")?,
            stages,
            a_cascade: input("A_INPUT")?,
            b_cascade: input("B_INPUT")?,
            use_dport: match string(params, "USE_DPORT", "FALSE") {
                "FALSE" => false,
                "TRUE" => true,
                v => bail!("USE_DPORT {v}"),
            },
            inverted,
            mask: int(params, "MASK", 0x3fff_ffff_ffff)? as u64,
            pattern: int(params, "PATTERN", 0)? as u64,
        })
    }

    pub fn params(&self) -> BTreeMap<String, ParamValue> {
        let mut p = BTreeMap::new();
        let int = |v: u8| ParamValue::int(i64::from(v));
        p.insert("AREG".into(), int(self.areg));
        p.insert("BREG".into(), int(self.breg));
        p.insert("ACASCREG".into(), int(self.acascreg));
        p.insert("BCASCREG".into(), int(self.bcascreg));
        for (&s, name) in self.stages.iter().zip(STAGES) {
            p.insert(name.into(), int(u8::from(s)));
        }
        let input = |c: bool| ParamValue::Str(if c { "CASCADE" } else { "DIRECT" }.into());
        p.insert("A_INPUT".into(), input(self.a_cascade));
        p.insert("B_INPUT".into(), input(self.b_cascade));
        p.insert(
            "USE_DPORT".into(),
            ParamValue::Str(if self.use_dport { "TRUE" } else { "FALSE" }.into()),
        );
        p.insert("USE_MULT".into(), ParamValue::Str("MULTIPLY".into()));
        p.insert("USE_SIMD".into(), ParamValue::Str("ONE48".into()));
        for (&v, (pin, w)) in self.inverted.iter().zip(INVERTIBLE) {
            p.insert(format!("IS_{pin}_INVERTED"), bits(u64::from(v), w));
        }
        p.insert("MASK".into(), bits(self.mask, 48));
        p.insert("PATTERN".into(), bits(self.pattern, 48));
        p
    }

    pub fn stage(&self, name: &str) -> bool {
        STAGES
            .iter()
            .position(|s| *s == name)
            .is_some_and(|i| self.stages[i])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parameters_read_back() {
        let c = Config {
            areg: 1,
            breg: 0,
            acascreg: 1,
            bcascreg: 0,
            stages: [
                false, true, false, false, false, false, false, false, false, false,
            ],
            a_cascade: false,
            b_cascade: false,
            use_dport: false,
            inverted: [0, 0, 1, 0, 0],
            mask: 0x3fff_ffff_ffff,
            pattern: 0,
        };
        assert_eq!(Config::from_params(&c.params()).unwrap(), c);
        assert!(c.stage("PREG") && !c.stage("MREG"));
    }
}
