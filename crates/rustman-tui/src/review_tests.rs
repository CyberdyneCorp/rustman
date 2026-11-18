//! Snapshot review in the TUI (snapshot-testing tasks 6.1 and 6.2): the
//! Snapshot tab with `a` approve and `r` reject against real sends and
//! snapshot files, CI mode, the `Alt+A` queue over three failures, and the
//! secret canary for both new surfaces.

use std::fs;
use std::path::{Path, PathBuf};

use rustman_core::assertions::Outcome;
use rustman_core::model::SnapshotAssertion;
use rustman_core::snapshot::{
    Exchange, SnapshotContext, SnapshotMode, SnapshotReview, SnapshotState, check,
};
use rustman_core::{Redactor, Secret, SecretBackend, SecretStore};
use rustman_test_support::TestServer;

use crate::app::{Action, Effect, Overlay, Screen, Session, State, update};
use crate::config::{Overrides, TuiConfig, UiOptions};
use crate::effects::execute;
use crate::focus::Focus;
use crate::keymap::chord::parse_chord;
use crate::response::ResponseTab;
use crate::response::received::fixtures::received;
use crate::review::{CI_DISABLED, Decision, Reviews};
use crate::snapshot::{buffer_text, render};
use crate::theme::ColorMode;

/// A secret value that must never be drawn.
const CANARY: &str = "canary-snap-77a1";

/// A workspace whose `<name>.rman.yaml` requests snapshot `/users/42`, and
/// `echo.rman.yaml` snapshots `/echo` with the secret `token` in a header.
fn workspace(server: &TestServer, names: &[&str]) -> tempfile::TempDir {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("ws");
    let write = |rel: &str, text: String| {
        let path = root.join(rel);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, text).unwrap();
    };
    write(
        "rustman.toml",
        "[workspace]\nname = \"demo\"\nversion = 1\n\n[defaults]\ndefault_environment = \"local\"\n"
            .into(),
    );
    write(
        "environments/local.yaml",
        format!(
            "variables:\n  base_url: {}\nsecrets: [token]\n",
            server.url("")
        ),
    );
    for name in names {
        write(
            &format!("{name}.rman.yaml"),
            format!(
                "version: 1\nkind: http\nname: {name}\nmethod: GET\n\
                 url: '{{{{base_url}}}}/users/42'\nassertions:\n  - snapshot\n"
            ),
        );
    }
    write(
        "echo.rman.yaml",
        "version: 1\nkind: http\nname: Echo\nmethod: GET\nurl: '{{base_url}}/echo'\n\
         headers:\n  - name: X-Token\n    value: '{{token}}'\nassertions:\n  - snapshot\n"
            .into(),
    );
    SecretStore::open(&root, SecretBackend::File)
        .set("local", &Secret::new("token", CANARY.to_owned()))
        .unwrap();
    tmp
}

/// The workspace loaded into a 200 x 50 shell with the file secret store.
fn shell(tmp: &tempfile::TempDir) -> State {
    let root = tmp.path().join("ws");
    let config = tmp.path().join("config");
    fs::create_dir_all(&config).unwrap();
    let (mut state, mut effects) = crate::initial_state_with(
        &root,
        Overrides::default(),
        Some(&config),
        ColorMode::NoColor,
    );
    while let Some(effect) = effects.pop() {
        effects.extend(update(&mut state, execute(effect)));
    }
    state.secret_backend = Some(SecretBackend::File);
    update(
        &mut state,
        Action::Resize {
            width: 200,
            height: 50,
        },
    );
    state
}

fn open(state: &mut State, request: &str) {
    crate::workbench::open_path(state, Path::new(request));
    state.shell.focus = Focus::EditorBody;
}

/// Send the active tab's request and apply the result.
async fn send(state: &mut State) {
    let effects = crate::send::start(state);
    let [Effect::Send(job)] = effects.as_slice() else {
        panic!("expected a send, got {effects:?}");
    };
    let job = job.clone();
    let done = tokio::task::spawn_blocking(move || crate::send::execute(&job))
        .await
        .unwrap();
    update(state, done);
}

