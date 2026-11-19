//! The mock server pane (openspec change `add-openapi-mock-server`, task
//! 6.5): start and stop a mock of an OpenAPI document, and follow its
//! request log live with the source of every response.
//!
//! The server runs on its own thread with its own runtime (see
//! [`jobs::spawn_server`]); each request log entry comes back as a
//! [`Msg::Entry`] action, already redacted by the server. A
//! network-reachable `[mock] host` is started only after confirmation.

mod jobs;
mod view;

#[cfg(test)]
mod tests;

use std::collections::VecDeque;
use std::net::SocketAddr;
use std::sync::Arc;

use rustman_mock::LogEntry;
use rustman_mock::server::{bind_addr, remote_warning};
use tokio_util::sync::CancellationToken;

pub use jobs::{Job, Prepared, StartJob, execute, intercept, spawn_server};
pub use view::render;

use crate::app::{Effect, Overlay, State};
use crate::dialogs::{Confirm, Input, Purpose};
use crate::keymap::chord::{Chord, Code};
use crate::toast::Severity;
use crate::workbench;

/// Request log rows kept in the pane.
pub const LOG_LINES: usize = 500;

/// The pane's state.
#[derive(Debug, Default)]
pub struct MockPane {
    /// The pane fills the shell (the server keeps running when closed).
    pub open: bool,
    pub server: Server,
    /// The document last served, offered again.
    pub document: String,
    /// The request log and notices, oldest first.
    pub log: VecDeque<LogRow>,
}

/// Where the server is in its lifecycle.
#[derive(Debug, Default)]
pub enum Server {
    #[default]
    Stopped,
    /// Reading `[mock]`.
    Preparing,
    /// Waiting for the user to accept a network-reachable address.
    Confirming(Arc<Prepared>),
    Starting,
    Running(Box<Running>),
    Stopping,
    Failed(String),
}

/// A running server as the pane sees it.
#[derive(Debug)]
pub struct Running {
    pub addr: SocketAddr,
    pub operations: usize,
    /// Cancelling it stops the server thread.
    pub stop: CancellationToken,
}

/// One row of the log: a served request, or a notice.
#[derive(Debug, Clone, PartialEq)]
pub enum LogRow {
    Request(Box<LogEntry>),
    Notice(String),
}

/// Completions and server events for the pane.
#[derive(Debug)]
pub enum Msg {
    Prepared(Result<Arc<Prepared>, String>),
    Started(Result<Box<Running>, String>),
    Entry(Box<LogEntry>),
    /// A reload, a failed reload or another server notice.
    Notice(String),
    /// The server thread ended and released the port.
    Stopped,
}

/// The text prompts of the pane.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Prompt {
    /// The OpenAPI document to serve.
    Document,
}

/// `mock.open`: show or hide the pane.
pub fn toggle_pane(state: &mut State) {
    state.mock.open = !state.mock.open;
}

/// Keys while the pane is open; `None` leaves the key to the keymap.
pub fn key(state: &mut State, chord: Chord) -> Option<Vec<Effect>> {
    if !state.mock.open || chord.ctrl || chord.alt {
        return None;
    }
    let effects = match chord.code {
        Code::Esc => {
            state.mock.open = false;
            Vec::new()
        }
        Code::Char('s') => start_or_stop(state),
        Code::Char('c') => {
            state.mock.log.clear();
            Vec::new()
        }
        Code::Char('?') => return None,
        Code::Char(_) => Vec::new(),
        _ => return None,
    };
    Some(effects)
}

fn start_or_stop(state: &mut State) -> Vec<Effect> {
    match &state.mock.server {
        Server::Running(running) => {
            running.stop.cancel();
            state.mock.server = Server::Stopping;
        }
        Server::Stopped | Server::Failed(_) => {
            let input = Input::new(
                " Mock server ",
                "OpenAPI document (file or URL)",
                Purpose::Mock(Prompt::Document),
                &state.mock.document,
            );
            state.overlay = Some(Overlay::Input(input));
        }
        _ => {}
    }
    Vec::new()
}

