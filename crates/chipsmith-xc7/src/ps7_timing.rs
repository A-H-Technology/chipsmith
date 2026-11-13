//! The PS7's clocked pins as Vivado times them (`ps7_timing.tsv`): which
//! clock pin each is on, and its clock-to-out or setup.

use std::collections::HashMap;
use std::sync::OnceLock;

use chipsmith_route::timing::Ps;

#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) enum Arc {
    /// An output: valid this long after its clock.
    ClockToOut(Ps),
    /// An input: wanted this long before its clock.
    Setup(Ps),
}

/// The clock pin and arc of PS7 site pin `pin` (`MAXIGP0ARADDR5`), if
/// Vivado times it.
pub(crate) fn of(pin: &str) -> Option<(&'static str, Arc)> {
    static TABLE: OnceLock<HashMap<&'static str, (&'static str, Arc)>> = OnceLock::new();
    TABLE
        .get_or_init(|| {
            include_str!("ps7_timing.tsv")
                .lines()
                .filter(|l| !l.starts_with('#'))
                .filter_map(|l| {
                    let f: Vec<&str> = l.split('\t').collect();
                    let ps: Ps = f.get(3)?.parse().ok()?;
                    let arc = match *f.get(2)? {
                        "clock_to_out" => Arc::ClockToOut(ps),
                        "setup" => Arc::Setup(ps),
                        _ => return None,
                    };
                    Some((f[0], (f[1], arc)))
                })
                .collect()
        })
        .get(pin)
        .copied()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gp0_pins_are_on_its_clock() {
        assert_eq!(
            of("MAXIGP0ARADDR5"),
            Some(("MAXIGP0ACLK", Arc::ClockToOut(1438.0)))
        );
        assert_eq!(
            of("MAXIGP0RDATA5"),
            Some(("MAXIGP0ACLK", Arc::Setup(550.0)))
        );
        assert_eq!(of("MAXIGP0ACLK"), None);
    }
}
