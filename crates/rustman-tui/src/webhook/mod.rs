//! The webhook pane (openspec change `add-webhook-inspector`, spec: TUI
//! webhook pane): start and stop the listener, follow its captures and
//! event log live, inspect a capture's headers and body (text, JSON or
//! hex), edit `webhook/mocks.yaml`, and save, replay, clear or copy
//! captures as curl.
//!
//! The listener runs on its own thread with its own runtime (see
//! [`jobs::spawn_listener`]); its events come back as
//! [`Msg::Event`] actions, so a capture is drawn on the next frame. An
//! action carries only the event's log line, never the capture itself, so
//! a stalled UI cannot keep evicted captures alive.

mod detail;
mod jobs;
mod view;

#[cfg(test)]
mod tests;

use std::collections::VecDeque;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;

use rustman_core::Redactor;
use rustman_webhook::listener::{bind_addr, is_remote, remote_warning};
use rustman_webhook::mocks::MOCKS_FILE;
use rustman_webhook::{Capture, CaptureStore, Event};
use tokio_util::sync::CancellationToken;

pub use jobs::{Job, Prepared, StartJob, execute, intercept, spawn_listener};
pub use view::render;

use crate::app::{Effect, Overlay, State};
use crate::dialogs::{Confirm, Input, Purpose};
use crate::keymap::chord::{Chord, Code};
use crate::status::WebhookBadge;
use crate::textarea::TextArea;
use crate::toast::Severity;
use crate::workbench;

/// Event log lines kept in the pane.
pub const LOG_LINES: usize = 200;

/// The pane's state.
#[derive(Debug, Default)]
pub struct WebhookPane {
    /// The pane fills the shell (the listener keeps running when closed).
    pub open: bool,
    pub listener: Listener,
    /// The buffered captures, newest first.
    pub captures: Vec<Arc<Capture>>,
    /// Body bytes held by the buffer.
    pub buffer_bytes: usize,
    /// The selected row of `captures`.
    pub selected: usize,
    pub body: BodyMode,
    /// The selected body's display lines, built once per capture and mode.
    pub body_cache: detail::BodyCache,
    /// How far the selected capture is scrolled.
    pub scroll: detail::Scroll,
    /// The redactor of the last listener that ran: its captures stay shown
    /// after it stopped.
    pub last_redactor: Redactor,
    /// The event log, oldest first, redacted.
    pub log: VecDeque<String>,
    /// The mock editor, while open.
    pub mocks: Option<MockEditor>,
    /// The last replay target, offered again.
    pub last_url: Option<String>,
}

/// Where the listener is in its lifecycle.
#[derive(Debug, Default)]
pub enum Listener {
    #[default]
    Stopped,
    /// Reading `[webhook]`, the mocks and the verifier secrets.
    Preparing,
    /// Waiting for the user to accept a network-reachable address.
    Confirming(Arc<Prepared>),
    Starting,
    Running(Box<Running>),
    Stopping,
    Failed(String),
}

/// A running listener as the pane sees it.
#[derive(Debug)]
pub struct Running {
    pub addr: SocketAddr,
    pub store: CaptureStore,
    /// Cancelling it stops the listener thread.
    pub stop: CancellationToken,
    /// Knows every verifier secret; applied to everything shown.
    pub redactor: Redactor,
}

/// How the selected capture's body is shown.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum BodyMode {
    /// JSON, form fields or text when the body decodes, else hex.
    #[default]
    Auto,
    Text,
    Json,
    Hex,
}

impl BodyMode {
    pub fn label(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Text => "text",
            Self::Json => "JSON",
            Self::Hex => "hex",
        }
    }

    fn next(self) -> Self {
        match self {
            Self::Auto => Self::Text,
            Self::Text => Self::Json,
            Self::Json => Self::Hex,
            Self::Hex => Self::Auto,
        }
    }
}

/// `webhook/mocks.yaml` being edited.
#[derive(Debug)]
pub struct MockEditor {
    pub path: PathBuf,
    pub text: TextArea,
    pub error: Option<String>,
}

