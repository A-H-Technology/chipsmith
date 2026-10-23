use std::fmt;
use std::str::FromStr;

/// A part chipsmith can build for. One variant per device database we have,
/// so a manifest naming anything else is rejected at parse time rather than
/// failing halfway through place-and-route.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Device {
    /// Cyclone II, the Terasic DE2's part. Database from cyclone2-re.
    Ep2c35f672c6,
}

impl Device {
    pub const ALL: &[Device] = &[Device::Ep2c35f672c6];

    pub fn part(self) -> &'static str {
        match self {
            Device::Ep2c35f672c6 => "EP2C35F672C6",
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