/// Type `keys` (space separated) and run the effects they return.
fn press(state: &mut State, keys: &str) {
    for key in keys.split(' ') {
        let chord = parse_chord(key).unwrap();
        let effects = crate::commands::shell_key(state, chord);
        run(state, effects);
    }
}

fn run(state: &mut State, mut effects: Vec<Effect>) {
    while let Some(effect) = effects.pop() {
        effects.extend(update(state, execute(effect)));
    }
}

fn snapshot_file(tmp: &tempfile::TempDir, stem: &str) -> PathBuf {
    tmp.path()
        .join("ws/__snapshots__")
        .join(format!("{stem}.default.snap.yaml"))
}

/// Change the stored snapshot so the next send fails: `Ana` → `Bob`.
fn tamper(path: &Path) -> String {
    let text = fs::read_to_string(path).unwrap().replace("Ana", "Bob");
    fs::write(path, &text).unwrap();
    text
}

/// The screen without toasts, which would cover the response pane.
fn screen(state: &mut State) -> String {
    while state.notifications.dismiss_newest() {}
    buffer_text(&render(state, state.area.width, state.area.height))
}

fn reviews(state: &State) -> &Reviews {
    &state
        .tabs
        .active()
        .unwrap()
        .exchange
        .as_ref()
        .unwrap()
        .snapshots
}

/// Show the Snapshot tab of the focused response.
fn snapshot_tab(state: &mut State) {
    state.shell.focus = Focus::Response;
    let tab = state.tabs.active_mut().unwrap();
    assert!(tab.viewer.tabs().contains(&ResponseTab::Snapshot));
    tab.viewer.set_tab(ResponseTab::Snapshot);
}

#[tokio::test(flavor = "multi_thread")]
async fn approve_rewrites_the_file_and_reject_keeps_it() {
    // Scenarios "Approve from TUI" and "Reject keeps file".
    let server = TestServer::http().await;
    let tmp = workspace(&server, &["user"]);
    let mut state = shell(&tmp);
    open(&mut state, "user.rman.yaml");
    send(&mut state).await;
    let file = snapshot_file(&tmp, "user");
    let recorded = fs::read_to_string(&file).unwrap();
    assert_eq!(
        reviews(&state).items[0].review.state,
        SnapshotState::Recorded
    );

    let stored = tamper(&file);
    send(&mut state).await;
    snapshot_tab(&mut state);
    let shown = screen(&mut state);
    assert!(shown.contains("snapshot default failed"), "{shown}");
    assert!(
        shown.contains("changed $.name: \"Bob\" → \"Ana\""),
        "{shown}"
    );
    assert!(shown.contains("a approve · r reject"), "{shown}");

    press(&mut state, "r");
    assert_eq!(
        fs::read_to_string(&file).unwrap(),
        stored,
        "reject keeps the file"
    );
    assert_eq!(reviews(&state).items[0].decision, Decision::Rejected);
    let record = &state
        .tabs
        .active()
        .unwrap()
        .exchange
        .as_ref()
        .unwrap()
        .record;
    assert_eq!(record.assertions[0].outcome, Outcome::Fail, "still failed");
    assert!(screen(&mut state).contains("rejected"));

    send(&mut state).await;
    snapshot_tab(&mut state);
    press(&mut state, "a");
    assert_eq!(
        fs::read_to_string(&file).unwrap(),
        recorded,
        "approve rewrites"
    );
    assert_eq!(reviews(&state).items[0].decision, Decision::Approved);
    let shown = screen(&mut state);
    assert!(shown.contains("· approved"), "{shown}");

    // A further `a` has nothing left to approve, and the file matches now.
    press(&mut state, "a");
    send(&mut state).await;
    assert!(
        reviews(&state).is_empty(),
        "a matching snapshot needs no review"
    );
    let tab = state.tabs.active().unwrap();
    assert!(!tab.viewer.tabs().contains(&ResponseTab::Snapshot));
}

