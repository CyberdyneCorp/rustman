//! "Copy as…" (runner-parity spec: Copy as picker, Clipboard via OSC 52,
//! Secrets in copied snippets): the focused request as code for one of
//! the snippet languages, made by the engine's snippet engine off the
//! loop and put on the clipboard through OSC 52.
//!
//! `Alt+C` opens a picker of the languages, the most recently used first;
//! each language is also a palette entry. Snippets are redacted unless
//! secrets are revealed (`Alt+R`); then the picker warns and copying takes
//! a second `Enter`. When the terminal does not take OSC 52, or the
//! snippet is over [`OSC52_LIMIT`], it opens in a read-only pane instead.

use std::fmt;
use std::path::PathBuf;

use rustman_core::collection::RequestEntry;
use rustman_core::model::Request;
use rustman_core::settings::Settings;
use rustman_core::workspace::RequestRef;
use rustman_core::{Environment, SecretBackend, Workspace};
use rustman_http::snippet::{RequestSnippet, request_snippet};
use rustman_http::{Lang, Snippet};

use crate::app::{Action, Effect, Overlay, State};
use crate::keymap::chord::{Chord, Code};
use crate::palette::{self, Entry, Target};
use crate::toast::Severity;
use crate::{clipboard, send, workbench};

/// The longest OSC 52 sequence sent, base64 and framing included (about
/// a 75 KB snippet); larger snippets open in the read-only pane, since
/// many terminals drop longer sequences.
pub const OSC52_LIMIT: usize = 100 * 1024;

/// The picker's warning row while secrets are revealed.
pub const WARNING: &str = "Secrets revealed: snippet will contain secret values";

/// The hint of the read-only snippet pane.
pub const SELECT_HINT: &str = "Select to copy";

/// The open "Copy as…" picker.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Picker {
    pub query: String,
    /// Index into `ranked`.
    pub selected: usize,
    pub entries: Vec<Entry>,
    /// Indices into `entries`, best first.
    pub ranked: Vec<usize>,
    /// Secrets are revealed: the snippet will hold their values.
    pub revealed: bool,
    /// The first `Enter` was pressed while revealed; the next one copies.
    pub armed: bool,
}

impl Picker {
    pub fn new(recent: &[Target], revealed: bool) -> Self {
        let entries = Lang::ALL
            .map(|lang| Entry {
                target: Target::Snippet(lang),
                label: lang.id().to_owned(),
                detail: String::new(),
            })
            .to_vec();
        let mut picker = Self {
            query: String::new(),
            selected: 0,
            entries,
            ranked: Vec::new(),
            revealed,
            armed: false,
        };
        picker.set_query("", recent);
        picker
    }

    pub fn set_query(&mut self, query: &str, recent: &[Target]) {
        query.clone_into(&mut self.query);
        self.ranked = palette::rank(&self.entries, query, recent);
        self.selected = 0;
        self.armed = false;
    }

    fn move_by(&mut self, delta: isize) {
        let len = isize::try_from(self.ranked.len()).unwrap_or(isize::MAX);
        if len > 0 {
            let current = isize::try_from(self.selected).unwrap_or(0);
            self.selected = usize::try_from((current + delta).rem_euclid(len)).unwrap_or(0);
        }
        self.armed = false;
    }

    /// The selected language.
    pub fn selected(&self) -> Option<Lang> {
        let entry = &self.entries[*self.ranked.get(self.selected)?];
        match entry.target {
            Target::Snippet(lang) => Some(lang),
            _ => None,
        }
    }

    /// The ranked entries.
    pub fn visible(&self) -> impl Iterator<Item = &Entry> {
        self.ranked.iter().map(|&i| &self.entries[i])
    }
}

/// A snippet shown for manual copying (no OSC 52, or too large).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SnippetPane {
    pub lang: Lang,
    pub code: String,
    pub scroll: u16,
}

