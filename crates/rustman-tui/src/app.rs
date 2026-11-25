//! Application state, actions, effects and the pure `update` function
//! (design D2).
//!
//! `update` never performs I/O: anything that touches the disk or the network
//! is returned as an [`Effect`], executed by the effect runner, and comes back
//! as an [`Action`].

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseEvent};
use ratatui::layout::Rect;
use rustman_core::model::Request;
use rustman_core::snapshot::SnapshotMode;
use rustman_core::{Secret, Workspace};

use crate::commands;
use crate::config::UiOptions;
use crate::dialogs::{Confirm, Input};
use crate::files::FileOp;
use crate::graphql_schema::{SchemaJob, SchemaLoaded};
use crate::keymap::{Chord, Keymap};
use crate::launch::Launch;
use crate::mcp::{self, McpPane};
use crate::palette::{Palette, Target};
use crate::response::html::ReportJob;
use crate::response::json_tree::JsonTree;
use crate::response::picker::{DiffJob, DiffView, Earlier, Picker};
use crate::response::{Received, Source};
use crate::reveal::Revealed;
use crate::send::SendJob;
use crate::shell::{self, ShellState};
use crate::status::{StatusInfo, VimMode};
use crate::tabs::{TabId, Tabs};
use crate::toast::{Notifications, Severity};
use crate::vim::Vim;
use crate::viz::{self, VizError, VizJob};
use crate::webhook::{self, WebhookPane};
use crate::workbench::{self, Pending};
use crate::{graphql, reveal, review, send, viewer_keys};

/// The whole UI state; `view` renders it and `update` changes it.
#[derive(Debug)]
pub struct State {
    pub screen: Screen,
    /// Set by the quit action; the loop exits after the current batch.
    pub quit: bool,
    /// Why the loop stopped without the user quitting, if it did.
    pub abort: Option<Abort>,
    pub ui: UiOptions,
    pub shell: ShellState,
    pub status: StatusInfo,
    pub notifications: Notifications,
    /// A modal drawn over the shell.
    pub overlay: Option<Overlay>,
    /// The terminal size, from the last resize (for mouse hit-testing).
    pub area: Rect,
    /// The clock as of the last tick.
    pub now: Instant,
    /// The effective keymap (defaults, vim preset, `keybindings.toml`).
    pub keymap: Keymap,
    /// Keys typed so far of a multi-key binding (`g` of `gg`).
    pub pending_keys: Vec<Chord>,
    /// The open request tabs.
    pub tabs: Tabs,
    /// Saves, reloads and file operations in progress.
    pub pending: Pending,
    /// Modal editing state for text areas, when vim keys are on.
    pub vim: Option<Vim>,
    /// Recently used palette entries, most recent first (this session only).
    pub recent: Vec<Target>,
    /// Text for the terminal's clipboard (OSC 52), written by the loop.
    pub clipboard: Option<String>,
    /// The terminal takes OSC 52 ("Copy as…" copies through it).
    pub osc52: bool,
    /// The secret store backend sends use; `None`: `RUSTMAN_SECRET_STORE`.
    pub secret_backend: Option<rustman_core::SecretBackend>,
    /// Secret values read while secrets are revealed (memory only).
    pub revealed: Revealed,
    /// Where the viz companion is looked for (read once per session).
    pub viz: viz::Discovery,
    /// The webhook inspector pane and its listener.
    pub webhook: WebhookPane,
    /// How sends treat snapshot files: CI mode (`CI=true`) never writes.
    pub snapshot_mode: SnapshotMode,
    /// The mock server pane and its server.
    pub mock: crate::mock_server::MockPane,
    /// The MCP inspector pane and its session.
    pub mcp: McpPane,
    /// Checks and records the workspace plugin trust; empty without plugin
    /// support.
    pub plugin_gate: crate::plugin_trust::Gate,
    /// The request kinds of the workspace's trusted plugins, with the panes
    /// they declare.
    pub plugin_kinds: Vec<rustman_core::plugins::PluginKind>,
    /// The traffic recorder pane and its proxy.
    pub recorder: crate::recorder::RecorderPane,
}