#[tokio::test(flavor = "multi_thread")]
async fn ci_mode_disables_approval() {
    let server = TestServer::http().await;
    let tmp = workspace(&server, &["user"]);
    let mut state = shell(&tmp);
    open(&mut state, "user.rman.yaml");
    send(&mut state).await;
    let file = snapshot_file(&tmp, "user");
    let stored = tamper(&file);

    state.snapshot_mode = SnapshotMode::Ci;
    send(&mut state).await;
    snapshot_tab(&mut state);
    assert!(reviews(&state).ci);
    assert!(screen(&mut state).contains("Approval is disabled in CI mode."));
    press(&mut state, "a");
    assert_eq!(
        fs::read_to_string(&file).unwrap(),
        stored,
        "CI mode never writes"
    );
    assert_eq!(reviews(&state).items[0].decision, Decision::Open);
    let toasts: Vec<_> = state
        .notifications
        .log()
        .map(|t| t.message.clone())
        .collect();
    assert!(toasts.iter().any(|m| m == CI_DISABLED), "{toasts:?}");

    // In CI mode a missing snapshot is not recorded either.
    fs::remove_file(&file).unwrap();
    send(&mut state).await;
    assert_eq!(
        reviews(&state).items[0].review.state,
        SnapshotState::Missing
    );
    assert!(!file.exists());
}

#[tokio::test(flavor = "multi_thread")]
async fn the_review_queue_walks_three_failures() {
    let server = TestServer::http().await;
    let names = ["one", "two", "three"];
    let tmp = workspace(&server, &names);
    let mut state = shell(&tmp);
    for name in names {
        open(&mut state, &format!("{name}.rman.yaml"));
        send(&mut state).await;
    }
    let recorded: Vec<String> = names
        .iter()
        .map(|n| fs::read_to_string(snapshot_file(&tmp, n)).unwrap())
        .collect();
    press(&mut state, "Alt+A");
    assert!(state.overlay.is_none(), "nothing failed yet: no queue");

    let stored: Vec<String> = names
        .iter()
        .map(|n| tamper(&snapshot_file(&tmp, n)))
        .collect();
    for index in 0..names.len() {
        state.tabs.select(index);
        send(&mut state).await;
    }
    state.shell.focus = Focus::EditorBody;
    press(&mut state, "Alt+A");
    let shown = screen(&mut state);
    assert!(shown.contains("Snapshot review 1 of 3"), "{shown}");
    assert!(
        shown.contains("__snapshots__/one.default.snap.yaml"),
        "{shown}"
    );
    assert!(shown.contains("changed $.name"), "{shown}");

    // `n` and `p` step without deciding.
    press(&mut state, "n");
    assert!(screen(&mut state).contains("Snapshot review 2 of 3"));
    press(&mut state, "p");
    assert!(screen(&mut state).contains("Snapshot review 1 of 3"));

    press(&mut state, "a");
    let shown = screen(&mut state);
    assert!(shown.contains("Snapshot review 2 of 3"), "{shown}");
    assert!(shown.contains("two.default.snap.yaml"), "{shown}");
    press(&mut state, "r");
    assert!(screen(&mut state).contains("Snapshot review 3 of 3"));
    press(&mut state, "a");
    assert!(
        state.overlay.is_none(),
        "the queue closes after the last one"
    );

    let now: Vec<String> = names
        .iter()
        .map(|n| fs::read_to_string(snapshot_file(&tmp, n)).unwrap())
        .collect();
    assert_eq!(now[0], recorded[0]);
    assert_eq!(now[1], stored[1], "rejected: unchanged");
    assert_eq!(now[2], recorded[2]);
    let decisions: Vec<Decision> = state
        .tabs
        .list
        .iter()
        .map(|t| {
            t.exchange.as_ref().unwrap().snapshots.items[0]
                .decision
                .clone()
        })
        .collect();
    assert_eq!(
        decisions,
        [Decision::Approved, Decision::Rejected, Decision::Approved]
    );
    press(&mut state, "Alt+A");
    assert!(state.overlay.is_none(), "nothing left to review");
}

