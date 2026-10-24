use std::fmt;
use std::str::FromStr;

pub use chipsmith_xc7::part::{Clg400Pin, Xc7Part, XC7Z010CLG400_1};

/// A part chipsmith can build for. One variant per device database we have,
/// so a manifest naming anything else is rejected at parse time rather than
/// failing halfway through place-and-route. The device decides the flow
/// (`flow::plan_build`) and how its pins are written (`manifest::Target`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Device {
    /// Cyclone II, the Terasic DE2's part. Database from cyclone2-re.
    Ep2c35f672c6,
    /// Zynq-7010 in the CLG400 package, speed grade -1: the Digilent Zybo's
    /// part. Database from openXC7's prjxray-db.
    Xc7z010clg400_1,
}

impl Device {
    pub const ALL: &[Device] = &[Device::Ep2c35f672c6, Device::Xc7z010clg400_1];

    pub fn part(self) -> &'static str {
        match self {
            Device::Ep2c35f672c6 => "EP2C35F672C6",
            Device::Xc7z010clg400_1 => "xc7z010clg400-1",
        }
    }
}

impl fmt::Display for Device {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.part())
    }
}

#[derive(Debug, thiserror::Error)]
#[error("unsupported device {0:?}; chipsmith knows {known}", known = Device::ALL.iter().map(|d| d.part()).collect::<Vec<_>>().join(", "))]
pub struct UnknownDevice(String);

impl FromStr for Device {
    type Err = UnknownDevice;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Device::ALL
            .iter()
            .copied()
            .find(|d| d.part().eq_ignore_ascii_case(s))
            .ok_or_else(|| UnknownDevice(s.to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parts_parse_case_insensitively() {
        assert_eq!(
            "XC7Z010CLG400-1".parse::<Device>().unwrap(),
            Device::Xc7z010clg400_1
        );
        assert!("xc7z020clg400-1".parse::<Device>().is_err());
    }
}
