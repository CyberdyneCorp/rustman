//! The Monitors view: a test-backend snapshot, and "run now" as an effect
//! that never blocks the update loop.

use std::fs;
use std::path::Path;

use rustman_core::{SecretBackend, Workspace};
use rustman_monitor::status::{FiringRule, MonitorStatus, Uptime};
use rustman_monitor::store::{Counts, LastRun};
use rustman_monitor::{Outcome, RunResult, Store, Timestamp};
use rustman_test_support::TestServer;

use crate::app::{Action, Effect, Overlay, Screen, Session, State, update};
use crate::config::{Overrides, TuiConfig, UiOptions};
use crate::effects::execute;
use crate::keymap::actions::ActionId;
use crate::keymap::chord::parse_chord;
use crate::monitors::{MonitorJob, MonitorsView, NO_TREND_VIEW, Row};
use crate::snapshot::{buffer_text, render};
use crate::theme::ColorMode;

fn at(text: &str) -> Timestamp {
    text.parse().unwrap()
}

fn result(at_text: &str, outcome: Outcome, ms: f64) -> RunResult {
    RunResult {
        at: at(at_text),
        outcome,
        duration_ms: ms,
        summary: Counts::default(),
        requests: Vec::new(),
        failures: Vec::new(),
        message: None,
    }
}

fn row(name: &str, outcome: Outcome, uptime: f64, firing: bool) -> Row {
    let hourly: Vec<Option<f64>> = (0..24).map(|i| Some(f64::from(100 + i * 10))).collect();
    let status = MonitorStatus {
        name: name.into(),
        environment: "prod".into(),
        schedule: "every 5m".into(),
        last: Some(LastRun {
            at: at("2026-03-01T10:00:00Z"),
            outcome,
            duration_ms: 120.0,
        }),
        uptime: Uptime {
            day: Some(uptime),
            week: Some(uptime),
            month: Some(uptime),
        },
        p95_24h_ms: Some(330.0),
        sparkline: rustman_monitor::status::sparkline(&hourly),
        hourly_p95_ms: hourly,
        firing: if firing {
            vec![FiringRule {
                rule: "consecutive_failures: 2".into(),
                since: at("2026-03-01T09:50:00Z"),
            }]
        } else {
            Vec::new()
        },
        coverage_24h: Some(100.0),
    };
    Row {
        status,
        recent: vec![
            result("2026-03-01T10:00:00Z", outcome, 120.0),
            result("2026-03-01T09:55:00Z", Outcome::Fail, 95.0),
        ],
    }
}

fn state() -> State {
    let ui = UiOptions::resolve(
        &TuiConfig::default(),
        Overrides::default(),
        ColorMode::NoColor,
    );
    let mut state = State::with_screen(Screen::Shell(Session::Scratch)).with_ui(ui);
    update(
        &mut state,
        Action::Resize {
            width: 90,
            height: 26,
        },
    );
    state
}

fn screen(state: &mut State) -> String {
    while state.notifications.dismiss_newest() {}
    buffer_text(&render(state, state.area.width, state.area.height))
}

fn press(state: &mut State, key: &str) -> Vec<Effect> {
    crate::commands::shell_key(state, parse_chord(key).unwrap())
}

#[test]
fn the_view_lists_status_uptime_sparkline_and_firing_rules() {
    let mut state = state();
    state.overlay = Some(Overlay::Monitors(Box::new(MonitorsView {
        rows: vec![
            row("api-health", Outcome::Fail, 98.95, true),
            row("checkout", Outcome::Pass, 100.0, false),
        ],
        at: 0,
        running: None,
        error: None,
    })));
    insta::assert_snapshot!("monitors_view", screen(&mut state));
    press(&mut state, "j");
    let Some(Overlay::Monitors(view)) = &state.overlay else {
        panic!("closed");
    };
    assert_eq!(view.at, 1);
    press(&mut state, "Esc");
    assert!(state.overlay.is_none());
}

fn workspace(base_url: &str) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let files = [
        (
            "rustman.toml",
            "[workspace]\nname = \"t\"\nversion = 1\n".to_owned(),
        ),
        (
            "environments/prod.yaml",
            format!("variables:\n  base_url: {base_url}\n"),
        ),
        (".gitignore", ".rustman/\n".to_owned()),
        (
            "monitors/api-health.rman-monitor.yaml",
            "version: 1\nname: api-health\ntargets: [health/]\nenvironment: prod\nschedule: {every: 5m}\n".to_owned(),
        ),
        (
            "health/status.rman.yaml",
            "version: 1\nkind: http\nname: status\nurl: '{{base_url}}/status/200'\nassertions:\n  - status: 200\n".to_owned(),
        ),
    ];
    for (path, text) in files {
        let path = dir.path().join(path);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, text).unwrap();
    }
    dir
}

fn stored(root: &Path) -> Vec<RunResult> {
    Store::new(root)
        .raw_since("api-health", Timestamp::MIN)
        .unwrap()
}