/// Modals drawn over the shell.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Overlay {
    /// The notification log (spec: Toasts).
    Notifications,
    /// The key bindings (spec: Help overlay), scrolled by `scroll` rows.
    Help { scroll: u16 },
    /// The command palette or the environment switcher.
    Palette(Box<Palette>),
    /// A question: Save / Discard / Cancel, delete, conflicts.
    Confirm(Confirm),
    /// A one-line text prompt: new request, rename, save as.
    Input(Input),
    /// Earlier exchanges to diff the current one against.
    Exchanges(Box<Picker>),
    /// The differences between two exchanges.
    Diff(Box<DiffView>),
    /// The "Copy as…" language picker.
    CopyAs(Box<crate::copy_as::Picker>),
    /// A snippet to select by hand (no OSC 52, or too large).
    Snippet(Box<crate::copy_as::SnippetPane>),
    /// The "Import…" dialog and its preview.
    Import(Box<crate::import_dialog::ImportDialog>),
    /// The snapshot review queue (`Alt+A`).
    SnapshotQueue(Box<review::Queue>),
    /// The Monitors view.
    Monitors(Box<crate::monitors::MonitorsView>),
    /// The workspace plugin trust prompt (add-plugin-system).
    PluginTrust(Box<rustman_core::plugins::PluginTrustPrompt>),
    /// A GraphQL result's addresses and hashes, into the EVM toolkit.
    ChainLinks(Box<crate::chain_links::ChainLinks>),
    /// The signing gate asks: the keystore password, the confirmation of
    /// the decoded summary, or the typed chain name.
    Sign(Box<crate::evm_sign::SignDialog>),
}

/// What fills the terminal.
#[derive(Debug)]
pub enum Screen {
    /// The workspace at this root is being loaded (or created).
    Loading(PathBuf),
    /// No workspace was found: create one or open a scratch session.
    Prompt(Prompt),
    /// The main shell over a workspace or a scratch session.
    Shell(Session),
    /// The workspace could not be loaded.
    Failed(String),
}

/// What the shell is working on.
#[derive(Debug)]
pub enum Session {
    Workspace(Box<Workspace>),
    /// An empty session that is not backed by any workspace on disk.
    Scratch,
}

impl Session {
    /// How the session is named in the UI.
    pub fn title(&self) -> String {
        match self {
            Self::Workspace(ws) => ws.settings.workspace.name.clone(),
            Self::Scratch => "Scratch session".to_owned(),
        }
    }
}

/// The "no workspace found" prompt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Prompt {
    /// Where "Create workspace here" creates `rustman.toml`.
    pub dir: PathBuf,
    pub selected: Choice,
    /// Why the last attempt to create a workspace failed.
    pub error: Option<String>,
}

impl Prompt {
    pub fn new(dir: PathBuf) -> Self {
        Self {
            dir,
            selected: Choice::CreateWorkspace,
            error: None,
        }
    }
}

/// The choices offered when no workspace is found.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Choice {
    CreateWorkspace,
    Scratch,
}

impl Choice {
    pub const ALL: [Self; 2] = [Self::CreateWorkspace, Self::Scratch];

    /// The label shown in the prompt (spec: No workspace found).
    pub fn label(self) -> &'static str {
        match self {
            Self::CreateWorkspace => "Create workspace here",
            Self::Scratch => "Scratch session",
        }
    }

    fn other(self) -> Self {
        match self {
            Self::CreateWorkspace => Self::Scratch,
            Self::Scratch => Self::CreateWorkspace,
        }
    }
}

