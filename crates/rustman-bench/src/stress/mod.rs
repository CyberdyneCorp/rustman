//! Stress testing (`rustman stress`, add-stress-testing): load that searches
//! for its own limit, built on the open model of the bench.
//!
//! - **Safety first** ([`gate`], [`caps`]): the shared safety policy pinned
//!   to the target (as for `rustman bench`), then the stricter stress rules:
//!   the target listed in `rustman.toml [stress] targets`, production only
//!   with `[stress] allow_production` and `--production`, never fuzzing,
//!   malformed or wire faults against production, costly families and wire
//!   faults only with their opt-ins, and the `[stress]` caps, which flags
//!   never raise. Requests go through a `GovernedSender` whose governor is
//!   built from the `[stress]` caps and never backs off on `429` (the
//!   controller measures throttling instead).
//! - **One controller over the arrival rate** ([`controller`]): every mode
//!   sets the target rate once per tick (a second) and reads the SLO verdict
//!   of each closed window (5 s): [`breakpoint`] (geometric steps, then
//!   bisection), [`spike`] (baseline, spike, baseline, recovery time) and
//!   [`soak`] (flat, with drift detection); `fuzz` holds a flat rate.
//! - **Windows and SLOs** ([`window`], [`slo`]): per-file statistics of each
//!   window, evaluated with the bench thresholds' metrics; [`classify`]
//!   names the failure mode of a broken window, [`endpoint`] the first
//!   request file to degrade, and [`saturation`] marks windows where the
//!   client, not the target, was the bottleneck.
//! - **Execution** ([`run`]): each VU is a task of its own with its own
//!   [`rustman_http::runner::Runner`], fed scheduled instants by the
//!   arrival-rate scheduler and run on worker threads, so load spreads over
//!   the cores.
//! - **Kill switch and recovery** ([`kill`], [`recovery`]): load stops once
//!   the target is down (connection errors or timeouts above half for
//!   5 s, or 3 unanswered health requests); after every mode the health
//!   request checks recovery against its pre-stress latency.
//! - **Minimization** ([`minimize`]): once the target recovered, the input
//!   of each crash-like finding is replayed alone and reduced, or marked
//!   `load-dependent`.
//! - **Results** ([`summary`], [`live`], [`report`], [`export`]): the
//!   end-of-run summary, the live view for dashboards and step lines, the
//!   `kind: stress` report kept under `.rustman/stress/<id>/`, and the
//!   regression export of its findings.
//!
//! See `docs/stress.md`.

mod after;
pub mod breakpoint;
pub mod caps;
pub mod classify;
pub mod controller;
pub mod endpoint;
mod executor;
pub mod export;
pub mod fd;
pub mod fuzz;
pub mod gate;
pub mod kill;
pub mod live;
pub mod malformed;
pub mod minimize;
mod probe;
pub mod recovery;
pub mod report;
pub mod run;
pub mod saturation;
pub mod slo;
pub mod soak;
pub mod spike;
pub mod summary;
pub mod window;
pub mod wire;
mod workers;

use std::fmt;
use std::str::FromStr;

pub use breakpoint::{BreakpointResult, BreakpointSpec, Step, StepVerdict, StopReason};
pub use classify::FailureMode;
pub use controller::{Baseline, Timing};
pub use run::{Stress, StressInput, StressOutcome};
pub use slo::Slo;
pub use soak::SoakSpec;
pub use spike::SpikeSpec;

use rustman_report::exploration::Severity;

/// A stress mode.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ModeKind {
    Breakpoint,
    Spike,
    Soak,
    Fuzz,
}

impl ModeKind {
    pub const ALL: [Self; 4] = [Self::Breakpoint, Self::Spike, Self::Soak, Self::Fuzz];

    pub fn name(self) -> &'static str {
        match self {
            Self::Breakpoint => "breakpoint",
            Self::Spike => "spike",
            Self::Soak => "soak",
            Self::Fuzz => "fuzz",
        }
    }
}

impl fmt::Display for ModeKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

impl FromStr for ModeKind {
    type Err = String;

    fn from_str(text: &str) -> Result<Self, String> {
        Self::ALL
            .into_iter()
            .find(|m| m.name() == text)
            .ok_or_else(|| {
                let names: Vec<&str> = Self::ALL.iter().map(|m| m.name()).collect();
                format!(
                    "unknown stress mode '{text}' (expected {})",
                    names.join(", ")
                )
            })
    }
}

