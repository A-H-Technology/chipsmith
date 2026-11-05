//! Reading `quartus_sta`'s Timing Summary.

use std::path::Path;

use chipsmith_toolchain::timing::{TimingCorner, TimingSummary};

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

/// Read the Timing Summary a finished build left behind, or `None` if timing
/// analysis produced no report. The caller decides what that silence means.
pub fn read_timing(path: &Path) -> Option<TimingSummary> {
    std::fs::read_to_string(path)
        .ok()
        .map(|report| parse_sta_summary(&report))
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

    /// A build whose timing step produced nothing must be distinguishable from
    /// one that passed — the old code returned unit either way and printed
    /// nothing, so a broken [clocks] feature looked exactly like a clean build.
    #[test]
    fn a_missing_report_is_none_rather_than_an_empty_summary() {
        let missing = std::env::temp_dir().join("chipsmith-no-such-report.sta.summary");
        let _ = std::fs::remove_file(&missing);
        assert!(read_timing(&missing).is_none());
    }

    #[test]
    fn reads_a_summary_that_is_there() {
        let path = std::env::temp_dir().join(format!(
            "chipsmith-timing-{}.sta.summary",
            std::process::id()
        ));
        std::fs::write(&path, REPORT).unwrap();

        let summary = read_timing(&path).expect("report exists");
        assert_eq!(summary.worst().unwrap().slack_ns, -2.112);

        std::fs::remove_file(&path).unwrap();
    }
}