/// monitors "Run now from the TUI": `r` only queues an effect (nothing has
/// run when update returns), the row shows `running`, a second `r` waits,
/// and the list updates when the run's completion arrives.
#[tokio::test(flavor = "multi_thread")]
async fn run_now_runs_in_the_background_and_updates_the_list() {
    let server = TestServer::http().await;
    let ws = workspace(&server.url(""));
    let loaded = Workspace::load_with(ws.path(), &crate::files::registry()).unwrap();
    let mut state = state();
    state.screen = Screen::Shell(Session::Workspace(Box::new(loaded)));
    state.secret_backend = Some(SecretBackend::File);

    let effects = crate::commands::run(&mut state, ActionId::MonitorsOpen);
    assert!(matches!(state.overlay, Some(Overlay::Monitors(_))));
    let [Effect::Monitors(load)] = effects.as_slice() else {
        panic!("{effects:?}");
    };
    assert!(matches!(**load, MonitorJob::Load { .. }));
    let load = Effect::Monitors(load.clone());
    let done = tokio::task::spawn_blocking(move || execute(load));
    update(&mut state, done.await.unwrap());

    let effects = press(&mut state, "r");
    assert_eq!(effects.len(), 1, "{effects:?}");
    assert!(stored(ws.path()).is_empty(), "nothing ran inside update");
    assert!(screen(&mut state).contains("running"));
    assert!(press(&mut state, "r").is_empty(), "one run at a time");

    let effect = effects.into_iter().next().unwrap();
    let done = tokio::task::spawn_blocking(move || execute(effect));
    update(&mut state, done.await.unwrap());
    let Some(Overlay::Monitors(view)) = &state.overlay else {
        panic!("closed");
    };
    assert_eq!(view.running, None);
    let last = view.rows[0].status.last.as_ref().unwrap();
    assert_eq!(last.outcome, Outcome::Pass);
    assert_eq!(view.rows[0].recent.len(), 1);
    assert_eq!(stored(ws.path()).len(), 1);
}

fn toasts(state: &State) -> Vec<String> {
    state
        .notifications
        .log()
        .map(|n| n.message.clone())
        .collect()
}

/// A workspace with one stored run, the Monitors view open, and `v`
/// pressed with `discovery`: the trend job, run as the effect runner does.
fn trend_with(discovery: crate::companion::Discovery) -> (tempfile::TempDir, State) {
    let ws = workspace("http://127.0.0.1:1");
    Store::new(ws.path())
        .record(
            "api-health",
            &RunResult {
                at: Timestamp::now(),
                ..result("2026-03-01T10:00:00Z", Outcome::Pass, 120.0)
            },
        )
        .unwrap();
    let loaded = Workspace::load_with(ws.path(), &crate::files::registry()).unwrap();
    let mut state = state();
    state.screen = Screen::Shell(Session::Workspace(Box::new(loaded)));
    state.viz = discovery;
    crate::commands::run(&mut state, ActionId::MonitorsOpen);
    let effects = press(&mut state, "v");
    let [Effect::Monitors(job)] = effects.as_slice() else {
        panic!("v queues one effect: {effects:?}");
    };
    assert!(matches!(**job, MonitorJob::Trend { .. }), "{job:?}");
    assert!(toasts(&state).is_empty(), "nothing ran inside update");
    let done = execute(Effect::Monitors(job.clone()));
    update(&mut state, done);
    (ws, state)
}

/// monitors "Companion absent" from the TUI: `v` says why, and the text
/// view stays open.
#[test]
fn v_without_a_companion_keeps_the_text_view() {
    let (_ws, state) = trend_with(crate::companion::Discovery::default());
    let toasts = toasts(&state);
    assert!(
        toasts
            .iter()
            .any(|t| t.starts_with(NO_TREND_VIEW) && t.contains("not found")),
        "{toasts:?}"
    );
    assert!(matches!(state.overlay, Some(Overlay::Monitors(_))));
}

/// `v` with a companion that has the view: it opens `monitor_trend` and
/// gets one `monitor_trend` per monitor, then `close`.
#[test]
fn v_sends_the_trends_to_the_companion() {
    let dir = tempfile::tempdir().unwrap();
    let ack = r#"{"type":"hello_ack","protocol":"rustman-viz/1","viewer":"stub-viz 0.1","views":["monitor_trend"]}"#;
    let script = crate::viz_tests::stub(dir.path(), ack, 0);
    let discovery = crate::companion::Discovery {
        override_path: Some(script.clone()),
        ..crate::companion::Discovery::default()
    };
    let (_ws, state) = trend_with(discovery);
    let toasts = toasts(&state);
    assert!(
        toasts
            .iter()
            .any(|t| t == "Monitor trends opened in stub-viz 0.1"),
        "{toasts:?}"
    );
    if cfg!(unix) {
        let log = crate::viz_tests::log_of(&script, "\"close\"");
        assert!(log.contains(r#""view":"monitor_trend""#), "{log}");
        assert!(
            log.contains(r#""type":"monitor_trend","monitor":"api-health""#),
            "{log}"
        );
        assert!(log.contains(r#""uptime_24h":100.0"#), "{log}");
    }
}

/// A companion without the view: the trends are not sent, and `v` says
/// the companion has no `monitor_trend` view.
#[test]
fn v_with_a_companion_lacking_the_view_falls_back() {
    let dir = tempfile::tempdir().unwrap();
    let ack = r#"{"type":"hello_ack","protocol":"rustman-viz/1","viewer":"stub-viz 0.1","views":["run_dashboard"]}"#;
    let script = crate::viz_tests::stub(dir.path(), ack, 0);
    let discovery = crate::companion::Discovery {
        override_path: Some(script),
        ..crate::companion::Discovery::default()
    };
    let (_ws, state) = trend_with(discovery);
    let toasts = toasts(&state);
    let want = format!("{NO_TREND_VIEW} ({})", crate::monitor_viz::NO_TREND_VIEW);
    assert!(toasts.contains(&want), "{toasts:?}");
}
