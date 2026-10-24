//! Parameter and attribute values.

use std::collections::BTreeMap;

use crate::sig::Const;

/// A parameter or attribute value: bits, or a string (`src`, `MEMID`, a
/// device cell's `"datac"`). RTLIL also flags constants signed or real;
/// yosys' JSON doesn't carry either flag, so neither does the IR.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum ParamValue {
    Bits(Const),
    Str(String),
}

impl ParamValue {
    /// A 32-bit integer, the width yosys gives integer parameters.
    pub fn int(v: i64) -> ParamValue {
        ParamValue::Bits(Const::from_int(v, 32))
    }

    pub fn bool(b: bool) -> ParamValue {
        ParamValue::Bits(Const::from_int(b as i64, 1))
    }

    pub fn bits(&self) -> Option<&Const> {
        match self {
            ParamValue::Bits(c) => Some(c),
            ParamValue::Str(_) => None,
        }
    }

    pub fn str(&self) -> Option<&str> {
        match self {
            ParamValue::Str(s) => Some(s),
            ParamValue::Bits(_) => None,
        }
    }
}

/// Attributes of a module, wire, cell or memory, by name (`src`, `keep`,
/// `init`, `top`, `blackbox`, ...). Named and valued as in yosys' JSON.
pub type Attributes = BTreeMap<String, ParamValue>;

/// The attributes the IR itself reads.
pub mod attr {
    /// The design's top module.
    pub const TOP: &str = "top";
    /// A module with an interface and no body (device cells, `-lib`).
    pub const BLACKBOX: &str = "blackbox";
    /// Power-up value of a register's output wire.
    pub const INIT: &str = "init";
    /// Not to be removed even if unused.
    pub const KEEP: &str = "keep";
    /// Source location, `file:line.col-line.col`.
    pub const SRC: &str = "src";
}

/// RTLIL's truthiness of an attribute: present with a nonzero value.
pub fn is_set(attributes: &Attributes, name: &str) -> bool {
    match attributes.get(name) {
        Some(ParamValue::Bits(c)) => c.as_bool(),
        Some(ParamValue::Str(_)) => true,
        None => false,
    }
}