/// What the pane needs of a listener event: its (unredacted) log line and
/// whether the capture list changed. The pane reads captures from the
/// store, so the event's capture is not kept.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EventNote {
    pub line: String,
    pub captured: bool,
    pub cleared: bool,
}

impl From<&Event> for EventNote {
    fn from(event: &Event) -> Self {
        Self {
            line: event.to_string(),
            captured: matches!(event, Event::Captured(_)),
            cleared: matches!(event, Event::Cleared),
        }
    }
}

/// Completions and listener events for the pane.
#[derive(Debug)]
pub enum Msg {
    Prepared(Result<Arc<Prepared>, String>),
    Started(Result<Box<Running>, String>),
    Event(EventNote),
    /// The listener thread ended and released the port.
    Stopped,
    MocksRead {
        path: PathBuf,
        result: Result<String, String>,
    },
    /// The mocks file was written with this many mocks.
    MocksWritten(Result<usize, String>),
    /// A save, replay or history entry finished: the toast text.
    Done(Result<String, String>),
}

/// The text prompts of the pane.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Prompt {
    Replay { id: u64 },
    SaveUrl { id: u64 },
    SaveFile { id: u64, url: String },
}

/// `webhook.open`: show or hide the pane.
pub fn toggle_pane(state: &mut State) {
    state.webhook.open = !state.webhook.open;
}

/// Keys while the pane is open; `None` leaves the key to the keymap.
pub fn key(state: &mut State, chord: Chord) -> Option<Vec<Effect>> {
    if !state.webhook.open {
        return None;
    }
    if state.webhook.mocks.is_some() {
        return Some(mock_key(state, chord));
    }
    if chord.ctrl || chord.alt {
        return None;
    }
    let effects = match chord.code {
        Code::Esc => {
            state.webhook.open = false;
            Vec::new()
        }
        Code::Up | Code::Char('k') => move_selection(state, -1),
        Code::Down | Code::Char('j') => move_selection(state, 1),
        Code::Home | Code::Char('g') => move_selection(state, isize::MIN),
        Code::End | Code::Char('G') => move_selection(state, isize::MAX),
        Code::PageDown => {
            state.webhook.scroll.page_down();
            Vec::new()
        }
        Code::PageUp => {
            state.webhook.scroll.page_up();
            Vec::new()
        }
        Code::Char(c) => action_key(state, c)?,
        _ => return None,
    };
    Some(effects)
}

/// Letter keys; `?` stays the help key, other letters do nothing here.
fn action_key(state: &mut State, c: char) -> Option<Vec<Effect>> {
    let effects = match c {
        's' => start_or_stop(state),
        'v' => {
            state.webhook.body = state.webhook.body.next();
            state.webhook.scroll.top = 0;
            Vec::new()
        }
        'c' => clear(state),
        'y' => copy_as_curl(state),
        'r' => ask(state, |id| Prompt::Replay { id }),
        'w' => ask(state, |id| Prompt::SaveUrl { id }),
        'h' => save_to_history(state),
        'm' => open_mocks(state),
        '?' => return None,
        _ => Vec::new(),
    };
    Some(effects)
}

fn move_selection(state: &mut State, by: isize) -> Vec<Effect> {
    let pane = &mut state.webhook;
    let last = pane.captures.len().saturating_sub(1);
    let before = pane.selected;
    pane.selected = pane.selected.saturating_add_signed(by).min(last);
    if pane.selected != before {
        pane.scroll.top = 0;
    }
    Vec::new()
}

/// The selected capture.
pub fn selected(pane: &WebhookPane) -> Option<&Arc<Capture>> {
    pane.captures.get(pane.selected)
}

fn running(pane: &WebhookPane) -> Option<&Running> {
    match &pane.listener {
        Listener::Running(running) => Some(running),
        _ => None,
    }
}

fn start_or_stop(state: &mut State) -> Vec<Effect> {
    match &state.webhook.listener {
        Listener::Running(running) => {
            running.stop.cancel();
            state.webhook.listener = Listener::Stopping;
            Vec::new()
        }
        Listener::Stopped | Listener::Failed(_) => {
            state.webhook.listener = Listener::Preparing;
            let job = Job::Prepare {
                root: workbench::workspace(state).map(|ws| ws.root.clone()),
                environment: state.status.environment.clone(),
                backend: state.secret_backend,
            };
            vec![Effect::Webhook(Box::new(job))]
        }
        _ => Vec::new(),
    }
}

