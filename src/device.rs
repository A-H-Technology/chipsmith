use std::fmt;
use std::str::FromStr;

mod clg400;

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

/// A 7-series die and package as prjxray-db and nextpnr name them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Xc7Part {
    /// prjxray-db's family directory, e.g. `zynq7`.
    pub family: &'static str,
    /// The die, which names nextpnr's chip database (`chipdb-<die>.bin`).
    pub die: &'static str,
    /// The full part with speed grade, e.g. `xc7z010clg400-1`.
    pub part: &'static str,
}

pub const XC7Z010CLG400_1: Xc7Part = Xc7Part {
    family: "zynq7",
    die: "xc7z010",
    part: "xc7z010clg400-1",
};

/// A user I/O ball of the Zynq-7010 CLG400 package, e.g. `L16`. Only the
/// programmable-logic pins: PS (MIO, DDR) and configuration pins can't be
/// assigned to a port, so they don't parse.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Clg400Pin(&'static str);

impl Clg400Pin {
    pub fn parse(s: &str) -> Option<Self> {
        clg400::USER_IO
            .iter()
            .find(|(ball, _)| ball.eq_ignore_ascii_case(s))
            .map(|&(ball, _)| Clg400Pin(ball))
    }

    pub fn as_str(self) -> &'static str {
        self.0
    }
}

impl fmt::Display for Clg400Pin {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.0)
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

    #[test]
    fn only_pl_user_io_balls_are_clg400_pins() {
        // The Zybo's clock and LEDs.
        for ball in ["L16", "M14", "M15", "G14", "D18"] {
            assert_eq!(Clg400Pin::parse(ball).unwrap().as_str(), ball);
        }
        // PS_MIO48 (the USB-UART's RXD), a PS DDR pin, and no such ball.
        for ball in ["B12", "A1", "Z99"] {
            assert!(Clg400Pin::parse(ball).is_none(), "{ball}");
        }
        assert_eq!(clg400::USER_IO.len(), 100);
    }
}