/// Everything that can change the state: input and effect completions.
#[derive(Debug)]
pub enum Action {
    Key(KeyEvent),
    Mouse(MouseEvent),
    /// Time passed; expires toasts. Sent by the runtime, or by tests with a
    /// mocked clock.
    Tick(Instant),
    /// Show a toast and log it.
    Notify(Severity, String),
    /// The terminal was resized; the next draw re-lays out from the state.
    Resize {
        width: u16,
        height: u16,
    },
    WorkspaceLoaded(Result<Box<Workspace>, String>),
    /// `rustman init` ran in this directory.
    WorkspaceCreated(Result<PathBuf, String>),
    /// Stop now: a termination signal arrived or background work failed.
    Abort(Abort),
    /// Workspace files changed on disk (workspace-relative paths).
    FilesChanged(Vec<PathBuf>),
    /// A tab was written to `path` (workspace-relative).
    Saved {
        tab: TabId,
        path: PathBuf,
        result: Result<(), String>,
    },
    /// A sidebar file operation finished.
    FileDone {
        op: FileOp,
        result: Result<(), String>,
    },
    /// An open request was read again after a change on disk.
    RequestRead {
        tab: TabId,
        result: Result<Box<Request>, String>,
    },
    /// The external editor returned the edited text.
    ExternalEdited {
        tab: TabId,
        result: Result<crate::external::Edited, String>,
    },
    /// A tab's send `seq` finished (sent by the send effect).
    SendFinished {
        tab: TabId,
        seq: u64,
        received: Box<Received>,
    },
    /// The matches of a JSONPath filter in a tab's body.
    Filtered {
        tab: TabId,
        expr: String,
        result: Result<Arc<JsonTree>, String>,
    },
    /// An off-loop tree search finished (`None`: it was cancelled).
    Searched {
        tab: TabId,
        generation: u64,
        matches: Option<Vec<usize>>,
    },
    /// A page of the paged raw view was read.
    PageRead {
        tab: TabId,
        page: u64,
        result: Result<Vec<String>, String>,
    },
    /// Earlier exchanges were read from history for the diff picker.
    ExchangesLoaded {
        tab: TabId,
        result: Result<Vec<Earlier>, String>,
    },
    /// A diff picked in the exchange picker was computed.
    Diffed(Box<DiffView>),
    /// A response body was saved to `path`.
    BodySaved {
        path: PathBuf,
        result: Result<(), String>,
    },
    /// A trace link was handed to the browser, or could not be.
    UrlOpened {
        url: String,
        result: Result<(), String>,
    },
    /// Text was copied with a clipboard tool (`None`: no tool worked).
    Copied {
        text: String,
        tool: Option<&'static str>,
    },
    /// The secrets of `environment` were read for the reveal toggle.
    SecretsRead {
        environment: String,
        result: Result<Vec<Secret<String>>, String>,
    },
    /// The viz hand-off finished.
    VizDone(Result<String, VizError>),
    /// An HTML report was written to `path`.
    ReportWritten(Result<PathBuf, String>),
    /// A webhook listener event or a webhook job finished.
    Webhook(Box<webhook::Msg>),
    /// A mock server event or a mock pane job finished.
    Mock(Box<crate::mock_server::Msg>),
    /// An MCP session event or an MCP inspector job finished.
    Mcp(Box<mcp::Msg>),
    /// A recording proxy event or a recorder job finished.
    Recorder(Box<crate::recorder::Msg>),
    /// A GraphQL tab's schema was loaded (or refreshed).
    GraphqlSchema {
        tab: TabId,
        refresh: bool,
        result: Result<Box<SchemaLoaded>, String>,
    },
    /// An EVM lookup from a GraphQL result link finished (redacted).
    EvmLookup(Result<Vec<String>, String>),
    /// A gRPC tab's schema was loaded (or refreshed).
    GrpcSchema {
        tab: TabId,
        refresh: bool,
        result: Result<Box<crate::grpc_pane::LoadedSchema>, String>,
    },
    /// A "Copy as…" snippet was made.
    SnippetMade {
        lang: rustman_http::Lang,
        result: Result<rustman_http::Snippet, String>,
    },
    /// An import (or its dry run) finished.
    Imported(Box<crate::import_dialog::ImportDone>),
    /// An approved snapshot was written (or not).
    SnapshotApproved(Box<review::Approved>),
    /// The Monitors view's load or run finished.
    MonitorsDone(Box<crate::monitors::MonitorsDone>),
    /// A signing job showed a line or asks a question.
    Sign(Box<crate::evm_sign::Msg>),
}

/// Why the loop stopped without the user quitting.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Abort {
    /// A termination signal (`SIGTERM`, `SIGHUP`, `SIGINT`, or a console
    /// close on Windows); `code` is the exit status to end with.
    Signal { name: &'static str, code: u8 },
    /// A background task panicked or the terminal input failed.
    Failed(String),
}

