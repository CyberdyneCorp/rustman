//! The traffic recorder pane (openspec change `add-traffic-recorder`,
//! spec: Live capture list): start and stop the recording proxy, follow
//! its captures live, filter them, mark several, inspect one (redacted),
//! and save the marked captures as request files or export them as HAR.
//!
//! The proxy runs on its own thread with its own runtime (see
//! [`jobs::spawn_proxy`]); each capture is announced by a
//! [`Msg::Captured`] action carrying nothing, and the pane re-reads the
//! bounded store, so a stalled UI never keeps evicted captures alive. A
//! network-reachable `[record] host` starts only after confirmation, and
//! the status bar shows `CA TRUSTED` while Rustman has the workspace's CA
//! installed in a trust store.

mod jobs;
mod view;

#[cfg(test)]
mod tests;

use std::collections::BTreeSet;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;

use rustman_proxy::config::remote_warning;
use rustman_proxy::{Capture, CaptureStore};
use tokio_util::sync::CancellationToken;

pub use jobs::{Job, Prepared, StartJob, execute, intercept, spawn_proxy};
pub use view::render;

use crate::app::{Effect, Overlay, State};
use crate::dialogs::{Confirm, Input, Purpose};
use crate::keymap::chord::{Chord, Code};
use crate::status::RecorderBadge;
use crate::toast::Severity;
use crate::workbench;

/// The HAR file name offered first.
pub const DEFAULT_HAR: &str = "recording.har";

/// The pane's state.
#[derive(Debug, Default)]
pub struct RecorderPane {
    /// The pane fills the shell (the proxy keeps running when closed).
    pub open: bool,
    pub proxy: Proxy,
    /// Every buffered capture, newest first.
    pub captures: Vec<Arc<Capture>>,
    /// The selected row of the filtered list.
    pub selected: usize,
    /// Marked capture ids, for save and export.
    pub marked: BTreeSet<u64>,
    /// Shown captures contain this text (case-insensitive).
    pub filter: String,
    /// Keys go to the filter.
    pub filtering: bool,
    /// How far the detail view is scrolled.
    pub scroll: usize,
    /// Rustman installed this workspace's CA in a trust store.
    pub ca_trusted: bool,
    /// The last HAR file name, offered again.
    pub har_name: Option<String>,
}

/// Where the proxy is in its lifecycle.
#[derive(Debug, Default)]
pub enum Proxy {
    #[default]
    Stopped,
    /// Reading `[record]` and the local CA.
    Preparing,
    /// Waiting for the user to accept a network-reachable address.
    Confirming(Arc<Prepared>),
    Starting,
    Running(Box<Running>),
    Stopping,
    Failed(String),
}

/// A running proxy as the pane sees it.
#[derive(Debug)]
pub struct Running {
    pub addr: SocketAddr,
    pub store: CaptureStore,
    /// Cancelling it stops the proxy thread.
    pub stop: CancellationToken,
    /// Host globs decrypted with the local CA.
    pub intercept: Vec<String>,
}

/// Completions and proxy events for the pane.
#[derive(Debug)]
pub enum Msg {
    Prepared(Result<Arc<Prepared>, String>),
    Started(Result<Box<Running>, String>),
    /// The store changed: a capture arrived (or several were missed).
    Captured,
    /// The proxy thread ended and released the port.
    Stopped,
    /// Whether the workspace's CA is installed in a trust store.
    CaTrusted(bool),
    /// A save or export finished: the toast text.
    Done(Result<String, String>),
}

/// The text prompts of the pane.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Prompt {
    /// The file to export the marked captures to.
    Har,
}

/// `recorder.open`: show or hide the pane; opening it re-reads whether the
/// CA is trusted.
pub fn toggle_pane(state: &mut State) -> Vec<Effect> {
    state.recorder.open = !state.recorder.open;
    match workbench::workspace(state) {
        Some(ws) if state.recorder.open => {
            let job = Job::CheckCa {
                root: ws.root.clone(),
            };
            vec![Effect::Recorder(Box::new(job))]
        }
        _ => Vec::new(),
    }
}

/// Keys while the pane is open; `None` leaves the key to the keymap.
pub fn key(state: &mut State, chord: Chord) -> Option<Vec<Effect>> {
    if !state.recorder.open || chord.ctrl || chord.alt {
        return None;
    }
    if state.recorder.filtering {
        filter_key(&mut state.recorder, chord);
        return Some(Vec::new());
    }
    let effects = match chord.code {
        Code::Esc => {
            state.recorder.open = false;
            Vec::new()
        }
        Code::Up | Code::Char('k') => move_selection(&mut state.recorder, -1),
        Code::Down | Code::Char('j') => move_selection(&mut state.recorder, 1),
        Code::Home | Code::Char('g') => move_selection(&mut state.recorder, isize::MIN),
        Code::End | Code::Char('G') => move_selection(&mut state.recorder, isize::MAX),
        Code::PageDown => scroll(&mut state.recorder, 10),
        Code::PageUp => scroll(&mut state.recorder, -10),
        Code::Char(c) => action_key(state, c)?,
        _ => return None,
    };
    Some(effects)
}