/// A prompt was submitted.
pub fn submit(state: &mut State, prompt: Prompt, text: &str) -> Vec<Effect> {
    state.overlay = None;
    match prompt {
        Prompt::Document => {
            let document = text.trim().to_owned();
            if document.is_empty() {
                return Vec::new();
            }
            state.mock.document.clone_from(&document);
            state.mock.server = Server::Preparing;
            let job = Job::Prepare {
                root: workbench::workspace(state).map(|ws| ws.root.clone()),
                document,
            };
            vec![Effect::Mock(Box::new(job))]
        }
    }
}

/// Apply a completion or server event.
pub fn on_msg(state: &mut State, msg: Msg) -> Vec<Effect> {
    match msg {
        Msg::Prepared(result) => return on_prepared(state, result),
        Msg::Started(Ok(running)) => {
            let banner = rustman_mock::server::banner(running.operations, running.addr);
            push(&mut state.mock, LogRow::Notice(banner));
            if !running.addr.ip().is_loopback() {
                state.notify(Severity::Warn, remote_warning(running.addr));
            }
            state.mock.server = Server::Running(running);
        }
        Msg::Started(Err(message)) => fail(state, message),
        Msg::Entry(entry) => push(&mut state.mock, LogRow::Request(entry)),
        Msg::Notice(line) => push(&mut state.mock, LogRow::Notice(line)),
        Msg::Stopped => {
            if !matches!(state.mock.server, Server::Failed(_)) {
                state.mock.server = Server::Stopped;
            }
            push(&mut state.mock, LogRow::Notice("mock: stopped".into()));
        }
    }
    Vec::new()
}

fn on_prepared(state: &mut State, result: Result<Arc<Prepared>, String>) -> Vec<Effect> {
    let prepared = match result {
        Ok(prepared) => prepared,
        Err(message) => {
            fail(state, message);
            return Vec::new();
        }
    };
    let mut probe = prepared.config.clone();
    probe.allow_remote = true;
    match bind_addr(&probe) {
        Err(err) => {
            fail(state, err.to_string());
            Vec::new()
        }
        Ok(addr) if !addr.ip().is_loopback() => {
            state.mock.server = Server::Confirming(prepared);
            let confirm = Confirm::mock_remote(&remote_warning(addr));
            state.overlay = Some(Overlay::Confirm(confirm));
            Vec::new()
        }
        Ok(_) => start(state, prepared),
    }
}

fn start(state: &mut State, prepared: Arc<Prepared>) -> Vec<Effect> {
    state.mock.server = Server::Starting;
    let job = StartJob {
        prepared,
        stop: CancellationToken::new(),
    };
    vec![Effect::Mock(Box::new(Job::Start(job)))]
}

/// The user accepted a network-reachable address.
pub fn confirm_remote(state: &mut State) -> Vec<Effect> {
    let Server::Confirming(prepared) = std::mem::take(&mut state.mock.server) else {
        return Vec::new();
    };
    let mut allowed = (*prepared).clone();
    allowed.config.allow_remote = true;
    start(state, Arc::new(allowed))
}

/// A dialog was cancelled: a pending remote start does not happen.
pub fn cancel_confirm(state: &mut State) {
    if matches!(state.mock.server, Server::Confirming(_)) {
        state.mock.server = Server::Stopped;
    }
}

fn fail(state: &mut State, message: String) {
    state.notify(Severity::Error, message.clone());
    push(&mut state.mock, LogRow::Notice(message.clone()));
    state.mock.server = Server::Failed(message);
}

fn push(pane: &mut MockPane, row: LogRow) {
    if pane.log.len() == LOG_LINES {
        pane.log.pop_front();
    }
    pane.log.push_back(row);
}

/// Stop a running server when the UI exits.
pub fn shut_down(state: &mut State) {
    if let Server::Running(running) = &state.mock.server {
        running.stop.cancel();
    }
}