/// Side effects requested by `update`, run by the effect runner.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Effect {
    LoadWorkspace(PathBuf),
    CreateWorkspace(PathBuf),
    /// Write tab `tab` to `path` (workspace-relative) with `op`.
    Save {
        tab: TabId,
        path: PathBuf,
        op: FileOp,
    },
    /// Create, rename or delete a file from the sidebar.
    File(FileOp),
    /// Read the request file `path` of the workspace at `root` again.
    ReadRequest {
        tab: TabId,
        root: PathBuf,
        path: PathBuf,
    },
    /// Edit `text` in the external editor, in a temp file `*.extension`.
    /// The loop suspends the terminal around it.
    EditExternally {
        tab: TabId,
        text: String,
        extension: &'static str,
    },
    /// Send a tab's request through the engine.
    Send(Box<SendJob>),
    /// Apply the JSONPath `expr` to a tab's body.
    Filter {
        tab: TabId,
        expr: String,
        source: Source,
    },
    /// Search a large JSON tree for an incremental search.
    Search {
        tab: TabId,
        job: crate::response::SearchJob,
    },
    /// Read page `page` of a tab's body for the paged raw view.
    ReadPage {
        tab: TabId,
        page: u64,
        source: Source,
    },
    /// Read earlier exchanges from the history at `dir` (those of `file`
    /// first).
    LoadExchanges {
        tab: TabId,
        dir: PathBuf,
        file: String,
    },
    /// Diff an earlier exchange against the current one.
    Diff(Box<DiffJob>),
    /// Write a (redacted) response body to `path`.
    SaveBody {
        source: Source,
        path: PathBuf,
    },
    /// Put text on the system clipboard.
    Copy(String),
    /// Open a trace link in the system browser.
    OpenUrl(crate::trace_link::LinkJob),
    /// Read the secrets `names` of `environment` for the reveal toggle.
    ReadSecrets {
        root: PathBuf,
        environment: String,
        names: Vec<String>,
        backend: Option<rustman_core::SecretBackend>,
    },
    /// Open the current exchange in the viz companion.
    Viz(Box<VizJob>),
    /// Write the current exchange as an HTML report.
    WriteReport(Box<ReportJob>),
    /// Webhook pane work: settings, the listener, mocks, save and replay.
    Webhook(Box<webhook::Job>),
    /// Mock pane work: settings and the server.
    Mock(Box<crate::mock_server::Job>),
    /// MCP inspector work: servers, trust, the session, saving a call.
    Mcp(Box<mcp::Job>),
    /// Recorder pane work: settings, the proxy, save and HAR export.
    Recorder(Box<crate::recorder::Job>),
    /// Load (or refresh) a GraphQL tab's schema.
    GraphqlSchema(Box<SchemaJob>),
    /// Decode a transaction or read a balance from a GraphQL result link.
    EvmLookup(Box<crate::chain_links::LookupJob>),
    /// Load (or refresh) a gRPC tab's schema.
    GrpcSchema(Box<crate::grpc::SchemaJob>),
    /// Make a "Copy as…" snippet.
    Snippet(Box<crate::copy_as::SnippetJob>),
    /// Run an import or its dry run.
    Import(Box<crate::import_dialog::ImportJob>),
    /// Write an approved snapshot.
    ApproveSnapshot(Box<review::ApproveJob>),
    /// Load the monitors or run one.
    Monitors(Box<crate::monitors::MonitorJob>),
}

impl State {
    /// The initial state for `launch`, with the effects that start it.
    pub fn new(launch: Launch) -> (Self, Vec<Effect>) {
        match launch {
            Launch::Workspace(root) => (
                Self::with_screen(Screen::Loading(root.clone())),
                vec![Effect::LoadWorkspace(root)],
            ),
            Launch::NoWorkspace(dir) => {
                (Self::with_screen(Screen::Prompt(Prompt::new(dir))), vec![])
            }
        }
    }

