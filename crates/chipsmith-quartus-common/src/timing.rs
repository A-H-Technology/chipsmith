use std::path::Path;

/// One `Type:`/`Slack:` pair from a `.sta.summary` report.
#[derive(Debug, Clone, PartialEq)]
pub struct TimingCorner {
    pub kind: String,
    pub slack_ns: f64,
}

#[derive(Debug, Default, Clone, PartialEq)]
pub struct TimingSummary {
    pub corners: Vec<TimingCorner>,
}

impl TimingSummary {
    /// The tightest corner — the one that decides whether the design meets timing.
    pub fn worst(&self) -> Option<&TimingCorner> {
        self.corners
            .iter()
            .min_by(|a, b| a.slack_ns.total_cmp(&b.slack_ns))
    }

    pub fn met(&self) -> bool {
        self.worst().is_none_or(|corner| corner.slack_ns >= 0.0)
    }
}

/// Parse `quartus_sta`'s summary report. Corners with non-numeric slack
/// (`N/A` on a fully unconstrained path) are skipped rather than guessed at.
pub fn parse_sta_summary(report: &str) -> TimingSummary {
    let mut corners = Vec::new();
    let mut kind: Option<String> = None;

    for line in report.lines() {
        let Some((label, value)) = line.split_once(':') else {
            continue;
        };
        match label.trim() {
            "Type" => kind = Some(value.trim().to_string()),
            "Slack" => {
                if let (Some(kind), Ok(slack_ns)) = (kind.take(), value.trim().parse()) {
                    corners.push(TimingCorner { kind, slack_ns });
                }
            }
            _ => {}
        }
    }

    TimingSummary { corners }
}

/// Read and report the timing result of a finished build. A design that misses
/// timing still produces a usable .sof, so this warns rather than fails — but it
/// says so loudly, because "Build complete" on a design that missed setup by 2ns
/// is exactly the trap this is here to close.
pub fn report_timing(path: &Path) {
    let Ok(report) = std::fs::read_to_string(path) else {
        return;
    };
    let summary = parse_sta_summary(&report);
    let Some(worst) = summary.worst() else {
        return;
    };

    if summary.met() {
        eprintln!(
            "Timing met — worst slack {:.3} ns ({})",
            worst.slack_ns, worst.kind
        );
    } else {
        eprintln!(
            "WARNING: timing NOT met — worst slack {:.3} ns ({})",
            worst.slack_ns, worst.kind
        );
        eprintln!("         see {}", path.display());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Abridged from a real quartus_sta run on a Cyclone V blinky
    const REPORT: &str = "\
------------------------------------------------------------
Timing Analyzer Summary
------------------------------------------------------------

Type  : Slow 1100mV 100C Model Setup 'clk'
Slack : -1.843
TNS   : -44.166

Type  : Slow 1100mV 100C Model Hold 'clk'
Slack : 0.437
TNS   : 0.000

Type  : Slow 1100mV -40C Model Setup 'clk'
Slack : -2.112
TNS   : -50.496
";

    #[test]
    fn worst_corner_is_the_lowest_slack() {
        let summary = parse_sta_summary(REPORT);
        assert_eq!(summary.corners.len(), 3);

        let worst = summary.worst().unwrap();
        assert_eq!(worst.slack_ns, -2.112);
        assert_eq!(worst.kind, "Slow 1100mV -40C Model Setup 'clk'");
        assert!(!summary.met());
    }

    #[test]
    fn all_positive_slack_meets_timing() {
        let summary = parse_sta_summary(
            "Type  : Slow Model Setup 'clk'\nSlack : 12.500\nType  : Slow Model Hold 'clk'\nSlack : 0.312\n",
        );
        assert!(summary.met());
        assert_eq!(summary.worst().unwrap().slack_ns, 0.312);
    }

    #[test]
    fn skips_corners_without_a_numeric_slack() {
        let summary = parse_sta_summary(
            "Type  : Unconstrained\nSlack : N/A\nType  : Slow Model Setup 'clk'\nSlack : 1.000\n",
        );
        assert_eq!(summary.corners.len(), 1);
        assert_eq!(summary.worst().unwrap().kind, "Slow Model Setup 'clk'");
    }

    #[test]
    fn a_report_with_no_corners_is_not_a_failure() {
        let summary = parse_sta_summary("Timing Analyzer Summary\n");
        assert!(summary.worst().is_none());
        assert!(summary.met());
    }
}
