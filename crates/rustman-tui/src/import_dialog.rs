//! "Import…" (importers task 6.1): ask for a source file or folder, run
//! the import engine as a dry run and show its preview, then import that
//! previewed source on confirmation. Both runs happen off the loop; the
//! workspace is loaded again afterwards so the sidebar shows the new
//! requests.
//!
//! Each dialog has its own id, and a run's result is applied only to the
//! dialog that started it: a dry run that finishes after its dialog was
//! closed is dropped. A real import cannot be stopped once it writes, so
//! `Esc` waits for it, and its result is always reported.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use ratatui::Frame;
use ratatui::style::Modifier;
use ratatui::text::{Line, Span};
use rustman_core::SecretBackend;
use rustman_import::session::{self, Outcome, Session};

use crate::app::{Action, Effect, Overlay, State};
use crate::config::UiOptions;
use crate::keymap::chord::{Chord, Code};
use crate::textarea::TextArea;
use crate::theme::Token;
use crate::toast::Severity;
use crate::view::{dialog, hint};
use crate::{vim, workbench};

/// How many preview lines show at once.
const PREVIEW_ROWS: usize = 14;

/// The hint while the import runs.
pub const IMPORTING_HINT: &str = "Importing… (it cannot be cancelled once it writes)";

/// Where the dialog is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Stage {
    /// Typing the source path.
    Source,
    /// The dry run is running.
    Previewing,
    /// The dry run's summary, waiting for confirmation; `job` is the dry
    /// run that made it, imported as it is on `Enter`.
    Preview {
        summary: String,
        scroll: usize,
        job: Box<ImportJob>,
    },
    /// The import is running.
    Importing,
}

/// The open "Import…" dialog.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImportDialog {
    /// Tells this dialog's runs from those of a dialog closed earlier.
    pub id: u64,
    pub source: TextArea,
    pub stage: Stage,
    pub error: Option<String>,
}

/// One import run (a dry run or the real one), owned for the effect.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImportJob {
    /// The dialog that started the run.
    pub dialog: u64,
    pub root: PathBuf,
    pub src: PathBuf,
    /// The source as typed, for messages.
    pub shown: String,
    pub environment: Option<String>,
    pub backend: Option<SecretBackend>,
    pub dry_run: bool,
}

/// A finished run, as the effect reports it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImportDone {
    pub job: ImportJob,
    pub result: Result<Outcome, String>,
    /// The secret store's warnings.
    pub warnings: Vec<String>,
}

fn next_id() -> u64 {
    static NEXT: AtomicU64 = AtomicU64::new(1);
    NEXT.fetch_add(1, Ordering::Relaxed)
}

/// `import.open`: ask for the source.
pub fn open(state: &mut State) {
    if workbench::workspace(state).is_none() {
        state.notify(Severity::Info, "Open a workspace to import into it");
        return;
    }
    let dialog = ImportDialog {
        id: next_id(),
        source: TextArea::from_text(""),
        stage: Stage::Source,
        error: None,
    };
    state.overlay = Some(Overlay::Import(Box::new(dialog)));
}

/// Keys while the dialog is open.
pub fn key(state: &mut State, chord: Chord) -> Vec<Effect> {
    let Some(Overlay::Import(dialog)) = &mut state.overlay else {
        return Vec::new();
    };
    match &mut dialog.stage {
        // The import is writing: the dialog stays until it is done.
        Stage::Importing => Vec::new(),
        _ if chord.code == Code::Esc => {
            state.overlay = None;
            Vec::new()
        }
        Stage::Source if chord.code == Code::Enter => submit(state),
        Stage::Source => {
            vim::type_key(&mut dialog.source, chord);
            dialog.error = None;
            Vec::new()
        }
        Stage::Preview { job, .. } if chord.code == Code::Enter => {
            let job = ImportJob {
                dry_run: false,
                ..job.as_ref().clone()
            };
            dialog.stage = Stage::Importing;
            vec![Effect::Import(Box::new(job))]
        }
        Stage::Preview {
            summary, scroll, ..
        } => {
            let last = summary.lines().count().saturating_sub(PREVIEW_ROWS);
            match chord.code {
                Code::Up => *scroll = scroll.saturating_sub(1),
                Code::Down => *scroll = (*scroll + 1).min(last),
                _ => {}
            }
            Vec::new()
        }
        Stage::Previewing => Vec::new(),
    }
}

/// `Enter` on the source: start the dry run.
fn submit(state: &mut State) -> Vec<Effect> {
    let Some(root) = workbench::workspace(state).map(|ws| ws.root.clone()) else {
        return Vec::new();
    };
    let environment = state.status.environment.clone();
    let backend = state.secret_backend;
    let Some(Overlay::Import(dialog)) = &mut state.overlay else {
        return Vec::new();
    };
    let typed = dialog.source.text().trim().to_owned();
    if typed.is_empty() {
        dialog.error = Some("Enter a file or folder to import".into());
        return Vec::new();
    }
    dialog.stage = Stage::Previewing;
    let job = ImportJob {
        dialog: dialog.id,
        src: source_path(&root, &typed),
        root,
        shown: typed,
        environment,
        backend,
        dry_run: true,
    };
    vec![Effect::Import(Box::new(job))]
}

