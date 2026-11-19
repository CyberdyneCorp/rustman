//! One request of a session (mcp-client spec "Progress, timeout and
//! cancellation"): rmcp gives every request a `progressToken`; this module
//! waits for the answer while it tracks progress, collects the notifications
//! that arrive meanwhile, and sends `notifications/cancelled` when the
//! timeout elapses or the user cancels.

use std::time::{Duration, Instant};

use rmcp::model::{ClientRequest, PaginatedRequestParams, ServerResult};
use rmcp::service::{PeerRequestOptions, RequestHandle, ServiceError};
use rmcp::{Peer, RoleClient};
use rustman_core::Redactor;
use serde::Serialize;
use tokio::sync::{broadcast, watch};
use tokio_util::sync::CancellationToken;

use super::error::ClientError;
use super::line_limit::EndReason;
use super::notify::{
    MAX_CALL_NOTIFICATIONS, Notification, NotificationKind, Notifications, token_text,
};

/// The timeout of a request when neither the server definition nor the
/// request sets one.
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(60);

/// The most pages a paginated list follows.
pub const MAX_PAGES: usize = 100;

/// How one request is sent.
#[derive(Clone, Debug, Default)]
pub struct RequestOptions {
    /// Overrides the server's timeout (request files set it).
    pub timeout: Option<Duration>,
    /// Cancelled by the user: the request is abandoned with
    /// `notifications/cancelled`.
    pub cancel: Option<CancellationToken>,
    /// Receives the progress of the request as it is reported.
    pub progress: Option<watch::Sender<Option<Progress>>>,
}

/// The last progress a server reported for a request.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Progress {
    pub progress: f64,
    pub total: Option<f64>,
    pub message: Option<String>,
}

impl Progress {
    /// The completed share in percent, when the total is known.
    pub fn percent(&self) -> Option<f64> {
        self.total
            .filter(|t| *t > 0.0)
            .map(|t| self.progress / t * 100.0)
    }
}

/// An answered request.
#[derive(Clone, Debug)]
pub struct Exchange<T> {
    pub result: T,
    /// The notifications received while the request ran (at most
    /// [`MAX_CALL_NOTIFICATIONS`]).
    pub notifications: Vec<Notification>,
    pub elapsed: Duration,
}

impl<T> Exchange<T> {
    pub(crate) fn map<U>(self, f: impl FnOnce(T) -> U) -> Exchange<U> {
        Exchange {
            result: f(self.result),
            notifications: self.notifications,
            elapsed: self.elapsed,
        }
    }
}

/// What a session needs to send requests.
#[derive(Clone)]
pub(crate) struct Sender {
    pub name: String,
    pub peer: Peer<RoleClient>,
    pub notifications: Notifications,
    pub timeout: Duration,
    pub redactor: Redactor,
    /// Why the client closed the connection itself, once it did.
    pub ended: EndReason,
}

enum Step {
    Answer(Box<Result<Result<ServerResult, ServiceError>, ClientError>>),
    Note(Result<Notification, broadcast::error::RecvError>),
    TimedOut,
    Cancelled,
}

impl Sender {
    /// Send `request` and wait for its answer.
    pub async fn send(
        &self,
        request: ClientRequest,
        options: &RequestOptions,
    ) -> Result<Exchange<ServerResult>, ClientError> {
        let timeout = options.timeout.unwrap_or(self.timeout);
        let mut notes = self.notifications.subscribe();
        let started = Instant::now();
        let mut handle = self
            .peer
            .send_request_with_option(request, PeerRequestOptions::no_options())
            .await
            .map_err(|e| self.service_error(e))?;
        let token = token_text(&handle.progress_token);
        let cancel = options.cancel.clone().unwrap_or_default();
        let deadline = tokio::time::sleep(timeout);
        tokio::pin!(deadline);
        let mut collected = Vec::new();
        loop {
            let step = tokio::select! {
                answer = &mut handle.rx => Step::Answer(Box::new(answer.map_err(|_| self.disconnected()))),
                note = notes.recv() => Step::Note(note),
                () = &mut deadline => Step::TimedOut,
                () = cancel.cancelled() => Step::Cancelled,
            };
            match step {
                Step::Answer(answer) => {
                    drain(&mut notes, &mut collected);
                    let result = (*answer)?.map_err(|e| self.service_error(e))?;
                    return Ok(Exchange {
                        result,
                        notifications: collected,
                        elapsed: started.elapsed(),
                    });
                }
                Step::Note(Ok(note)) => record(note, &token, options, &mut collected),
                // Lagging loses notes, not the answer. The channel cannot
                // close: `self.notifications` holds its sender.
                Step::Note(Err(_)) => notes = notes.resubscribe(),
                Step::TimedOut => {
                    abandon(handle, "request timed out").await;
                    return Err(ClientError::Timeout(format_timeout(timeout)));
                }
                Step::Cancelled => {
                    abandon(handle, "cancelled by user").await;
                    return Err(ClientError::Cancelled);
                }
            }
        }
    }

