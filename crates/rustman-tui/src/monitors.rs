//! The Monitors view (monitors "TUI monitors view"), opened from the
//! command palette: each monitor's last outcome, 24 h uptime, latency
//! sparkline and firing rules; the selected monitor's recent results; and
//! `r` to run it now.
//!
//! Loading and running are effects ([`MonitorJob`] on the effect runner),
//! so the UI never waits: a run goes through the same cycle as `rustman
//! monitor run` (its policy, store, alerts and notifications) and the list
//! is reloaded when it finishes. `v` opens the trend view in the viz
//! companion ([`crate::monitor_viz`]), also as an effect; without one it
//! says why and the text status stays on screen.

use std::path::{Path, PathBuf};

use ratatui::Frame;
use ratatui::style::Modifier;
use ratatui::text::{Line, Span};
use rustman_core::{SecretBackend, Workspace};
use rustman_monitor::cycle::CycleContext;
use rustman_monitor::notify::{self, SystemDesktop};
use rustman_monitor::status::trend::Trend;
use rustman_monitor::status::{self, MonitorStatus};
use rustman_monitor::{Outcome, RunResult, Store, Timestamp};
use tokio_util::sync::CancellationToken;

use crate::app::{Action, Effect, Overlay, Screen, Session, State};
use crate::companion::Discovery;
use crate::config::UiOptions;
use crate::keymap::chord::{Chord, Code};
use crate::monitor_viz;
use crate::theme::Token;
use crate::toast::Severity;
use crate::view::{dialog, hint};

/// What `v` says when the companion cannot show the trends, before why.
pub const NO_TREND_VIEW: &str =
    "The viz companion is unavailable for monitor trends; the text status is shown here";
/// Recent results shown for the selected monitor.
const RECENT: usize = 5;

/// The open view.
#[derive(Debug, Clone, PartialEq)]
pub struct MonitorsView {
    pub rows: Vec<Row>,
    pub at: usize,
    /// The monitor running now, if any.
    pub running: Option<String>,
    /// Why the monitors could not be read.
    pub error: Option<String>,
}

// The view is compared whole in tests; its latencies are never NaN.
impl Eq for MonitorsView {}

/// One monitor.
#[derive(Debug, Clone, PartialEq)]
pub struct Row {
    pub status: MonitorStatus,
    /// The latest results, newest first.
    pub recent: Vec<RunResult>,
}

/// Work for the effect runner.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MonitorJob {
    /// Read the monitors and their store.
    Load { root: PathBuf },
    /// Run monitor `name` once, then read everything again.
    Run {
        root: PathBuf,
        name: String,
        backend: Option<SecretBackend>,
    },
    /// Open every monitor's trend in the viz companion.
    Trend { root: PathBuf, discovery: Discovery },
}

/// What a job found.
#[derive(Debug, Clone, PartialEq)]
pub struct MonitorsDone {
    pub rows: Result<Vec<Row>, String>,
    /// For a run: its one-line summary, or why it could not run.
    pub ran: Option<Result<String, String>>,
    /// For trends: where they opened, or why the text status stays.
    pub note: Option<String>,
}

/// Open the view over the workspace's monitors.
pub fn open(state: &mut State) -> Vec<Effect> {
    let Some(root) = root(state) else {
        state.notify(Severity::Info, "Monitors need a workspace");
        return Vec::new();
    };
    state.overlay = Some(Overlay::Monitors(Box::new(MonitorsView {
        rows: Vec::new(),
        at: 0,
        running: None,
        error: None,
    })));
    vec![Effect::Monitors(Box::new(MonitorJob::Load { root }))]
}

fn root(state: &State) -> Option<PathBuf> {
    match &state.screen {
        Screen::Shell(Session::Workspace(ws)) => Some(ws.root.clone()),
        _ => None,
    }
}

/// Keys while the view is open.
pub fn key(state: &mut State, chord: Chord) -> Vec<Effect> {
    let root = root(state);
    let backend = state.secret_backend;
    let Some(Overlay::Monitors(view)) = &mut state.overlay else {
        return Vec::new();
    };
    match chord.code {
        Code::Esc | Code::Char('q') => state.overlay = None,
        Code::Down | Code::Char('j') => {
            view.at = (view.at + 1).min(view.rows.len().saturating_sub(1))
        }
        Code::Up | Code::Char('k') => view.at = view.at.saturating_sub(1),
        Code::Char('v') => {
            let Some(root) = root else {
                return Vec::new();
            };
            let discovery = state.viz.clone();
            return vec![Effect::Monitors(Box::new(MonitorJob::Trend {
                root,
                discovery,
            }))];
        }
        Code::Char('r') => return run_now(view, root, backend),
        _ => {}
    }
    Vec::new()
}