#[tokio::test(flavor = "multi_thread")]
async fn secrets_never_reach_the_snapshot_tab_or_the_queue() {
    let server = TestServer::http().await;
    let tmp = workspace(&server, &[]);
    let mut state = shell(&tmp);
    open(&mut state, "echo.rman.yaml");
    send(&mut state).await;
    let file = snapshot_file(&tmp, "echo");
    let recorded = fs::read_to_string(&file).unwrap();
    assert!(!recorded.contains(CANARY), "{recorded}");
    assert!(recorded.contains("«redacted:token»"), "{recorded}");
    fs::write(&file, recorded.replace("GET", "PUT")).unwrap();

    send(&mut state).await;
    snapshot_tab(&mut state);
    let item = &reviews(&state).items[0];
    assert_eq!(item.review.state, SnapshotState::Failed);
    assert!(!format!("{item:?}").contains(CANARY));
    let shown = screen(&mut state);
    assert!(shown.contains("snapshot default failed"), "{shown}");
    assert!(!shown.contains(CANARY), "{shown}");
    press(&mut state, "Alt+A");
    let shown = screen(&mut state);
    assert!(shown.contains("Snapshot review 1 of 1"), "{shown}");
    assert!(!shown.contains(CANARY), "{shown}");
    press(&mut state, "a");
    let written = fs::read_to_string(&file).unwrap();
    assert_eq!(written, recorded);
    assert!(!written.contains(CANARY));
}

/// A failed snapshot review of `users/get-user.rman.yaml`: `Bob` stored,
/// `Ana` received.
fn failed_review(dir: &Path) -> SnapshotReview {
    let spec = rustman_core::format::from_yaml::<rustman_core::model::Checked<SnapshotAssertion>>(
        Path::new("s.yaml"),
        "{}",
    )
    .unwrap()
    .into_inner();
    let headers = vec![("Content-Type".to_owned(), "application/json".to_owned())];
    let request_file = dir.join("users/get-user.rman.yaml");
    let redactor = Redactor::new();
    let mut outcome = None;
    for (body, mode) in [
        (r#"{"id":42,"name":"Bob"}"#, SnapshotMode::Record),
        (
            r#"{"id":42,"name":"Ana","role":"admin"}"#,
            SnapshotMode::Record,
        ),
    ] {
        let exchange = Exchange {
            kind: "http",
            status: Some(200),
            headers: &headers,
            body: body.as_bytes(),
            events: None,
        };
        let ctx = SnapshotContext {
            request_file: &request_file,
            request: "users/get-user.rman.yaml",
            mode,
            redactor: &redactor,
        };
        outcome = Some(check(&spec, &exchange, &ctx).unwrap());
    }
    SnapshotReview::of(&outcome.unwrap(), &redactor).unwrap()
}

#[test]
fn the_snapshot_tab_renders_the_diff() {
    let tmp = tempfile::tempdir().unwrap();
    let review = failed_review(tmp.path());
    let ui = UiOptions::resolve(
        &TuiConfig::default(),
        Overrides::default(),
        ColorMode::NoColor,
    );
    let mut state = State::with_screen(Screen::Shell(Session::Scratch)).with_ui(ui);
    update(
        &mut state,
        Action::Resize {
            width: 100,
            height: 30,
        },
    );
    crate::workbench::new_tab(&mut state);
    let mut shown = received(200, "application/json", br#"{"id":42,"name":"Ana"}"#);
    shown.snapshots = Reviews::new(vec![review], SnapshotMode::Record);
    state.tabs.active_mut().unwrap().show(shown);
    snapshot_tab(&mut state);
    insta::assert_snapshot!("snapshot_tab_failed", screen(&mut state));

    press(&mut state, "r");
    insta::assert_snapshot!("snapshot_tab_rejected", screen(&mut state));
    let reviews = &mut state
        .tabs
        .active_mut()
        .unwrap()
        .exchange
        .as_mut()
        .unwrap()
        .snapshots;
    reviews.items[0].decision = Decision::Open;
    reviews.ci = true;
    insta::assert_snapshot!("snapshot_tab_ci", screen(&mut state));
    assert!(state.overlay.is_none());

    let reviews = &mut state
        .tabs
        .active_mut()
        .unwrap()
        .exchange
        .as_mut()
        .unwrap()
        .snapshots;
    reviews.ci = false;
    press(&mut state, "Alt+A");
    assert!(matches!(state.overlay, Some(Overlay::SnapshotQueue(_))));
    insta::assert_snapshot!("snapshot_queue", screen(&mut state));
    press(&mut state, "Esc");
    assert!(state.overlay.is_none());
}
