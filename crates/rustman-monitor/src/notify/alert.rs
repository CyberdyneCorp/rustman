//! What a notification says: its kind, the monitor, the rule and the
//! latest failure, as a title and a text every channel shares.

use std::time::Duration;

use jiff::Timestamp;
use rustman_core::Redactor;

use crate::definition::Monitor;
use crate::evaluate::{Notice, NoticeKind, describe, elapsed};
use crate::store::RunResult;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AlertKind {
    Firing,
    /// Still firing after a cooldown.
    Reminder,
    Resolved,
    /// `rustman monitor test-alert`.
    Test,
}

impl AlertKind {
    /// `firing`, `reminder`, `resolved` or `test`, as templates see it.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Firing => "firing",
            Self::Reminder => "reminder",
            Self::Resolved => "resolved",
            Self::Test => "test",
        }
    }
}

/// One notification.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Alert {
    pub monitor: String,
    pub environment: String,
    pub kind: AlertKind,
    /// The rule as written; `None` for a test.
    pub rule: Option<String>,
    pub at: Timestamp,
    /// The latest failure message, already redacted in the store.
    pub failure: Option<String>,
    /// How long the rule has fired (reminder) or fired (resolved).
    pub duration: Option<Duration>,
}

impl Alert {
    /// The alert for `notice`, raised by the run `latest` of `monitor`.
    pub fn from_notice(monitor: &Monitor, notice: &Notice, latest: &RunResult) -> Self {
        let (kind, duration) = match notice.kind {
            NoticeKind::Firing => (AlertKind::Firing, None),
            NoticeKind::Reminder { since } => {
                (AlertKind::Reminder, Some(elapsed(since, notice.at)))
            }
            NoticeKind::Resolved { downtime } => (AlertKind::Resolved, Some(downtime)),
        };
        let failure = (kind != AlertKind::Resolved)
            .then(|| latest.failures.first().or(latest.message.as_ref()).cloned())
            .flatten();
        Self {
            monitor: monitor.name.clone(),
            environment: monitor.environment.clone(),
            kind,
            rule: Some(notice.rule.clone()),
            at: notice.at,
            failure,
            duration,
        }
    }

    /// The clearly labeled test notification of `monitor`.
    pub fn test(monitor: &Monitor, at: Timestamp) -> Self {
        Self {
            monitor: monitor.name.clone(),
            environment: monitor.environment.clone(),
            kind: AlertKind::Test,
            rule: None,
            at,
            failure: None,
            duration: None,
        }
    }

    /// `[FIRING] api-health: consecutive_failures: 3`, `[TEST] api-health`.
    pub fn title(&self) -> String {
        let tag = match self.kind {
            AlertKind::Firing => "FIRING",
            AlertKind::Reminder => "STILL FIRING",
            AlertKind::Resolved => "RESOLVED",
            AlertKind::Test => "TEST",
        };
        match &self.rule {
            Some(rule) => format!("[{tag}] {}: {rule}", self.monitor),
            None => format!("[{tag}] {}", self.monitor),
        }
    }

    /// One or two sentences: what happened, then the latest failure.
    pub fn text(&self) -> String {
        let (monitor, env) = (&self.monitor, &self.environment);
        let rule = self.rule.as_deref().unwrap_or_default();
        let lasted = self.duration.map(describe).unwrap_or_default();
        let head = match self.kind {
            AlertKind::Firing => format!("{monitor} ({env}): {rule} is firing."),
            AlertKind::Reminder => {
                format!("{monitor} ({env}): {rule} has been firing for {lasted}.")
            }
            AlertKind::Resolved => {
                format!("{monitor} ({env}): {rule} resolved; the outage lasted {lasted}.")
            }
            AlertKind::Test => format!(
                "Test notification for monitor {monitor} ({env}) from rustman monitor test-alert."
            ),
        };
        match &self.failure {
            Some(failure) => format!("{head}\nLatest failure: {failure}"),
            None => head,
        }
    }

    /// This alert with every text field redacted.
    pub fn redacted(&self, redactor: &Redactor) -> Self {
        let clean = |text: &str| redactor.text(text).into_owned();
        Self {
            monitor: clean(&self.monitor),
            environment: clean(&self.environment),
            rule: self.rule.as_deref().map(clean),
            failure: self.failure.as_deref().map(clean),
            ..self.clone()
        }
    }
}