/// Start a background run of the selected monitor; one at a time.
fn run_now(
    view: &mut MonitorsView,
    root: Option<PathBuf>,
    backend: Option<SecretBackend>,
) -> Vec<Effect> {
    let (Some(root), Some(row), None) = (root, view.rows.get(view.at), &view.running) else {
        return Vec::new();
    };
    let name = row.status.name.clone();
    view.running = Some(name.clone());
    vec![Effect::Monitors(Box::new(MonitorJob::Run {
        root,
        name,
        backend,
    }))]
}

/// A job finished: show what it found.
pub fn on_done(state: &mut State, done: MonitorsDone) {
    match &done.ran {
        Some(Ok(summary)) => state.notify(Severity::Info, summary.clone()),
        Some(Err(error)) => state.notify(Severity::Error, error.clone()),
        None => {}
    }
    if let Some(note) = &done.note {
        state.notify(Severity::Info, note.clone());
    }
    let Some(Overlay::Monitors(view)) = &mut state.overlay else {
        return;
    };
    if done.ran.is_some() {
        view.running = None;
    }
    match done.rows {
        Ok(rows) => {
            view.at = view.at.min(rows.len().saturating_sub(1));
            view.rows = rows;
            view.error = None;
        }
        Err(error) => view.error = Some(error),
    }
}

/// Run `job` (on the effect runner's blocking thread).
pub fn execute(job: &MonitorJob) -> Action {
    let done = match job {
        MonitorJob::Load { root } => MonitorsDone {
            rows: load(root),
            ran: None,
            note: None,
        },
        MonitorJob::Trend { root, discovery } => MonitorsDone {
            rows: load(root),
            ran: None,
            note: Some(trend(root, discovery)),
        },
        MonitorJob::Run {
            root,
            name,
            backend,
        } => {
            let ran = run(root, name, *backend);
            MonitorsDone {
                rows: load(root),
                ran: Some(ran),
                note: None,
            }
        }
    };
    Action::MonitorsDone(Box::new(done))
}

/// Every monitor with its status and latest results.
pub fn load(root: &Path) -> Result<Vec<Row>, String> {
    let monitors = rustman_monitor::load_all(root).map_err(|e| e.to_string())?;
    let store = Store::new(root);
    let now = Timestamp::now();
    monitors
        .iter()
        .map(|monitor| {
            let status = status::status(&store, monitor, now).map_err(|e| e.to_string())?;
            let recent = status::history(&store, &monitor.name, None, None, now)
                .map_err(|e| e.to_string())?
                .into_iter()
                .filter_map(|entry| match entry {
                    status::HistoryEntry::Run(run) => Some(run),
                    status::HistoryEntry::Hour(_) => None,
                })
                .take(RECENT)
                .collect();
            Ok(Row { status, recent })
        })
        .collect()
}

/// Send every monitor's trend to the companion: where they opened, or
/// [`NO_TREND_VIEW`] and why.
fn trend(root: &Path, discovery: &Discovery) -> String {
    let shown = trends(root).and_then(|trends| monitor_viz::show(discovery, &trends));
    match shown {
        Ok(viewer) => format!("Monitor trends opened in {viewer}"),
        Err(why) => format!("{NO_TREND_VIEW} ({why})"),
    }
}

fn trends(root: &Path) -> Result<Vec<Trend>, String> {
    let monitors = rustman_monitor::load_all(root).map_err(|e| e.to_string())?;
    let store = Store::new(root);
    let now = Timestamp::now();
    monitors
        .iter()
        .map(|monitor| {
            let status = status::status(&store, monitor, now)?;
            status::trend::trend(&store, &status, now)
        })
        .collect::<Result<_, _>>()
        .map_err(|e| e.to_string())
}