    /// Follow `nextCursor` until exhausted, at most [`MAX_PAGES`] pages.
    pub async fn paginate<T>(
        &self,
        method: &str,
        options: &RequestOptions,
        request: impl Fn(PaginatedRequestParams) -> ClientRequest,
        page: impl Fn(ServerResult) -> Option<(Vec<T>, Option<String>)>,
    ) -> Result<Exchange<Vec<T>>, ClientError> {
        let started = Instant::now();
        let mut items = Vec::new();
        let mut notifications = Vec::new();
        let mut cursor = None;
        for _ in 0..MAX_PAGES {
            let params = PaginatedRequestParams::default().with_cursor(cursor);
            let exchange = self.send(request(params), options).await?;
            notifications.extend(exchange.notifications);
            let (found, next) = page(exchange.result).ok_or_else(|| self.unexpected(method))?;
            items.extend(found);
            cursor = next;
            if cursor.is_none() {
                notifications.truncate(MAX_CALL_NOTIFICATIONS);
                return Ok(Exchange {
                    result: items,
                    notifications,
                    elapsed: started.elapsed(),
                });
            }
        }
        Err(ClientError::TooManyPages(method.to_owned()))
    }

    pub fn unexpected(&self, method: &str) -> ClientError {
        ClientError::Transport {
            name: self.name.clone(),
            message: format!("unexpected answer to {method}"),
        }
    }

    fn disconnected(&self) -> ClientError {
        match self.ended.get() {
            Some(reason) => ClientError::Transport {
                name: self.name.clone(),
                message: reason.clone(),
            },
            None => ClientError::Disconnected(self.name.clone()),
        }
    }

    pub fn service_error(&self, error: ServiceError) -> ClientError {
        match error {
            ServiceError::McpError(data) => ClientError::Protocol {
                code: data.code.0,
                message: self.redactor.text(&data.message).into_owned(),
            },
            ServiceError::TransportClosed => self.disconnected(),
            ServiceError::Timeout { timeout } => ClientError::Timeout(format_timeout(timeout)),
            other => ClientError::Transport {
                name: self.name.clone(),
                message: self.redactor.text(&other.to_string()).into_owned(),
            },
        }
    }
}

/// Keep `note` for the call and report it when it is the call's progress.
fn record(
    note: Notification,
    token: &str,
    options: &RequestOptions,
    collected: &mut Vec<Notification>,
) {
    if let NotificationKind::Progress {
        token: of,
        progress,
        total,
        message,
    } = &note.kind
        && of == token
        && let Some(sender) = &options.progress
    {
        sender.send_replace(Some(Progress {
            progress: *progress,
            total: *total,
            message: message.clone(),
        }));
    }
    if collected.len() < MAX_CALL_NOTIFICATIONS {
        collected.push(note);
    }
}

/// The notifications already queued when the answer arrived.
fn drain(notes: &mut broadcast::Receiver<Notification>, collected: &mut Vec<Notification>) {
    while let Ok(note) = notes.try_recv() {
        if collected.len() < MAX_CALL_NOTIFICATIONS {
            collected.push(note);
        }
    }
}

/// Stop waiting for `handle` and tell the server with
/// `notifications/cancelled`.
async fn abandon(handle: RequestHandle<RoleClient>, reason: &str) {
    let _ = handle.cancel(Some(reason.to_owned())).await;
}

/// `60s`, or `1500ms` when not a whole number of seconds.
pub fn format_timeout(timeout: Duration) -> String {
    if timeout.subsec_millis() == 0 {
        format!("{}s", timeout.as_secs())
    } else {
        format!("{}ms", timeout.as_millis())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn timeouts_are_shown_in_seconds_when_whole() {
        assert_eq!(format_timeout(DEFAULT_TIMEOUT), "60s");
        assert_eq!(format_timeout(Duration::from_millis(1500)), "1500ms");
        assert_eq!(
            ClientError::Timeout(format_timeout(DEFAULT_TIMEOUT)).to_string(),
            "mcp: request timed out after 60s"
        );
        assert_eq!(ClientError::Cancelled.to_string(), "cancelled by user");
    }

    #[test]
    fn progress_percent_needs_a_total() {
        let progress = Progress {
            progress: 2.0,
            total: Some(5.0),
            message: None,
        };
        assert_eq!(progress.percent(), Some(40.0));
        assert_eq!(
            Progress {
                total: None,
                ..progress
            }
            .percent(),
            None
        );
    }
}