    pub fn with_screen(screen: Screen) -> Self {
        Self {
            screen,
            quit: false,
            abort: None,
            ui: UiOptions::default(),
            shell: ShellState::default(),
            status: StatusInfo::default(),
            notifications: Notifications::default(),
            overlay: None,
            area: Rect::new(0, 0, 80, 24),
            now: Instant::now(),
            keymap: Keymap::default(),
            pending_keys: Vec::new(),
            tabs: Tabs::default(),
            pending: Pending::default(),
            vim: None,
            recent: Vec::new(),
            clipboard: None,
            osc52: false,
            secret_backend: None,
            revealed: Revealed::default(),
            viz: viz::Discovery::default(),
            webhook: WebhookPane::default(),
            snapshot_mode: SnapshotMode::Record,
            mock: crate::mock_server::MockPane::default(),
            mcp: McpPane::default(),
            plugin_gate: crate::plugin_trust::Gate::default(),
            plugin_kinds: Vec::new(),
            recorder: crate::recorder::RecorderPane::default(),
        }
    }

    /// Use this keymap; a vim keymap also turns on modal editing.
    #[must_use]
    pub fn with_keymap(mut self, keymap: Keymap) -> Self {
        self.vim = keymap.vim.then(Vim::default);
        self.status.vim = keymap.vim.then_some(VimMode::Normal);
        self.keymap = keymap;
        self
    }

    /// Use these UI options (theme, glyphs, mouse).
    #[must_use]
    pub fn with_ui(mut self, ui: UiOptions) -> Self {
        self.ui = ui;
        self
    }

    /// Show a toast stamped with the current clock.
    pub fn notify(&mut self, severity: Severity, message: impl Into<String>) {
        self.notifications.push(self.now, severity, message);
    }
}

/// Apply `action` to `state` and return the effects to run.
pub fn update(state: &mut State, action: Action) -> Vec<Effect> {
    let mut effects = dispatch(state, action);
    effects.extend(graphql::pending_loads(state));
    effects.extend(crate::grpc::pending_loads(state));
    crate::jsonrpc::pending_catalogs(state);
    workbench::ask_conflict(state);
    state.status.tls_insecure = tls_insecure(state);
    state.status.otlp = otlp_host(state);
    effects
}

/// The collector the active environment's sends export spans to
/// (`[trace.export]`), when this build can export.
fn otlp_host(state: &State) -> Option<String> {
    let ws = workbench::workspace(state)?;
    let environment = state
        .status
        .environment
        .as_deref()
        .and_then(|name| ws.environments.iter().find(|e| e.name == name));
    let config = match environment {
        Some(env) => env.trace.over(&ws.settings.trace),
        None => ws.settings.trace.clone(),
    };
    rustman_http::runner::export_host(&config)
}

/// TLS verification is off for the workspace or for the request in the
/// active tab (spec: Insecure TLS indicator).
fn tls_insecure(state: &State) -> bool {
    let Some(ws) = workbench::workspace(state) else {
        return false;
    };
    if !ws.settings.defaults.verify_tls {
        return true;
    }
    let Some(path) = state.tabs.active().and_then(|t| t.path.as_deref()) else {
        return false;
    };
    ws.requests()
        .iter()
        .find(|r| r.entry.path == path)
        .is_some_and(|r| match r.request() {
            Request::Http(http) => http
                .settings
                .as_ref()
                .is_some_and(|s| s.verify_tls == Some(false)),
            _ => false,
        })
}

fn dispatch(state: &mut State, action: Action) -> Vec<Effect> {
    match action {
        Action::Key(key) => on_key(state, key),
        Action::Mouse(event) => {
            shell::on_mouse(state, event);
            Vec::new()
        }
        Action::Tick(now) => {
            state.now = now;
            state.notifications.expire(now);
            Vec::new()
        }
        Action::Notify(severity, message) => {
            state.notify(severity, message);
            Vec::new()
        }
        Action::Resize { width, height } => {
            state.area = Rect::new(0, 0, width, height);
            shell::on_resize(state);
            Vec::new()
        }
        Action::WorkspaceLoaded(result) => on_loaded(state, result),
        Action::WorkspaceCreated(result) => on_created(state, result),
        Action::Abort(abort) => {
            state.quit = true;
            state.abort.get_or_insert(abort);
            Vec::new()
        }
        other => on_workbench(state, other),
    }
}