/// Apply a completion or listener event.
pub fn on_msg(state: &mut State, msg: Msg) -> Vec<Effect> {
    let effects = match msg {
        Msg::Prepared(result) => on_prepared(state, result),
        Msg::Started(result) => {
            on_started(state, result);
            Vec::new()
        }
        Msg::Event(event) => {
            on_event(state, &event);
            Vec::new()
        }
        Msg::Stopped => {
            if !matches!(state.webhook.listener, Listener::Failed(_)) {
                state.webhook.listener = Listener::Stopped;
            }
            push_log(&mut state.webhook, "listener stopped".to_owned());
            Vec::new()
        }
        other => on_done(state, other),
    };
    sync_status(state);
    effects
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
        Ok(addr) if is_remote(addr) => {
            state.webhook.listener = Listener::Confirming(prepared);
            state.overlay = Some(Overlay::Confirm(Confirm::webhook_remote(&remote_warning(
                addr,
            ))));
            Vec::new()
        }
        Ok(_) => start(state, prepared),
    }
}

fn fail(state: &mut State, message: String) {
    state.notify(Severity::Error, message.clone());
    push_log(&mut state.webhook, message.clone());
    state.webhook.listener = Listener::Failed(message);
}

fn start(state: &mut State, prepared: Arc<Prepared>) -> Vec<Effect> {
    state.webhook.listener = Listener::Starting;
    let job = StartJob {
        prepared,
        stop: CancellationToken::new(),
    };
    vec![Effect::Webhook(Box::new(Job::Start(job)))]
}

/// The user accepted a network-reachable address.
pub fn confirm_remote(state: &mut State) -> Vec<Effect> {
    let Listener::Confirming(prepared) = std::mem::take(&mut state.webhook.listener) else {
        return Vec::new();
    };
    let mut allowed = (*prepared).clone();
    allowed.config.allow_remote = true;
    let effects = start(state, Arc::new(allowed));
    sync_status(state);
    effects
}

/// A dialog was cancelled: a pending remote start does not happen.
pub fn cancel_confirm(state: &mut State) {
    if matches!(state.webhook.listener, Listener::Confirming(_)) {
        state.webhook.listener = Listener::Stopped;
    }
}

fn on_started(state: &mut State, result: Result<Box<Running>, String>) {
    match result {
        Ok(running) => {
            let addr = running.addr;
            state.webhook.last_redactor = running.redactor.clone();
            state.webhook.body_cache.reset();
            state.webhook.listener = Listener::Running(running);
            push_log(&mut state.webhook, rustman_webhook::listener::banner(addr));
            if is_remote(addr) {
                state.notify(Severity::Warn, remote_warning(addr));
            }
            refresh(&mut state.webhook);
        }
        Err(message) => fail(state, message),
    }
}

fn on_event(state: &mut State, event: &EventNote) {
    let pane = &mut state.webhook;
    let Some(running) = running(pane) else { return };
    let line = running.redactor.text(&event.line).into_owned();
    let new_capture = event.captured;
    push_log(pane, line);
    if new_capture || event.cleared {
        // Keep the same capture selected when one arrives on top.
        if new_capture && pane.selected > 0 {
            pane.selected += 1;
        } else if new_capture {
            pane.scroll.top = 0;
        }
        refresh(pane);
    }
}

/// Mirror the store: newest first, evictions included.
fn refresh(pane: &mut WebhookPane) {
    let Some(running) = running(pane) else { return };
    let mut captures = running.store.snapshot();
    captures.reverse();
    pane.buffer_bytes = running.store.total_bytes();
    pane.captures = captures;
    pane.selected = pane.selected.min(pane.captures.len().saturating_sub(1));
}

fn push_log(pane: &mut WebhookPane, line: String) {
    if pane.log.len() == LOG_LINES {
        pane.log.pop_front();
    }
    pane.log.push_back(line);
}

