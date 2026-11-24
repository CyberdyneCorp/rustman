//! What each view shows, as plain data: built from protocol messages or
//! saved reports, and pushed to the window by [`crate::app`]. Nothing here
//! touches Raven, so the dashboards' content is tested without a GPU.

pub mod bench;
pub mod exploration;
pub mod graph;
pub mod inspector;
pub mod run;
pub mod stats;

/// Where a live view stands.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Phase {
    /// Waiting for the first event, or receiving them.
    #[default]
    Running,
    Completed,
    Aborted,
    /// The producer went away before the end (stdin closed): the view
    /// keeps the last state it received.
    Disconnected,
}

impl Phase {
    pub fn text(self) -> &'static str {
        match self {
            Self::Running => "Running",
            Self::Completed => "Completed",
            Self::Aborted => "Aborted",
            Self::Disconnected => "Producer disconnected",
        }
    }
}