/// Completions of the request workbench.
fn on_workbench(state: &mut State, action: Action) -> Vec<Effect> {
    match action {
        Action::FilesChanged(paths) => workbench::on_files_changed(state, &paths),
        Action::Saved { tab, path, result } => workbench::on_saved(state, tab, path, result),
        Action::FileDone { op, result } => workbench::on_file_done(state, &op, result),
        Action::RequestRead { tab, result } => {
            workbench::on_request_read(state, tab, result);
            Vec::new()
        }
        Action::ExternalEdited { tab, result } => {
            workbench::on_external(state, tab, result);
            Vec::new()
        }
        Action::SendFinished { tab, seq, received } => {
            send::finished(state, tab, seq, *received);
            Vec::new()
        }
        Action::SecretsRead {
            environment,
            result,
        } => {
            reveal::on_read(state, environment, result);
            Vec::new()
        }
        Action::VizDone(result) => {
            viz::on_done(state, result);
            Vec::new()
        }
        Action::Webhook(msg) => webhook::on_msg(state, *msg),
        Action::Mock(msg) => crate::mock_server::on_msg(state, *msg),
        Action::Mcp(msg) => mcp::on_msg(state, *msg),
        Action::Recorder(msg) => crate::recorder::on_msg(state, *msg),
        Action::GraphqlSchema {
            tab,
            refresh,
            result,
        } => {
            graphql::on_loaded(state, tab, refresh, result);
            Vec::new()
        }
        Action::EvmLookup(result) => {
            crate::chain_links::on_lookup(state, result);
            Vec::new()
        }
        Action::GrpcSchema {
            tab,
            refresh,
            result,
        } => {
            crate::grpc::on_loaded(state, tab, refresh, result);
            Vec::new()
        }
        Action::SnippetMade { lang, result } => {
            crate::copy_as::on_made(state, lang, result);
            Vec::new()
        }
        Action::Imported(done) => crate::import_dialog::on_done(state, *done),
        Action::SnapshotApproved(done) => {
            review::on_approved(state, *done);
            Vec::new()
        }
        Action::MonitorsDone(done) => {
            crate::monitors::on_done(state, *done);
            Vec::new()
        }
        Action::Sign(msg) => crate::evm_sign::on_msg(state, *msg),
        other => viewer_keys::on_action(state, other),
    }
}

fn on_loaded(state: &mut State, result: Result<Box<Workspace>, String>) -> Vec<Effect> {
    if matches!(state.screen, Screen::Shell(Session::Workspace(_))) {
        return workbench::on_reloaded(state, result);
    }
    match result {
        Ok(ws) => open_workspace(state, ws),
        Err(message) => state.screen = Screen::Failed(message),
    }
    Vec::new()
}

/// Show the shell over a loaded workspace; its load warnings become toasts.
fn open_workspace(state: &mut State, ws: Box<Workspace>) {
    state.status.environment = ws
        .active_environment(None, None)
        .ok()
        .flatten()
        .map(|env| env.name.clone());
    for warning in &ws.warnings {
        state.notify(Severity::Warn, warning.clone());
    }
    state.screen = Screen::Shell(Session::Workspace(ws));
    crate::plugin_trust::check(state);
}

fn on_key(state: &mut State, key: KeyEvent) -> Vec<Effect> {
    // Windows reports releases too; only presses and repeats act.
    if key.kind == KeyEventKind::Release {
        return Vec::new();
    }
    if matches!(state.screen, Screen::Shell(_)) {
        return Chord::from_event(key).map_or_else(Vec::new, |c| commands::shell_key(state, c));
    }
    if is_quit(key) {
        state.quit = true;
        return Vec::new();
    }
    let Screen::Prompt(prompt) = &mut state.screen else {
        return Vec::new();
    };
    match prompt_key(prompt, key) {
        Some(Choice::CreateWorkspace) => {
            let dir = prompt.dir.clone();
            state.screen = Screen::Loading(dir.clone());
            vec![Effect::CreateWorkspace(dir)]
        }
        Some(Choice::Scratch) => {
            state.screen = Screen::Shell(Session::Scratch);
            Vec::new()
        }
        None => Vec::new(),
    }
}

