//! The notifications stream (mcp-client spec "Logging and notifications
//! stream"): every server notification of a session, timestamped, kept in a
//! bounded log and broadcast to whoever listens (the TUI's Notifications
//! tab, a call collecting what arrived while it ran). Text from the server
//! is redacted before it is stored.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::SystemTime;

use rmcp::model::{
    CancelledNotificationParam, LoggingLevel, LoggingMessageNotificationParam, NumberOrString,
    ProgressNotificationParam, ProgressToken,
};
use serde::Serialize;
use serde_json::Value;
use tokio::sync::broadcast;

use super::shared::SharedRedactor;

/// The most notifications a session keeps.
pub const LOG_CAPACITY: usize = 1000;

/// The most notifications recorded with one call (request-history).
pub const MAX_CALL_NOTIFICATIONS: usize = 200;

/// One server notification.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum NotificationKind {
    Log {
        level: String,
        logger: Option<String>,
        data: Value,
    },
    Progress {
        token: String,
        progress: f64,
        total: Option<f64>,
        message: Option<String>,
    },
    ToolsListChanged,
    ResourcesListChanged,
    PromptsListChanged,
    ResourceUpdated {
        uri: String,
        /// Whether the session is subscribed to `uri`, so a re-read is offered.
        subscribed: bool,
    },
    Cancelled {
        request_id: Option<String>,
        reason: Option<String>,
    },
    Other {
        method: String,
    },
}

/// A notification and when it arrived.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Notification {
    #[serde(with = "unix_millis")]
    pub at: SystemTime,
    #[serde(flatten)]
    pub kind: NotificationKind,
}

impl Notification {
    /// The resource to offer a re-read of: a subscribed resource that
    /// changed.
    pub fn reread_uri(&self) -> Option<&str> {
        match &self.kind {
            NotificationKind::ResourceUpdated {
                uri,
                subscribed: true,
            } => Some(uri),
            _ => None,
        }
    }

    /// One line for the stream view.
    pub fn summary(&self) -> String {
        match &self.kind {
            NotificationKind::Log {
                level,
                logger,
                data,
            } => {
                let text = data
                    .as_str()
                    .map_or_else(|| data.to_string(), str::to_owned);
                match logger {
                    Some(logger) => format!("log {level} [{logger}] {text}"),
                    None => format!("log {level} {text}"),
                }
            }
            NotificationKind::Progress {
                progress, total, ..
            } => match total {
                Some(total) => format!("progress {progress}/{total}"),
                None => format!("progress {progress}"),
            },
            NotificationKind::ToolsListChanged => "tools list changed".to_owned(),
            NotificationKind::ResourcesListChanged => "resources list changed".to_owned(),
            NotificationKind::PromptsListChanged => "prompts list changed".to_owned(),
            NotificationKind::ResourceUpdated { uri, .. } => format!("resource updated {uri}"),
            NotificationKind::Cancelled { reason, .. } => {
                format!("cancelled {}", reason.as_deref().unwrap_or_default())
            }
            NotificationKind::Other { method } => method.clone(),
        }
    }
}

/// The log of one session; clones share it.
#[derive(Clone)]
pub struct Notifications {
    inner: Arc<Inner>,
}

struct Inner {
    log: Mutex<VecDeque<Notification>>,
    sender: broadcast::Sender<Notification>,
    redactor: SharedRedactor,
}

impl std::fmt::Debug for Notifications {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Notifications").finish_non_exhaustive()
    }
}

impl Notifications {
    pub(crate) fn new(redactor: SharedRedactor) -> Self {
        let (sender, _) = broadcast::channel(LOG_CAPACITY);
        Self {
            inner: Arc::new(Inner {
                log: Mutex::default(),
                sender,
                redactor,
            }),
        }
    }

    /// Every notification kept, oldest first.
    pub fn entries(&self) -> Vec<Notification> {
        self.lock().iter().cloned().collect()
    }

    /// Notifications from now on.
    pub fn subscribe(&self) -> broadcast::Receiver<Notification> {
        self.inner.sender.subscribe()
    }

    pub(crate) fn push(&self, kind: NotificationKind) {
        let notification = Notification {
            at: SystemTime::now(),
            kind: self.redact(kind),
        };
        {
            let mut log = self.lock();
            if log.len() == LOG_CAPACITY {
                log.pop_front();
            }
            log.push_back(notification.clone());
        }
        let _ = self.inner.sender.send(notification);
    }