/// The status bar badge follows the listener.
fn sync_status(state: &mut State) {
    state.status.webhook = running(&state.webhook).map(|r| WebhookBadge {
        addr: r.addr.to_string(),
        remote: is_remote(r.addr),
    });
}

fn clear(state: &mut State) -> Vec<Effect> {
    match running(&state.webhook) {
        Some(running) => running.store.clear(),
        None => state.webhook.captures.clear(),
    }
    state.webhook.selected = 0;
    state.webhook.scroll.top = 0;
    refresh(&mut state.webhook);
    Vec::new()
}

/// The redactor of the running listener, else of the last one that ran:
/// its captures are still shown (none before a listener ran: no secrets
/// were read).
fn redactor(pane: &WebhookPane) -> Redactor {
    running(pane).map_or_else(|| pane.last_redactor.clone(), |r| r.redactor.clone())
}

/// The URL a capture was sent to: its `Host` header, else the listener.
fn original_url(pane: &WebhookPane, capture: &Capture) -> String {
    let host = capture
        .header_values("host")
        .next()
        .map(|v| String::from_utf8_lossy(v).into_owned())
        .or_else(|| running(pane).map(|r| r.addr.to_string()))
        .unwrap_or_else(|| "localhost".to_owned());
    format!("http://{host}{}", capture.target())
}

fn copy_as_curl(state: &mut State) -> Vec<Effect> {
    let Some(capture) = selected(&state.webhook).cloned() else {
        return nothing_selected(state);
    };
    let url = original_url(&state.webhook, &capture);
    let redactor = redactor(&state.webhook);
    match rustman_webhook::curl::copy_as_curl(&capture, &url, &redactor) {
        Ok(exported) => {
            for warning in exported.warnings {
                state.notify(Severity::Warn, warning);
            }
            vec![Effect::Copy(exported.command)]
        }
        Err(err) => {
            state.notify(Severity::Error, err.to_string());
            Vec::new()
        }
    }
}

fn nothing_selected(state: &mut State) -> Vec<Effect> {
    state.notify(Severity::Info, "No capture selected");
    Vec::new()
}

/// Open the prompt for replay or save, about the selected capture.
fn ask(state: &mut State, prompt: impl Fn(u64) -> Prompt) -> Vec<Effect> {
    let Some(capture) = selected(&state.webhook).cloned() else {
        return nothing_selected(state);
    };
    let input = match prompt(capture.id) {
        Prompt::Replay { id } => {
            let url = state
                .webhook
                .last_url
                .clone()
                .unwrap_or_else(|| format!("http://localhost:3000{}", capture.target()));
            let title = format!("Replay capture #{id}");
            Input::new(
                &title,
                "URL:",
                Purpose::Webhook(Prompt::Replay { id }),
                &url,
            )
        }
        other => {
            let title = format!("Save capture #{} as a request", capture.id);
            let url = format!("{{{{base_url}}}}{}", capture.path);
            Input::new(&title, "Target URL:", Purpose::Webhook(other), &url)
        }
    };
    state.overlay = Some(Overlay::Input(input));
    Vec::new()
}

/// A pane prompt was submitted with `text`.
pub fn submit(state: &mut State, prompt: Prompt, text: &str) -> Vec<Effect> {
    state.overlay = None;
    let text = text.trim().to_owned();
    let id = match &prompt {
        Prompt::Replay { id } | Prompt::SaveUrl { id } | Prompt::SaveFile { id, .. } => *id,
    };
    let Some(capture) = state.webhook.captures.iter().find(|c| c.id == id).cloned() else {
        state.notify(
            Severity::Warn,
            format!("Capture #{id} is no longer buffered"),
        );
        return Vec::new();
    };
    match prompt {
        Prompt::Replay { .. } => {
            state.webhook.last_url = Some(text.clone());
            let redactor = redactor(&state.webhook);
            let job = Job::Replay {
                capture,
                url: text,
                redactor,
            };
            vec![Effect::Webhook(Box::new(job))]
        }
        Prompt::SaveUrl { .. } => {
            let name = format!("webhook-{id}{}", rustman_core::model::REQUEST_FILE_SUFFIX);
            let title = format!("Save capture #{id} as a request");
            let next = Prompt::SaveFile { id, url: text };
            let input = Input::new(&title, "File name:", Purpose::Webhook(next), &name);
            state.overlay = Some(Overlay::Input(input));
            Vec::new()
        }
        Prompt::SaveFile { url, .. } => save_file(state, capture, url, &text),
    }
}

