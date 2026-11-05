//! What a timing analysis concluded.
//!
//! Part of the Toolchain interface, not of any one vendor: every FPGA
//! toolchain analyses a design at several Timing Corners and reports Slack.
//! Parsing a particular vendor's report format is the backend's job.

/// One analysed process/voltage/temperature combination and its Slack.
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
    /// The tightest Corner — the one that decides whether the design meets timing.
    pub fn worst(&self) -> Option<&TimingCorner> {
        self.corners
            .iter()
            .min_by(|a, b| a.slack_ns.total_cmp(&b.slack_ns))
    }

    /// Every Corner has non-negative Slack. A summary with no Corners analysed
    /// nothing, and vacuously meets timing — callers that care about the
    /// difference should check `worst()`.
    pub fn met(&self) -> bool {
        self.worst().is_none_or(|corner| corner.slack_ns >= 0.0)
    }
}