/// One cycle of monitor `name`, as `rustman monitor run` does it.
fn run(root: &Path, name: &str, backend: Option<SecretBackend>) -> Result<String, String> {
    let ws = Workspace::load_with(root, &crate::files::registry()).map_err(|e| e.to_string())?;
    let monitors = rustman_monitor::load_all(root).map_err(|e| e.to_string())?;
    let monitor = rustman_monitor::find(&monitors, name).map_err(|e| e.to_string())?;
    let secrets = crate::send::secret_store(root, backend)?;
    let retention = rustman_monitor::retention(&ws.settings.monitors).map_err(|e| e.to_string())?;
    let engine = rustman_http::HttpEngine::new();
    let ctx = CycleContext {
        ws: &ws,
        secrets: &secrets,
        desktop: &SystemDesktop,
        engine: &engine,
        retention,
        notify_timeout: notify::TIMEOUT,
    };
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| e.to_string())?;
    let cycle = runtime
        .block_on(ctx.run(monitor, CancellationToken::new()))
        .map_err(|e| format!("{name}: {e}"))?;
    let result = &cycle.run.result;
    Ok(format!(
        "{name}: {} in {:.0} ms",
        result.outcome, result.duration_ms
    ))
}

pub fn render(frame: &mut Frame, view: &MonitorsView, ui: &UiOptions) {
    let mut lines = Vec::new();
    if let Some(error) = &view.error {
        lines.push(Line::from(Span::styled(
            error.clone(),
            ui.theme.fg(Token::Error),
        )));
    } else if view.rows.is_empty() {
        lines.push(hint(
            "No monitors (add monitors/<name>.rman-monitor.yaml)",
            ui,
        ));
    }
    for (i, row) in view.rows.iter().enumerate() {
        lines.push(row_line(row, i == view.at, view.running.as_deref(), ui));
    }
    if let Some(row) = view.rows.get(view.at) {
        lines.push(Line::default());
        lines.extend(detail_lines(row, ui));
    }
    lines.push(Line::default());
    lines.push(hint("j/k select · r run now · v trends · Esc close", ui));
    dialog(frame, frame.area(), "Monitors", lines, ui);
}

/// `› api-health   fail    98.95% ▁▂▃▄ firing`.
fn row_line(row: &Row, selected: bool, running: Option<&str>, ui: &UiOptions) -> Line<'static> {
    let s = &row.status;
    let outcome = if running == Some(s.name.as_str()) {
        ("running", Token::Info)
    } else {
        s.last.as_ref().map_or(("never", Token::Muted), |l| {
            (l.outcome.as_str(), outcome_token(l.outcome))
        })
    };
    let uptime = s.uptime.day.map_or("-".to_owned(), |u| format!("{u:.2}%"));
    let firing = if s.firing.is_empty() { "" } else { " firing" };
    let marker = if selected { "› " } else { "  " };
    let mut name = ui.theme.fg(Token::Text);
    if selected {
        name = name.add_modifier(Modifier::BOLD);
    }
    Line::from(vec![
        Span::styled(format!("{marker}{:<12} ", s.name), name),
        Span::styled(format!("{:<7}", outcome.0), ui.theme.fg(outcome.1)),
        Span::styled(format!("{uptime:>7} "), ui.theme.fg(Token::Text)),
        Span::styled(s.sparkline.clone(), ui.theme.fg(Token::Accent)),
        Span::styled(firing, ui.theme.fg(Token::Error)),
    ])
}

/// The selected monitor: schedule, firing rules and recent results.
fn detail_lines(row: &Row, ui: &UiOptions) -> Vec<Line<'static>> {
    let s = &row.status;
    let mut lines = vec![hint(
        &format!("{} · {} · {}", s.name, s.environment, s.schedule),
        ui,
    )];
    for firing in &s.firing {
        lines.push(Line::from(Span::styled(
            format!("firing {} since {}", firing.rule, firing.since),
            ui.theme.fg(Token::Error),
        )));
    }
    for run in &row.recent {
        let text = format!(
            "{}  {:<7} {:>5.0} ms",
            run.at,
            run.outcome.as_str(),
            run.duration_ms
        );
        lines.push(Line::from(Span::styled(
            text,
            ui.theme.fg(outcome_token(run.outcome)),
        )));
    }
    lines
}

fn outcome_token(outcome: Outcome) -> Token {
    match outcome {
        Outcome::Pass => Token::Ok,
        Outcome::Fail | Outcome::Error => Token::Error,
        Outcome::Skipped => Token::Muted,
    }
}