/// Letter keys; `?` stays the help key.
fn action_key(state: &mut State, c: char) -> Option<Vec<Effect>> {
    let effects = match c {
        's' => start_or_stop(state),
        ' ' => toggle_mark(&mut state.recorder),
        'a' => mark_all(&mut state.recorder),
        '/' => {
            state.recorder.filtering = true;
            Vec::new()
        }
        'w' => save(state),
        'e' => ask_har(state),
        '?' => return None,
        _ => Vec::new(),
    };
    Some(effects)
}

/// Typing in the filter: `Enter` keeps it, `Esc` clears it.
fn filter_key(pane: &mut RecorderPane, chord: Chord) {
    match chord.code {
        Code::Enter => pane.filtering = false,
        Code::Esc => {
            pane.filtering = false;
            pane.filter.clear();
        }
        Code::Backspace => {
            pane.filter.pop();
        }
        Code::Char(c) => pane.filter.push(c),
        _ => {}
    }
    pane.selected = 0;
    pane.scroll = 0;
}

/// The captures the filter shows, newest first.
pub fn visible(pane: &RecorderPane) -> Vec<&Arc<Capture>> {
    let needle = pane.filter.to_lowercase();
    pane.captures
        .iter()
        .filter(|c| needle.is_empty() || view::summary(c).to_lowercase().contains(&needle))
        .collect()
}

/// The selected capture.
pub fn selected(pane: &RecorderPane) -> Option<Arc<Capture>> {
    visible(pane).get(pane.selected).map(|c| Arc::clone(c))
}

fn move_selection(pane: &mut RecorderPane, by: isize) -> Vec<Effect> {
    let last = visible(pane).len().saturating_sub(1);
    let before = pane.selected;
    pane.selected = pane.selected.saturating_add_signed(by).min(last);
    if pane.selected != before {
        pane.scroll = 0;
    }
    Vec::new()
}

fn scroll(pane: &mut RecorderPane, by: isize) -> Vec<Effect> {
    pane.scroll = pane.scroll.saturating_add_signed(by);
    Vec::new()
}

fn toggle_mark(pane: &mut RecorderPane) -> Vec<Effect> {
    if let Some(capture) = selected(pane)
        && !pane.marked.remove(&capture.id)
    {
        pane.marked.insert(capture.id);
    }
    Vec::new()
}

/// Mark every shown capture, or unmark them all when they all are.
fn mark_all(pane: &mut RecorderPane) -> Vec<Effect> {
    let ids: Vec<u64> = visible(pane).iter().map(|c| c.id).collect();
    if ids.iter().all(|id| pane.marked.contains(id)) {
        pane.marked.clear();
    } else {
        pane.marked.extend(ids);
    }
    Vec::new()
}

/// The marked captures, oldest first; the selected one when none is.
fn chosen(pane: &RecorderPane) -> Vec<Arc<Capture>> {
    let mut chosen: Vec<Arc<Capture>> = pane
        .captures
        .iter()
        .filter(|c| pane.marked.contains(&c.id))
        .cloned()
        .collect();
    if chosen.is_empty() {
        chosen.extend(selected(pane));
    }
    chosen.reverse();
    chosen
}

fn running(pane: &RecorderPane) -> Option<&Running> {
    match &pane.proxy {
        Proxy::Running(running) => Some(running),
        _ => None,
    }
}

fn start_or_stop(state: &mut State) -> Vec<Effect> {
    match &state.recorder.proxy {
        Proxy::Running(running) => {
            running.stop.cancel();
            state.recorder.proxy = Proxy::Stopping;
            Vec::new()
        }
        Proxy::Stopped | Proxy::Failed(_) => {
            state.recorder.proxy = Proxy::Preparing;
            let job = Job::Prepare {
                root: workbench::workspace(state).map(|ws| ws.root.clone()),
                backend: state.secret_backend,
            };
            vec![Effect::Recorder(Box::new(job))]
        }
        _ => Vec::new(),
    }
}

/// Apply a completion or proxy event.
pub fn on_msg(state: &mut State, msg: Msg) -> Vec<Effect> {
    let effects = match msg {
        Msg::Prepared(result) => on_prepared(state, result),
        Msg::Started(Ok(running)) => {
            on_started(state, running);
            Vec::new()
        }
        Msg::Started(Err(message)) => {
            fail(state, message);
            Vec::new()
        }
        Msg::Captured => {
            refresh(&mut state.recorder);
            Vec::new()
        }
        Msg::Stopped => {
            if !matches!(state.recorder.proxy, Proxy::Failed(_)) {
                state.recorder.proxy = Proxy::Stopped;
            }
            Vec::new()
        }
        Msg::CaTrusted(trusted) => {
            state.recorder.ca_trusted = trusted;
            Vec::new()
        }
        Msg::Done(Ok(message)) => notify(state, Severity::Success, message),
        Msg::Done(Err(message)) => notify(state, Severity::Error, message),
    };
    sync_status(state);
    effects
}

