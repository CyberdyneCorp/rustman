//! The viz protocol (openspec change `add-raven-visualizer`, design D2):
//! the messages `rustman` writes to the `rustman-viz` companion's stdin, one
//! JSON object per line, and the few the companion writes back.
//!
//! This crate holds serde types and pure framing helpers only: no I/O, no
//! process handling. Producers (`rustman-cli`, `rustman-tui`) and the
//! companion share it so both sides agree on the wire format.
//!
//! Versioning: the protocol is named `rustman-viz/<major>`. The major
//! version changes only on breaking changes; additions are new optional
//! fields or new message types, and readers ignore what they do not know
//! (unknown fields are skipped, unknown `type` values decode as
//! [`Message::Unknown`]).

mod frame;
mod message;

pub use frame::{
    FrameError, LineDecoder, MAX_LINE_BYTES, decode_line, encode_inspect, encode_line,
};
pub use message::{
    Assertions, BenchFinished, BenchThreshold, BenchTick, BlobRef, Body, CallTrace, ChainBlock,
    Close, ErrorMessage, ExchangeCompleted, ExplorationFinished, ExplorationStarted, Finding,
    Header, Hello, HelloAck, HelloReject, HourOutcomes, InspectResponse, LatencyPoint, LoadReport,
    Message, MonitorTrend, OpenView, Outcome, ProbeCompleted, ProbeLatency, ReportKind,
    RunFinished, RunStarted, RunState, RunSummary, Severity, TraceNode, View,
};

/// The protocol this build speaks.
pub const PROTOCOL: &str = "rustman-viz/1";

/// The major version of [`PROTOCOL`].
pub const MAJOR: u32 = 1;

/// The JSON Schema (draft 2020-12) of every message of [`PROTOCOL`].
pub const SCHEMA: &str = include_str!("../schema/rustman-viz-1.schema.json");

/// The `error` code a companion sends when it cannot start its graphics
/// (it then exits with code 3).
pub const VULKAN_UNAVAILABLE: &str = "vulkan_unavailable";

/// The major version of a protocol name such as `rustman-viz/1`.
pub fn major(protocol: &str) -> Option<u32> {
    let version = protocol.strip_prefix("rustman-viz/")?;
    let major = version.split('.').next()?;
    major.parse().ok()
}

/// Whether a peer speaking `protocol` understands this build.
pub fn is_compatible(protocol: &str) -> bool {
    major(protocol) == Some(MAJOR)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn protocol_names_carry_the_major_version() {
        assert_eq!(major(PROTOCOL), Some(MAJOR));
        assert_eq!(major("rustman-viz/2"), Some(2));
        assert_eq!(major("rustman-viz/1.3"), Some(1));
        assert_eq!(major("other/1"), None);
        assert_eq!(major("rustman-viz/x"), None);
        assert!(is_compatible("rustman-viz/1"));
        assert!(!is_compatible("rustman-viz/2"));
    }
}