fn save_file(state: &mut State, capture: Arc<Capture>, url: String, name: &str) -> Vec<Effect> {
    let Some(root) = workbench::workspace(state).map(|ws| ws.root.clone()) else {
        state.notify(Severity::Info, "Saving a request needs a workspace");
        return Vec::new();
    };
    let Some(rel) = crate::files::request_file_name(name) else {
        state.notify(Severity::Error, "Type a file name without `..` or a drive.");
        return Vec::new();
    };
    let job = Job::SaveFile {
        capture,
        url,
        path: root.join(rel),
        redactor: redactor(&state.webhook),
    };
    vec![Effect::Webhook(Box::new(job))]
}

fn save_to_history(state: &mut State) -> Vec<Effect> {
    let Some(capture) = selected(&state.webhook).cloned() else {
        return nothing_selected(state);
    };
    let Some(ws) = workbench::workspace(state) else {
        state.notify(Severity::Info, "History needs a workspace");
        return Vec::new();
    };
    if !ws.settings.history.enabled {
        state.notify(Severity::Info, "History is off for this workspace");
        return Vec::new();
    }
    let job = Job::SaveHistory {
        capture,
        root: ws.root.clone(),
        settings: ws.settings.history,
        redactor: redactor(&state.webhook),
    };
    vec![Effect::Webhook(Box::new(job))]
}

fn open_mocks(state: &mut State) -> Vec<Effect> {
    let Some(root) = workbench::workspace(state).map(|ws| ws.root.clone()) else {
        state.notify(
            Severity::Info,
            "Mocks live in a workspace's webhook/mocks.yaml",
        );
        return Vec::new();
    };
    vec![Effect::Webhook(Box::new(Job::ReadMocks(
        root.join(MOCKS_FILE),
    )))]
}

/// Keys of the mock editor: `Ctrl+S` saves, `Esc` closes without saving.
fn mock_key(state: &mut State, chord: Chord) -> Vec<Effect> {
    let Some(editor) = state.webhook.mocks.as_mut() else {
        return Vec::new();
    };
    match chord.code {
        Code::Esc => state.webhook.mocks = None,
        Code::Char('s' | 'S') if chord.ctrl => {
            let job = Job::WriteMocks {
                path: editor.path.clone(),
                text: editor.text.text(),
            };
            return vec![Effect::Webhook(Box::new(job))];
        }
        _ => {
            if crate::vim::type_key(&mut editor.text, chord) {
                editor.error = None;
            }
        }
    }
    Vec::new()
}

/// Completions of the mock editor and of save, replay and history jobs.
fn on_done(state: &mut State, msg: Msg) -> Vec<Effect> {
    match msg {
        Msg::MocksRead { path, result } => match result {
            Ok(text) => {
                state.webhook.mocks = Some(MockEditor {
                    path,
                    text: TextArea::from_text(&text),
                    error: None,
                });
            }
            Err(message) => state.notify(Severity::Error, message),
        },
        Msg::MocksWritten(Ok(count)) => {
            state.webhook.mocks = None;
            state.notify(Severity::Success, format!("Mocks saved: {count} defined"));
        }
        Msg::MocksWritten(Err(message)) => match state.webhook.mocks.as_mut() {
            Some(editor) => editor.error = Some(message),
            None => state.notify(Severity::Error, message),
        },
        Msg::Done(Ok(message)) => state.notify(Severity::Success, message),
        Msg::Done(Err(message)) => state.notify(Severity::Error, message),
        _ => {}
    }
    Vec::new()
}

/// Stop the listener when the UI quits.
pub fn shut_down(state: &mut State) {
    if let Some(running) = running(&state.webhook) {
        running.stop.cancel();
    }
}