    fn redact(&self, kind: NotificationKind) -> NotificationKind {
        let redactor = &self.inner.redactor.get();
        let text = |s: String| redactor.text(&s).into_owned();
        match kind {
            NotificationKind::Log {
                level,
                logger,
                data,
            } => NotificationKind::Log {
                level,
                logger: logger.map(text),
                data: redact_value(redactor, data),
            },
            NotificationKind::Progress {
                token,
                progress,
                total,
                message,
            } => NotificationKind::Progress {
                token,
                progress,
                total,
                message: message.map(text),
            },
            NotificationKind::Cancelled { request_id, reason } => NotificationKind::Cancelled {
                request_id,
                reason: reason.map(text),
            },
            other => other,
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, VecDeque<Notification>> {
        self.inner
            .log
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }
}

/// `value` with every known secret replaced, as JSON again when it still
/// parses (it does unless a secret spanned JSON syntax).
pub(crate) fn redact_value(redactor: &rustman_core::Redactor, value: Value) -> Value {
    let text = value.to_string();
    let redacted = redactor.text(&text);
    if redacted == text {
        return value;
    }
    serde_json::from_str(&redacted).unwrap_or(Value::String(redacted.into_owned()))
}

pub(crate) fn token_text(token: &ProgressToken) -> String {
    match &token.0 {
        NumberOrString::Number(n) => n.to_string(),
        NumberOrString::String(s) => s.to_string(),
    }
}

pub(crate) fn log_kind(params: LoggingMessageNotificationParam) -> NotificationKind {
    NotificationKind::Log {
        level: level_name(params.level).to_owned(),
        logger: params.logger,
        data: params.data,
    }
}

pub(crate) fn progress_kind(params: &ProgressNotificationParam) -> NotificationKind {
    NotificationKind::Progress {
        token: token_text(&params.progress_token),
        progress: params.progress,
        total: params.total,
        message: params.message.clone(),
    }
}

pub(crate) fn cancelled_kind(params: CancelledNotificationParam) -> NotificationKind {
    NotificationKind::Cancelled {
        request_id: params.request_id.map(|id| match id {
            NumberOrString::Number(n) => n.to_string(),
            NumberOrString::String(s) => s.to_string(),
        }),
        reason: params.reason,
    }
}

/// The level as written in MCP (`debug`, ..., `emergency`).
pub fn level_name(level: LoggingLevel) -> &'static str {
    match level {
        LoggingLevel::Debug => "debug",
        LoggingLevel::Info => "info",
        LoggingLevel::Notice => "notice",
        LoggingLevel::Warning => "warning",
        LoggingLevel::Error => "error",
        LoggingLevel::Critical => "critical",
        LoggingLevel::Alert => "alert",
        LoggingLevel::Emergency => "emergency",
    }
}

/// Parse a level name as the user types it.
pub fn parse_level(name: &str) -> Option<LoggingLevel> {
    serde_json::from_value(Value::String(name.to_ascii_lowercase())).ok()
}

mod unix_millis {
    use std::time::{SystemTime, UNIX_EPOCH};

    use serde::Serializer;

    pub fn serialize<S: Serializer>(at: &SystemTime, serializer: S) -> Result<S::Ok, S::Error> {
        let millis = at
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| d.as_millis() as u64);
        serializer.serialize_u64(millis)
    }
}

#[cfg(test)]
mod tests {
    use rustman_core::{Redactor, Secret};
    use serde_json::json;

    use super::*;

    #[test]
    fn notifications_are_kept_broadcast_and_redacted() {
        let redactor = Redactor::new().with(&Secret::new("t", "canary-s3cr3t".to_owned()));
        let log = Notifications::new(SharedRedactor::new(redactor));
        let mut rx = log.subscribe();
        log.push(NotificationKind::Log {
            level: "debug".into(),
            logger: None,
            data: json!({"token": "canary-s3cr3t"}),
        });
        let received = rx.try_recv().unwrap();
        assert!(!received.summary().contains("canary-s3cr3t"));
        assert!(received.summary().starts_with("log debug "));
        assert_eq!(log.entries(), vec![received]);
    }

    #[test]
    fn only_subscribed_updates_offer_a_reread() {
        let at = SystemTime::now();
        let updated = |subscribed| Notification {
            at,
            kind: NotificationKind::ResourceUpdated {
                uri: "file:///a".into(),
                subscribed,
            },
        };
        assert_eq!(updated(true).reread_uri(), Some("file:///a"));
        assert_eq!(updated(false).reread_uri(), None);
    }

    #[test]
    fn levels_round_trip() {
        assert_eq!(parse_level("DEBUG"), Some(LoggingLevel::Debug));
        assert_eq!(level_name(LoggingLevel::Warning), "warning");
        assert_eq!(parse_level("loud"), None);
    }

    #[test]
    fn the_log_is_bounded() {
        let log = Notifications::new(SharedRedactor::default());
        for _ in 0..LOG_CAPACITY + 5 {
            log.push(NotificationKind::ToolsListChanged);
        }
        assert_eq!(log.entries().len(), LOG_CAPACITY);
    }
}