/// `typed` as a path: absolute as it is, relative to `root` otherwise.
pub fn source_path(root: &Path, typed: &str) -> PathBuf {
    let path = Path::new(typed);
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        root.join(path)
    }
}

/// Run the import (the effect side). Blocks: it reads and writes files.
pub fn execute(job: &ImportJob) -> Action {
    let mut warnings = Vec::new();
    let result = backend(job.backend).and_then(|backend| {
        let run = Session {
            root: &job.root,
            src: &job.src,
            shown: &job.shown,
            environment: job.environment.as_deref(),
            backend,
            dry_run: job.dry_run,
        };
        session::run(&run, &mut warnings).map_err(|e| e.to_string())
    });
    Action::Imported(Box::new(ImportDone {
        job: job.clone(),
        result,
        warnings,
    }))
}

/// `backend`, or the one `RUSTMAN_SECRET_STORE` names.
fn backend(backend: Option<SecretBackend>) -> Result<SecretBackend, String> {
    match backend {
        Some(backend) => Ok(backend),
        None => {
            let value = std::env::var(rustman_core::secrets::BACKEND_VAR).ok();
            SecretBackend::from_env_value(value.as_deref()).map_err(|e| e.to_string())
        }
    }
}

/// A run finished: show the preview, or report the import.
pub fn on_done(state: &mut State, done: ImportDone) -> Vec<Effect> {
    for warning in done.warnings {
        state.notify(Severity::Warn, warning);
    }
    let id = done.job.dialog;
    if !done.job.dry_run {
        if is_open(state, id, &Stage::Importing) {
            state.overlay = None;
        }
        return imported(state, done.result);
    }
    // A preview belongs to the dialog that asked for it, if still open.
    if !is_open(state, id, &Stage::Previewing) {
        return Vec::new();
    }
    if let Some(Overlay::Import(dialog)) = &mut state.overlay {
        dialog.stage = match done.result {
            Ok(outcome) => Stage::Preview {
                summary: outcome.summary,
                scroll: 0,
                job: Box::new(done.job),
            },
            Err(message) => {
                dialog.error = Some(message);
                Stage::Source
            }
        };
    }
    Vec::new()
}

/// Whether the dialog `id` is open at `stage`.
fn is_open(state: &State, id: u64, stage: &Stage) -> bool {
    matches!(&state.overlay, Some(Overlay::Import(d)) if d.id == id && &d.stage == stage)
}

fn imported(state: &mut State, result: Result<Outcome, String>) -> Vec<Effect> {
    let outcome = match result {
        Ok(outcome) => outcome,
        Err(message) => {
            state.notify(Severity::Error, message);
            return Vec::new();
        }
    };
    let first = outcome.summary.lines().next().unwrap_or("imported");
    state.notify(Severity::Success, first.to_owned());
    if outcome.has_losses {
        state.notify(
            Severity::Warn,
            "Some features were unsupported, dropped or unmapped: run `rustman import --dry-run` for details",
        );
    }
    workbench::workspace(state)
        .map(|ws| Effect::LoadWorkspace(ws.root.clone()))
        .into_iter()
        .collect()
}

/// Draw the dialog.
pub fn render(frame: &mut Frame, dialog_state: &ImportDialog, ui: &UiOptions) {
    let g = &ui.glyphs;
    let (title, lines) = match &dialog_state.stage {
        Stage::Source => (" Import ", source_lines(dialog_state, ui)),
        Stage::Previewing => (" Import ", vec![hint("Reading the source…", ui)]),
        Stage::Importing => (" Import ", vec![hint(IMPORTING_HINT, ui)]),
        Stage::Preview {
            summary, scroll, ..
        } => {
            let mut lines: Vec<Line<'static>> = summary
                .lines()
                .skip(*scroll)
                .take(PREVIEW_ROWS)
                .map(|l| Line::styled(l.to_owned(), ui.theme.fg(Token::Text)))
                .collect();
            let keys = format!(
                "Enter import {} {} scroll {} Esc cancel",
                g.dot, g.up_down, g.dot
            );
            lines.extend([Line::default(), hint(&keys, ui)]);
            (" Import preview ", lines)
        }
    };
    dialog(frame, frame.area(), title, lines, ui);
}

fn source_lines(dialog_state: &ImportDialog, ui: &UiOptions) -> Vec<Line<'static>> {
    let text_style = ui.theme.fg(Token::Text);
    let value = dialog_state.source.text();
    let cursor = dialog_state.source.cursor().col;
    let before: String = value.chars().take(cursor).collect();
    let at: String = value.chars().nth(cursor).map_or(" ".into(), String::from);
    let after: String = value.chars().skip(cursor + 1).collect();
    let mut lines = vec![
        Line::from("Postman, Insomnia, Bruno, HAR or .http file or folder:"),
        Line::from(vec![
            Span::styled(format!("{} {before}", ui.glyphs.pointer), text_style),
            Span::styled(at, text_style.add_modifier(Modifier::REVERSED)),
            Span::styled(after, text_style),
        ]),
    ];
    if let Some(error) = &dialog_state.error {
        lines.push(Line::styled(error.clone(), ui.theme.fg(Token::Error)));
    }
    let keys = format!("Enter preview {} Esc cancel", ui.glyphs.dot);
    lines.extend([Line::default(), hint(&keys, ui)]);
    lines
}
