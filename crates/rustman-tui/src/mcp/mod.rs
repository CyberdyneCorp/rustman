//! The MCP inspector pane (openspec change `add-mcp-client`, spec: TUI MCP
//! inspector pane): the workspace's MCP servers and their connection
//! status, the session's server info and capabilities, the `Tools`,
//! `Resources`, `Prompts`, `Notifications` and `Stderr` tabs, argument
//! forms generated from schemas, the result view, and the dialogs for
//! stdio trust, sampling and elicitation. A persistent status-bar badge
//! counts the approvals waiting for the user, whatever pane is shown.
//!
//! A session runs on its own thread (see [`worker`]); its events come back
//! as [`Msg`] actions. Sampling and elicitation requests wait in
//! [`McpPane::approvals`] until the user answers: nothing is answered
//! automatically.

// rmcp marks sampling and logging deprecated (SEP-2577), but they are part
// of every protocol revision the client negotiates.
#![allow(deprecated)]

mod dialog;
pub mod form;
mod keys;
mod view;
pub mod worker;

#[cfg(test)]
mod tests;

use std::collections::VecDeque;
use std::path::PathBuf;

use rustman_core::trust::TrustKey;

use rustman_mcp::client::protocol::{Prompt as ServerPrompt, Resource, ResourceTemplate, Tool};
use rustman_mcp::client::{
    Approval, McpOp, Notification, OpResult, PendingApproval, Progress, ResolvedServer,
    SessionInfo, TrustCheck,
};
use tokio::sync::mpsc::UnboundedSender;
use tokio_util::sync::CancellationToken;

pub use keys::{key, submit};
pub use view::render;
pub use worker::{Command, Job, execute, intercept};

use crate::app::{Effect, State};
use crate::status::McpBadge;
use crate::toast::Severity;
use crate::workbench;

/// Notification and stderr lines kept per session.
pub const LOG_LINES: usize = 500;

/// The inspector's state.
#[derive(Debug, Default)]
pub struct McpPane {
    /// The pane fills the shell (sessions keep running when it is closed).
    pub open: bool,
    pub servers: Vec<ServerRow>,
    /// The selected row of `servers`.
    pub selected: usize,
    pub focus: Focus,
    pub tab: Tab,
    /// The selected row of the tab's list.
    pub item: usize,
    pub session: Option<Live>,
    pub form: Option<form::Form>,
    pub result: Option<ResultView>,
    /// Sampling and elicitation requests waiting for the user, oldest first.
    pub approvals: VecDeque<dialog::Waiting>,
    /// A stdio command waiting for the user's approval.
    pub trust: Option<TrustPrompt>,
    /// The last call made, for saving as a request file.
    pub last_call: Option<(String, McpOp)>,
    /// The last operation sent, for `Ctrl+O` after a validation error.
    pub attempted: Option<McpOp>,
    /// Seals this machine's stdio approvals (the user's data directory).
    pub trust_key: TrustKey,
}

/// One server of the workspace.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServerRow {
    pub name: String,
    /// `stdio` or `http`.
    pub transport: &'static str,
    pub conn: Conn,
}

/// A server's connection status.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum Conn {
    #[default]
    Idle,
    Connecting,
    Connected,
    Failed(String),
}

/// Where keys go.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Focus {
    #[default]
    Servers,
    List,
    Form,
}

/// The tabs of a session.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Tab {
    #[default]
    Tools,
    Resources,
    Prompts,
    Notifications,
    Stderr,
}

impl Tab {
    pub const ALL: [Self; 5] = [
        Self::Tools,
        Self::Resources,
        Self::Prompts,
        Self::Notifications,
        Self::Stderr,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::Tools => "Tools",
            Self::Resources => "Resources",
            Self::Prompts => "Prompts",
            Self::Notifications => "Notifications",
            Self::Stderr => "Stderr",
        }
    }
}

/// The lists a connected session answered.
#[derive(Debug, Clone, Default)]
pub struct Lists {
    pub tools: Vec<Tool>,
    pub resources: Vec<Resource>,
    pub templates: Vec<ResourceTemplate>,
    pub prompts: Vec<ServerPrompt>,
}

/// A connected session as the pane sees it.
#[derive(Debug)]
pub struct Live {
    pub server: String,
    pub info: SessionInfo,
    pub lists: Lists,
    /// Timestamped notification lines, oldest first.
    pub notifications: VecDeque<String>,
    pub stderr: VecDeque<String>,
    /// The last resource update offered for a re-read.
    pub reread: Option<String>,
    /// The log level last set (index into [`LEVELS`]).
    pub level: Option<usize>,
    pub busy: Option<Busy>,
    pub commands: UnboundedSender<Command>,
}

/// The log levels `l` cycles through.
pub const LEVELS: [&str; 4] = ["debug", "info", "warning", "error"];

/// A request in flight.
#[derive(Debug)]
pub struct Busy {
    pub title: String,
    pub progress: Option<Progress>,
    pub cancel: CancellationToken,
}

