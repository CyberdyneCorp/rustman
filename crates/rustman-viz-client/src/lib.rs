//! The producer side of the viz protocol (openspec change
//! `add-raven-visualizer`, tasks 2.1–2.2): finding the `rustman-viz`
//! companion, checking the protocol it speaks, the `rustman-viz/1`
//! handshake, and a non-blocking stream of messages for live runs.
//!
//! Shared by the CLI (`--viz`, `rustman viz`) and the TUI (`v`). The crate
//! knows nothing about requests or reports: callers map their data to
//! [`rustman_viz_protocol::Message`]s and redact it before sending (the
//! `visual-dashboards` spec, "Secrets stay redacted in the companion").

mod connect;
mod discovery;
mod stream;

pub use connect::{ConnectOptions, Connection, connect};
pub use discovery::{Discovery, probe};
pub use stream::{EXITED, Stream, StreamOptions, Warn};

use rustman_viz_protocol::PROTOCOL;

/// The companion's executable name.
pub const COMPANION: &str = if cfg!(windows) {
    "rustman-viz.exe"
} else {
    "rustman-viz"
};

/// The environment variable that names the companion explicitly.
pub const OVERRIDE_VAR: &str = "RUSTMAN_VIZ";

/// The companion's exit code when graphics cannot start.
pub const EXIT_NO_GRAPHICS: i32 = 3;

/// The flag that makes the companion print the protocols it speaks.
pub const PROTOCOL_CHECK: &str = "--protocol-check";

/// Why the companion cannot be used.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum VizError {
    #[error("Viz companion not found: {0}")]
    NotFound(String),
    #[error("Viz companion speaks {theirs}, rustman needs {PROTOCOL}; upgrade rustman-viz")]
    Rejected { theirs: String },
    #[error("Graphics unavailable (Vulkan)")]
    Graphics,
    #[error("Viz companion failed: {0}")]
    Failed(String),
}

impl VizError {
    pub(crate) fn failed(message: impl Into<String>) -> Self {
        Self::Failed(message.into())
    }
}