/// A mode and its profile.
#[derive(Clone, Debug, PartialEq)]
pub enum ModeSpec {
    Breakpoint(BreakpointSpec),
    Spike(SpikeSpec),
    Soak(SoakSpec),
    /// A flat rate (fuzz mode; fuzz mixing comes with it).
    Fuzz(FlatSpec),
}

impl ModeSpec {
    pub fn kind(&self) -> ModeKind {
        match self {
            Self::Breakpoint(_) => ModeKind::Breakpoint,
            Self::Spike(_) => ModeKind::Spike,
            Self::Soak(_) => ModeKind::Soak,
            Self::Fuzz(_) => ModeKind::Fuzz,
        }
    }

    /// The rate the mode starts at: that of a fuzzing run's baseline.
    pub fn initial_rate(&self) -> f64 {
        match self {
            Self::Breakpoint(b) => b.start_rps,
            Self::Spike(s) => s.baseline_rps,
            Self::Soak(s) => s.rps,
            Self::Fuzz(f) => f.rps,
        }
    }
}

/// A flat rate for a duration.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FlatSpec {
    /// Iterations per second.
    pub rps: f64,
    pub duration: std::time::Duration,
}

/// A finding of a stress run, with a stable rule id.
#[derive(Clone, Debug, PartialEq)]
pub struct StressFinding {
    /// Such as `latency-drift`.
    pub rule: &'static str,
    pub severity: Severity,
    /// What was observed, with the numbers.
    pub message: String,
    /// The fuzz source or fault family (`deep-nesting`, `cl-te-conflict`,
    /// `mutation`, `schema`), for findings from fuzzing and wire faults.
    pub family: Option<String>,
    /// The exchange that showed it, redacted.
    pub evidence: Option<Evidence>,
    /// The request file it concerns, when one does.
    pub file: Option<String>,
    /// What replaying its input alone found (crash-like findings).
    pub minimization: Option<Minimization>,
    /// The whole body of the variant that showed it, redacted, when it is
    /// text of at most 64 KiB (for regression export).
    pub input: Option<String>,
}

impl StressFinding {
    /// A finding without family or evidence.
    pub fn new(rule: &'static str, severity: Severity, message: String) -> Self {
        Self {
            rule,
            severity,
            message,
            family: None,
            evidence: None,
            file: None,
            minimization: None,
            input: None,
        }
    }
}

/// The input of a crash-like finding, replayed alone after the load.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Minimization {
    /// The smallest body that still reproduced the symptom, redacted;
    /// `None` when the input was not JSON (or too large) to reduce.
    pub body: Option<String>,
    /// Its `Content-Type`.
    pub content_type: Option<String>,
    /// Replays sent.
    pub replays: u32,
    /// The input did not reproduce the symptom alone: it only failed
    /// under load.
    pub load_dependent: bool,
}

/// The exchange behind a finding. Every text is redacted with the
/// request's redactor before it is stored.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Evidence {
    /// `POST http://host/orders`, or the raw request line of a wire fault.
    pub request: String,
    pub headers: Vec<(String, String)>,
    /// The variant: its source and what it changed.
    pub variant: String,
    /// The start of the body sent.
    pub body: String,
    /// The status, or what happened instead.
    pub response: String,
    /// The start of the response body (at most [`RESPONSE_EXCERPT`]
    /// bytes); empty without a response or a text body.
    pub response_body: String,
    /// The response body was longer than the excerpt.
    pub response_truncated: bool,
}

/// The longest response body excerpt kept as evidence (4 KiB, as in
/// exploration reports).
pub const RESPONSE_EXCERPT: usize = 4 * 1024;

/// `12.5`, `10`, `33.75`: a rate with at most two decimals.
pub fn format_rate(rate: f64) -> String {
    let text = format!("{rate:.2}");
    text.trim_end_matches('0').trim_end_matches('.').to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn modes_parse_by_name_and_list_the_valid_ones() {
        assert_eq!("soak".parse::<ModeKind>(), Ok(ModeKind::Soak));
        assert_eq!(
            "chaos".parse::<ModeKind>().unwrap_err(),
            "unknown stress mode 'chaos' (expected breakpoint, spike, soak, fuzz)"
        );
    }

    #[test]
    fn rates_show_at_most_two_decimals() {
        assert_eq!(format_rate(10.0), "10");
        assert_eq!(format_rate(33.75), "33.75");
        assert_eq!(format_rate(75.9375), "75.94");
        assert_eq!(format_rate(22.5), "22.5");
    }
}