/// Everything making a snippet needs, owned, so it runs off the loop.
#[derive(Clone)]
pub struct SnippetJob {
    pub lang: Lang,
    pub request: Request,
    /// The tab's file, relative to the workspace root.
    pub path: Option<PathBuf>,
    /// `None` in a scratch session.
    pub workspace: Option<Box<Workspace>>,
    pub environment: Option<String>,
    /// Secrets are revealed: put their values in the snippet.
    pub show_secrets: bool,
    pub backend: Option<SecretBackend>,
}

impl fmt::Debug for SnippetJob {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SnippetJob")
            .field("lang", &self.lang)
            .field("path", &self.path)
            .field("environment", &self.environment)
            .field("show_secrets", &self.show_secrets)
            .finish_non_exhaustive()
    }
}

impl PartialEq for SnippetJob {
    fn eq(&self, other: &Self) -> bool {
        (self.lang, &self.path, self.show_secrets) == (other.lang, &other.path, other.show_secrets)
    }
}

impl Eq for SnippetJob {}

/// The focused request, or why it has no snippet (as a toast).
fn focused_request(state: &State) -> Result<Request, (Severity, String)> {
    let Some(tab) = state.tabs.active() else {
        return Err((Severity::Info, "Open a request to copy it as code".into()));
    };
    let request = tab
        .editor
        .request()
        .map_err(|e| (Severity::Error, format!("Cannot copy: {e}")))?;
    match &request {
        Request::Opaque(opaque) if opaque.kind != "graphql" => Err((
            Severity::Warn,
            format!("No snippet export for kind '{}'", opaque.kind),
        )),
        Request::Mcp(_) => Err((
            Severity::Warn,
            "No snippet export for kind 'mcp'".to_owned(),
        )),
        _ => Ok(request),
    }
}

/// `request.copy_as` (`Alt+C`): open the picker.
pub fn open(state: &mut State) {
    if let Err((severity, message)) = focused_request(state) {
        state.notify(severity, message);
        return;
    }
    let picker = Picker::new(&state.recent, state.status.secrets_visible);
    state.overlay = Some(Overlay::CopyAs(Box::new(picker)));
}

/// The palette entry "Copy as <language>": copy at once, or, while
/// secrets are revealed, open the picker on that language to confirm.
pub fn copy(state: &mut State, lang: Lang) -> Vec<Effect> {
    if !state.status.secrets_visible {
        return start(state, lang);
    }
    open(state);
    if let Some(Overlay::CopyAs(picker)) = &mut state.overlay {
        picker.set_query(lang.id(), &state.recent);
    }
    Vec::new()
}

/// Keys while the picker is open.
pub fn picker_key(state: &mut State, chord: Chord) -> Vec<Effect> {
    let Some(Overlay::CopyAs(picker)) = &mut state.overlay else {
        return Vec::new();
    };
    match (chord.code, chord.ctrl) {
        (Code::Esc, _) => state.overlay = None,
        (Code::Enter, _) => return confirm(state),
        (Code::Up, _) | (Code::Char('p' | 'k'), true) => picker.move_by(-1),
        (Code::Down, _) | (Code::Char('n' | 'j'), true) => picker.move_by(1),
        (Code::Backspace, _) => {
            let mut query = picker.query.clone();
            query.pop();
            picker.set_query(&query, &state.recent);
        }
        _ => {
            if let Some(c) = chord.typed_char() {
                let query = format!("{}{c}", picker.query);
                picker.set_query(&query, &state.recent);
            }
        }
    }
    Vec::new()
}

/// `Enter` in the picker: copy, or arm the copy while secrets are revealed.
fn confirm(state: &mut State) -> Vec<Effect> {
    let Some(Overlay::CopyAs(picker)) = &mut state.overlay else {
        return Vec::new();
    };
    let Some(lang) = picker.selected() else {
        return Vec::new();
    };
    if picker.revealed && !picker.armed {
        picker.armed = true;
        return Vec::new();
    }
    state.overlay = None;
    palette::touch(&mut state.recent, Target::Snippet(lang));
    start(state, lang)
}