fn notify(state: &mut State, severity: Severity, message: String) -> Vec<Effect> {
    state.notify(severity, message);
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
    state.recorder.ca_trusted = prepared.ca_trusted;
    let mut probe = prepared.config.clone();
    probe.allow_remote = true;
    match probe.bind_addr() {
        Err(err) => {
            fail(state, err.to_string());
            Vec::new()
        }
        Ok(addr) if !addr.ip().is_loopback() => {
            state.recorder.proxy = Proxy::Confirming(prepared);
            let confirm = Confirm::recorder_remote(&remote_warning(addr));
            state.overlay = Some(Overlay::Confirm(confirm));
            Vec::new()
        }
        Ok(_) => start(state, prepared),
    }
}

fn start(state: &mut State, prepared: Arc<Prepared>) -> Vec<Effect> {
    state.recorder.proxy = Proxy::Starting;
    let job = StartJob {
        prepared,
        stop: CancellationToken::new(),
    };
    vec![Effect::Recorder(Box::new(Job::Start(job)))]
}

/// The user accepted a network-reachable address.
pub fn confirm_remote(state: &mut State) -> Vec<Effect> {
    let Proxy::Confirming(prepared) = std::mem::take(&mut state.recorder.proxy) else {
        return Vec::new();
    };
    let mut allowed = (*prepared).clone();
    allowed.config.allow_remote = true;
    start(state, Arc::new(allowed))
}

/// A dialog was cancelled: a pending remote start does not happen.
pub fn cancel_confirm(state: &mut State) {
    if matches!(state.recorder.proxy, Proxy::Confirming(_)) {
        state.recorder.proxy = Proxy::Stopped;
    }
}

fn on_started(state: &mut State, running: Box<Running>) {
    if !running.addr.ip().is_loopback() {
        state.notify(Severity::Warn, remote_warning(running.addr));
    }
    state.recorder.captures.clear();
    state.recorder.marked.clear();
    state.recorder.selected = 0;
    state.recorder.proxy = Proxy::Running(running);
    refresh(&mut state.recorder);
}

fn fail(state: &mut State, message: String) {
    state.notify(Severity::Error, message.clone());
    state.recorder.proxy = Proxy::Failed(message);
}

/// Mirror the store, newest first: the top row follows the newest
/// capture, any other selection stays on its capture.
fn refresh(pane: &mut RecorderPane) {
    let Some(running) = running(pane) else { return };
    let before = selected(pane).filter(|_| pane.selected > 0).map(|c| c.id);
    let mut captures = running.store.snapshot();
    captures.reverse();
    pane.captures = captures;
    let kept: BTreeSet<u64> = pane.captures.iter().map(|c| c.id).collect();
    pane.marked.retain(|id| kept.contains(id));
    let shown = visible(pane);
    pane.selected = before
        .and_then(|id| shown.iter().position(|c| c.id == id))
        .unwrap_or(0);
}

/// The status bar follows the proxy and the CA.
fn sync_status(state: &mut State) {
    state.status.recorder = running(&state.recorder).map(|r| RecorderBadge {
        addr: r.addr.to_string(),
        remote: !r.addr.ip().is_loopback(),
    });
    state.status.ca_trusted = state.recorder.ca_trusted;
}

fn nothing_chosen(state: &mut State) -> Vec<Effect> {
    state.notify(Severity::Info, "No capture selected");
    Vec::new()
}

/// Save the marked captures as request files (secrets as placeholders,
/// their values kept nowhere: the TUI never stores them).
fn save(state: &mut State) -> Vec<Effect> {
    let captures = chosen(&state.recorder);
    if captures.is_empty() {
        return nothing_chosen(state);
    }
    let Some(root) = workbench::workspace(state).map(|ws| ws.root.clone()) else {
        state.notify(Severity::Info, "Saving captures needs a workspace");
        return Vec::new();
    };
    vec![Effect::Recorder(Box::new(Job::Save { root, captures }))]
}

fn ask_har(state: &mut State) -> Vec<Effect> {
    if chosen(&state.recorder).is_empty() {
        return nothing_chosen(state);
    }
    let name = state
        .recorder
        .har_name
        .clone()
        .unwrap_or_else(|| DEFAULT_HAR.to_owned());
    let input = Input::new(
        " Export HAR ",
        "File:",
        Purpose::Recorder(Prompt::Har),
        &name,
    );
    state.overlay = Some(Overlay::Input(input));
    Vec::new()
}

/// A pane prompt was submitted with `text`.
pub fn submit(state: &mut State, prompt: Prompt, text: &str) -> Vec<Effect> {
    state.overlay = None;
    let name = text.trim().to_owned();
    match prompt {
        Prompt::Har if name.is_empty() => Vec::new(),
        Prompt::Har => {
            let captures = chosen(&state.recorder);
            let base = workbench::workspace(state).map_or_else(PathBuf::new, |ws| ws.root.clone());
            state.recorder.har_name = Some(name.clone());
            let job = Job::ExportHar {
                path: base.join(name),
                captures,
            };
            vec![Effect::Recorder(Box::new(job))]
        }
    }
}

/// Stop the proxy when the UI quits.
pub fn shut_down(state: &mut State) {
    if let Some(running) = running(&state.recorder) {
        running.stop.cancel();
    }
}