/// `Ctrl+Q` quits outside the shell, where the keymap does not apply
/// (design D3); `Ctrl+C` never does.
fn is_quit(key: KeyEvent) -> bool {
    key.modifiers.contains(KeyModifiers::CONTROL) && matches!(key.code, KeyCode::Char('q' | 'Q'))
}

/// Move the prompt selection, or return the confirmed choice.
fn prompt_key(prompt: &mut Prompt, key: KeyEvent) -> Option<Choice> {
    match key.code {
        KeyCode::Up | KeyCode::Down | KeyCode::Tab | KeyCode::BackTab => {
            prompt.selected = prompt.selected.other();
            None
        }
        KeyCode::Char('k' | 'j') => {
            prompt.selected = prompt.selected.other();
            None
        }
        KeyCode::Enter => Some(prompt.selected),
        KeyCode::Char('c') => Some(Choice::CreateWorkspace),
        KeyCode::Char('s') => Some(Choice::Scratch),
        _ => None,
    }
}

fn on_created(state: &mut State, result: Result<PathBuf, String>) -> Vec<Effect> {
    match result {
        Ok(root) => {
            state.screen = Screen::Loading(root.clone());
            vec![Effect::LoadWorkspace(root)]
        }
        Err(message) => {
            let dir = match &state.screen {
                Screen::Loading(dir) => dir.clone(),
                _ => PathBuf::new(),
            };
            let mut prompt = Prompt::new(dir);
            prompt.error = Some(message);
            state.screen = Screen::Prompt(prompt);
            Vec::new()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn press(code: KeyCode) -> Action {
        Action::Key(KeyEvent::new(code, KeyModifiers::NONE))
    }

    fn ctrl(c: char) -> Action {
        Action::Key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL))
    }

    fn prompt_state(dir: &std::path::Path) -> State {
        let (state, effects) = State::new(Launch::NoWorkspace(dir.to_path_buf()));
        assert!(effects.is_empty());
        state
    }

    #[test]
    fn an_abort_quits_and_keeps_the_first_reason() {
        let mut state = State::with_screen(Screen::Shell(Session::Scratch));
        let term = Abort::Signal {
            name: "SIGTERM",
            code: 143,
        };
        update(&mut state, Action::Abort(term.clone()));
        update(&mut state, Action::Abort(Abort::Failed("later".into())));
        assert!(state.quit);
        assert_eq!(state.abort, Some(term));
    }

    #[test]
    fn a_found_workspace_is_loaded_by_an_effect_not_by_new() {
        let (state, effects) = State::new(Launch::Workspace("/proj".into()));
        assert!(matches!(state.screen, Screen::Loading(ref root) if root.as_os_str() == "/proj"));
        assert_eq!(effects, vec![Effect::LoadWorkspace("/proj".into())]);
    }

    #[test]
    fn update_returns_the_create_effect_without_touching_the_disk() {
        let tmp = tempfile::tempdir().unwrap();
        let mut state = prompt_state(tmp.path());
        let effects = update(&mut state, press(KeyCode::Enter));
        assert_eq!(
            effects,
            vec![Effect::CreateWorkspace(tmp.path().to_path_buf())]
        );
        assert!(!tmp.path().join("rustman.toml").exists());
        assert!(matches!(state.screen, Screen::Loading(_)));
    }

    #[test]
    fn a_created_workspace_is_then_loaded() {
        let mut state = State::with_screen(Screen::Loading("/new".into()));
        let effects = update(&mut state, Action::WorkspaceCreated(Ok("/new".into())));
        assert_eq!(effects, vec![Effect::LoadWorkspace("/new".into())]);
    }

    #[test]
    fn a_failed_create_returns_to_the_prompt_with_the_error() {
        let mut state = State::with_screen(Screen::Loading("/ro".into()));
        let effects = update(&mut state, Action::WorkspaceCreated(Err("denied".into())));
        assert!(effects.is_empty());
        let Screen::Prompt(prompt) = &state.screen else {
            panic!("expected the prompt, got {:?}", state.screen);
        };
        assert_eq!(prompt.dir.as_os_str(), "/ro");
        assert_eq!(prompt.error.as_deref(), Some("denied"));
    }

    #[test]
    fn arrows_move_the_selection_and_enter_opens_a_scratch_session() {
        let mut state = prompt_state(std::path::Path::new("/x"));
        update(&mut state, press(KeyCode::Down));
        let Screen::Prompt(prompt) = &state.screen else {
            panic!("expected the prompt");
        };
        assert_eq!(prompt.selected, Choice::Scratch);
        assert!(update(&mut state, press(KeyCode::Enter)).is_empty());
        assert!(matches!(state.screen, Screen::Shell(Session::Scratch)));
    }

    #[test]
    fn shortcut_letters_pick_a_choice() {
        let mut state = prompt_state(std::path::Path::new("/x"));
        update(&mut state, press(KeyCode::Char('s')));
        assert!(matches!(state.screen, Screen::Shell(Session::Scratch)));
        let mut state = prompt_state(std::path::Path::new("/x"));
        let effects = update(&mut state, press(KeyCode::Char('c')));
        assert_eq!(effects, vec![Effect::CreateWorkspace("/x".into())]);
    }

    #[test]
    fn ctrl_q_quits_and_ctrl_c_does_not() {
        let mut state = State::with_screen(Screen::Shell(Session::Scratch));
        update(&mut state, ctrl('c'));
        assert!(!state.quit);
        update(&mut state, ctrl('q'));
        assert!(state.quit);
    }

    #[test]
    fn key_releases_are_ignored() {
        let mut state = prompt_state(std::path::Path::new("/x"));
        let mut key = KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE);
        key.kind = KeyEventKind::Release;
        assert!(update(&mut state, Action::Key(key)).is_empty());
        assert!(matches!(state.screen, Screen::Prompt(_)));
    }

    #[test]
    fn a_loaded_workspace_fills_the_status_and_toasts_warnings() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("demo");
        rustman_core::init::init(&dir, "demo", false).unwrap();
        let toml = std::fs::read_to_string(dir.join("rustman.toml")).unwrap();
        let toml = toml.replace("[defaults]", "[defaults]\nverify_tls = false");
        std::fs::write(dir.join("rustman.toml"), toml).unwrap();
        let mut ws = Workspace::load(&dir).unwrap();
        ws.warnings.push("unknown key `colour`".into());
        let mut state = State::with_screen(Screen::Loading(dir));
        update(&mut state, Action::WorkspaceLoaded(Ok(Box::new(ws))));
        assert!(state.status.tls_insecure);
        assert_eq!(state.status.environment.as_deref(), Some("local"));
        let toast = state.notifications.visible().next().unwrap();
        assert_eq!(
            (toast.severity, toast.message.as_str()),
            (Severity::Warn, "unknown key `colour`")
        );
    }

    #[test]
    fn ticks_expire_toasts_on_the_mocked_clock() {
        let mut state = State::with_screen(Screen::Shell(Session::Scratch));
        let t0 = state.now;
        update(&mut state, Action::Notify(Severity::Info, "copied".into()));
        update(
            &mut state,
            Action::Notify(Severity::Error, "Permission denied (os error 13)".into()),
        );
        update(
            &mut state,
            Action::Tick(t0 + std::time::Duration::from_secs(3)),
        );
        let left: Vec<_> = state.notifications.visible().map(|t| t.severity).collect();
        assert_eq!(left, [Severity::Error]);
        update(
            &mut state,
            Action::Tick(t0 + std::time::Duration::from_secs(10)),
        );
        assert_eq!(state.notifications.visible().len(), 0);
        assert_eq!(state.notifications.log().len(), 2);
    }

    #[test]
    fn a_load_error_is_shown() {
        let mut state = State::with_screen(Screen::Loading("/p".into()));
        update(&mut state, Action::WorkspaceLoaded(Err("bad file".into())));
        assert!(matches!(state.screen, Screen::Failed(ref m) if m == "bad file"));
    }
}