/// Make the `lang` snippet of the focused request off the loop.
fn start(state: &mut State, lang: Lang) -> Vec<Effect> {
    let request = match focused_request(state) {
        Ok(request) => request,
        Err((severity, message)) => {
            state.notify(severity, message);
            return Vec::new();
        }
    };
    let job = SnippetJob {
        lang,
        request,
        path: state.tabs.active().and_then(|t| t.path.clone()),
        workspace: workbench::workspace(state).cloned().map(Box::new),
        environment: state.status.environment.clone(),
        show_secrets: state.status.secrets_visible,
        backend: state.secret_backend,
    };
    vec![Effect::Snippet(Box::new(job))]
}

/// Make the snippet (the effect side). Blocks: secrets are read.
pub fn execute(job: &SnippetJob) -> Action {
    Action::SnippetMade {
        lang: job.lang,
        result: make(job),
    }
}

fn make(job: &SnippetJob) -> Result<Snippet, String> {
    let (_, file) = send::names(job.path.as_deref());
    let loose = RequestEntry {
        path: PathBuf::from(file),
        request: job.request.clone(),
    };
    let Some(ws) = &job.workspace else {
        let root = std::env::current_dir().map_err(|e| format!("no current directory: {e}"))?;
        let settings = Settings::ad_hoc();
        let target = RequestRef {
            root: &root,
            http: &settings.http,
            entry: &loose,
            ancestors: Vec::new(),
        };
        return render(target, &settings, None, job);
    };
    let mut ws = ws.as_ref().clone();
    let path = job
        .path
        .clone()
        .filter(|p| send::replace(&mut ws.tree, p, &job.request));
    let env = ws
        .environments
        .iter()
        .find(|e| job.environment.as_deref() == Some(e.name.as_str()));
    let requests = ws.requests();
    let found = path.and_then(|p| requests.into_iter().find(|r| r.entry.path == p));
    let target = found.unwrap_or_else(|| RequestRef {
        root: &ws.root,
        http: &ws.settings.http,
        entry: &loose,
        ancestors: vec![&ws.tree],
    });
    render(target, &ws.settings, env, job)
}

/// Resolve `target` (a GraphQL request as its POST) and render it.
fn render(
    target: RequestRef<'_>,
    settings: &Settings,
    environment: Option<&Environment>,
    job: &SnippetJob,
) -> Result<Snippet, String> {
    let store = send::secret_store(target.root, job.backend)?;
    let how = RequestSnippet {
        settings,
        environment,
        secrets: &store,
        lang: job.lang,
        show_secrets: job.show_secrets,
        placeholders: false,
    };
    request_snippet(target, &how).map_err(|e| e.to_string())
}

/// The snippet was made: copy it through OSC 52, or show it.
pub fn on_made(state: &mut State, lang: Lang, result: Result<Snippet, String>) {
    let snippet = match result {
        Ok(snippet) => snippet,
        Err(e) => {
            state.notify(Severity::Error, format!("Cannot copy as {lang}: {e}"));
            return;
        }
    };
    for warning in snippet.warnings {
        state.notify(Severity::Warn, warning);
    }
    if state.osc52 && clipboard::osc52(&snippet.code).len() <= OSC52_LIMIT {
        state.clipboard = Some(snippet.code);
        state.notify(Severity::Success, format!("Copied {lang} snippet"));
        return;
    }
    let pane = SnippetPane {
        lang,
        code: snippet.code,
        scroll: 0,
    };
    state.overlay = Some(Overlay::Snippet(Box::new(pane)));
}

/// Keys while the read-only snippet pane is open.
pub fn pane_key(state: &mut State, chord: Chord) {
    let Some(Overlay::Snippet(pane)) = &mut state.overlay else {
        return;
    };
    match chord.code {
        Code::Esc | Code::Enter | Code::Char('q') => state.overlay = None,
        Code::Up | Code::Char('k') => pane.scroll = pane.scroll.saturating_sub(1),
        Code::Down | Code::Char('j') => pane.scroll = pane.scroll.saturating_add(1),
        Code::PageUp => pane.scroll = pane.scroll.saturating_sub(10),
        Code::PageDown => pane.scroll = pane.scroll.saturating_add(10),
        _ => {}
    }
}