/// What a call returned, as shown.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResultView {
    pub title: String,
    /// `ok`, `tool error`, `protocol error <code>`, `cancelled` or `error`.
    pub label: String,
    pub lines: Vec<String>,
    /// Input violations that the user may send anyway (`Ctrl+O`).
    pub overridable: bool,
}

/// The trust dialog of a stdio server.
#[derive(Debug)]
pub struct TrustPrompt {
    pub pending: PendingApproval,
    pub server: Box<ResolvedServer>,
}

/// What a connected session sends first.
#[derive(Debug)]
pub struct Connected {
    pub info: SessionInfo,
    pub lists: Lists,
    pub commands: UnboundedSender<Command>,
}

/// A finished call.
#[derive(Debug)]
pub struct Performed {
    pub title: String,
    pub op: McpOp,
    pub result: Result<Box<OpResult>, String>,
}

/// Completions and session events for the pane.
#[derive(Debug)]
pub enum Msg {
    Servers(Result<Vec<(String, &'static str)>, String>),
    Prepared {
        name: String,
        result: Result<Box<worker::Prepared>, String>,
    },
    Trusted {
        result: Result<(Box<ResolvedServer>, rustman_mcp::client::Approved), String>,
    },
    Connected {
        name: String,
        result: Result<Box<Connected>, String>,
    },
    Performed(Box<Performed>),
    Progress(Progress),
    Notification(Box<Notification>),
    Stderr(Vec<String>),
    Tools(Vec<Tool>),
    Approval(Approval),
    Disconnected(String),
    /// An OAuth login waits for the user: the authorization URL.
    Login(String),
    /// A save, log level or subscription finished: the toast text.
    Done(Result<String, String>),
    Saved(Result<String, String>),
}

/// The text prompts of the pane.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Prompt {
    /// The file name to save the last call as.
    SaveCall,
}

/// `mcp.open`: show or hide the pane; the first time, read the servers.
pub fn toggle_pane(state: &mut State) -> Vec<Effect> {
    state.mcp.open = !state.mcp.open;
    if !state.mcp.open || !state.mcp.servers.is_empty() {
        return Vec::new();
    }
    match workbench::workspace(state) {
        Some(ws) => vec![Effect::Mcp(Box::new(Job::LoadServers {
            root: ws.root.clone(),
        }))],
        None => Vec::new(),
    }
}

/// Apply a completion or session event.
pub fn on_msg(state: &mut State, msg: Msg) -> Vec<Effect> {
    let effects = match msg {
        Msg::Servers(result) => {
            on_servers(state, result);
            Vec::new()
        }
        Msg::Prepared { name, result } => on_prepared(state, &name, result),
        Msg::Trusted { result } => on_trusted(state, result),
        Msg::Connected { name, result } => {
            on_connected(state, &name, result);
            Vec::new()
        }
        Msg::Approval(approval) => {
            state
                .mcp
                .approvals
                .push_back(dialog::Waiting::new(approval));
            Vec::new()
        }
        other => {
            on_event(state, other);
            Vec::new()
        }
    };
    sync_status(state);
    effects
}

fn on_servers(state: &mut State, result: Result<Vec<(String, &'static str)>, String>) {
    match result {
        Ok(servers) => {
            state.mcp.servers = servers
                .into_iter()
                .map(|(name, transport)| ServerRow {
                    name,
                    transport,
                    conn: Conn::Idle,
                })
                .collect();
        }
        Err(message) => state.notify(Severity::Error, message),
    }
}

fn set_conn(state: &mut State, name: &str, conn: Conn) {
    if let Some(row) = state.mcp.servers.iter_mut().find(|r| r.name == name) {
        row.conn = conn;
    }
}

fn on_prepared(
    state: &mut State,
    name: &str,
    result: Result<Box<worker::Prepared>, String>,
) -> Vec<Effect> {
    let prepared = match result {
        Ok(prepared) => prepared,
        Err(message) => {
            set_conn(state, name, Conn::Failed(message.clone()));
            state.notify(Severity::Error, message);
            return Vec::new();
        }
    };
    let worker::Prepared { server, check } = *prepared;
    match check {
        TrustCheck::Approved(approved) => connect(state, server, approved),
        TrustCheck::NeedsApproval(pending) => {
            // Nothing is spawned until the user confirms the command line.
            state.mcp.trust = Some(TrustPrompt { pending, server });
            Vec::new()
        }
    }
}

fn on_trusted(
    state: &mut State,
    result: Result<(Box<ResolvedServer>, rustman_mcp::client::Approved), String>,
) -> Vec<Effect> {
    match result {
        Ok((server, approved)) => connect(state, server, approved),
        Err(message) => {
            state.notify(Severity::Error, message);
            Vec::new()
        }
    }
}

fn connect(
    state: &mut State,
    server: Box<ResolvedServer>,
    approved: rustman_mcp::client::Approved,
) -> Vec<Effect> {
    set_conn(state, &server.name.clone(), Conn::Connecting);
    let root = crate::workbench::workspace(state).map(|ws| ws.root.clone());
    vec![Effect::Mcp(Box::new(Job::Connect {
        server,
        approved,
        root,
        backend: state.secret_backend,
    }))]
}

fn on_connected(state: &mut State, name: &str, result: Result<Box<Connected>, String>) {
    match result {
        Ok(connected) => {
            set_conn(state, name, Conn::Connected);
            if let Some(old) = state.mcp.session.take() {
                let _ = old.commands.send(Command::Disconnect);
            }
            let Connected {
                info,
                lists,
                commands,
            } = *connected;
            state.mcp.session = Some(Live {
                server: name.to_owned(),
                info,
                lists,
                notifications: VecDeque::new(),
                stderr: VecDeque::new(),
                reread: None,
                level: None,
                busy: None,
                commands,
            });
            state.mcp.item = 0;
            state.mcp.focus = Focus::List;
        }
        Err(message) => {
            set_conn(state, name, Conn::Failed(message.clone()));
            state.notify(Severity::Error, message);
        }
    }
}

/// Events of the live session.
fn on_event(state: &mut State, msg: Msg) {
    match msg {
        Msg::Performed(performed) => on_performed(state, *performed),
        Msg::Progress(progress) => {
            if let Some(busy) = state.mcp.session.as_mut().and_then(|s| s.busy.as_mut()) {
                busy.progress = Some(progress);
            }
        }
        Msg::Disconnected(message) => on_disconnected(state, message),
        Msg::Login(url) => state.notify(
            Severity::Info,
            format!("Log in to the server in your browser: {url}"),
        ),
        Msg::Done(Ok(text)) | Msg::Saved(Ok(text)) => state.notify(Severity::Success, text),
        Msg::Done(Err(text)) | Msg::Saved(Err(text)) => state.notify(Severity::Error, text),
        other => on_log(state, other),
    }
}

fn on_log(state: &mut State, msg: Msg) {
    let Some(live) = state.mcp.session.as_mut() else {
        return;
    };
    match msg {
        Msg::Notification(note) => {
            if let Some(uri) = note.reread_uri() {
                live.reread = Some(uri.to_owned());
            }
            push(&mut live.notifications, dialog::note_line(&note));
        }
        Msg::Stderr(lines) => lines.into_iter().for_each(|l| push(&mut live.stderr, l)),
        Msg::Tools(tools) => live.lists.tools = tools,
        _ => {}
    }
}

fn push(log: &mut VecDeque<String>, line: String) {
    if log.len() == LOG_LINES {
        log.pop_front();
    }
    log.push_back(line);
}

fn on_disconnected(state: &mut State, message: String) {
    if let Some(live) = state.mcp.session.take() {
        set_conn(state, &live.server, Conn::Failed(message.clone()));
        state.notify(Severity::Warn, format!("{}: {message}", live.server));
    }
}

fn on_performed(state: &mut State, performed: Performed) {
    if let Some(live) = state.mcp.session.as_mut() {
        live.busy = None;
    }
    let Performed { title, op, result } = performed;
    let view = match result {
        Ok(done) => ResultView {
            title,
            label: done.outcome.to_owned(),
            lines: done.text.lines().map(str::to_owned).collect(),
            overridable: false,
        },
        Err(message) => ResultView {
            title,
            label: error_label(&message),
            overridable: message.starts_with("mcp: invalid arguments:"),
            lines: vec![message],
        },
    };
    if view.label == "ok" || view.label == "tool error" {
        let server = state.mcp.session.as_ref().map(|s| s.server.clone());
        state.mcp.last_call = server.map(|s| (s, op));
    }
    state.mcp.result = Some(view);
}

/// `protocol error -32602` for JSON-RPC errors, else `cancelled` or `error`.
fn error_label(message: &str) -> String {
    if let Some(rest) = message.strip_prefix("mcp: protocol error ") {
        let code = rest.split(':').next().unwrap_or_default();
        return format!("protocol error {code}");
    }
    if message == "cancelled by user" {
        return "cancelled".to_owned();
    }
    "error".to_owned()
}

/// The status-bar badge follows the approvals waiting.
fn sync_status(state: &mut State) {
    let pending = state.mcp.approvals.len();
    state.status.mcp = (pending > 0).then_some(McpBadge { pending });
}

/// Forget answered approvals and update the badge.
pub(crate) fn answered(state: &mut State) {
    state.mcp.approvals.pop_front();
    sync_status(state);
}

/// Disconnect every session when the UI quits.
pub fn shut_down(state: &mut State) {
    if let Some(live) = &state.mcp.session {
        let _ = live.commands.send(Command::Disconnect);
    }
}

/// Where a saved call goes: the workspace root.
fn save_path(state: &State, typed: &str) -> Option<PathBuf> {
    let root = workbench::workspace(state)?.root.clone();
    crate::files::request_file_name(typed).map(|rel| root.join(rel))
}

#[cfg(test)]
mod label_tests {
    use super::*;

    #[test]
    fn error_labels_name_protocol_errors() {
        assert_eq!(
            error_label("mcp: protocol error -32602: bad"),
            "protocol error -32602"
        );
        assert_eq!(error_label("cancelled by user"), "cancelled");
        assert_eq!(error_label("mcp: request timed out after 60s"), "error");
    }
}
