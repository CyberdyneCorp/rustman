//! Environments and secrets driven through `update` with keys (tasks 7.1
//! to 7.3): the switcher, the next send, the reveal toggle and the TLS
//! badge. Effects run synchronously, the way the effect runner would.

use std::fs;
use std::path::Path;

use rustman_core::{Secret, SecretBackend, SecretStore};
use rustman_test_support::TestServer;

use crate::app::{Overlay, State, update};
use crate::config::Overrides;
use crate::reveal::Revealed;
use crate::theme::ColorMode;
use crate::vars_view::VarScope;
use crate::workbench_tests::{press, run, text};

/// A secret value that must only be drawn while secrets are revealed.
pub(crate) const CANARY: &str = "canary-51d0-reveal";

/// A workspace under `tmp/ws` with `files` (relative path, contents).
pub(crate) fn workspace(files: &[(&str, &str)]) -> tempfile::TempDir {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("ws");
    for (rel, text) in files {
        let path = root.join(rel);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, text).unwrap();
    }
    tmp
}

/// The workspace of `tmp` loaded into a `width` x `height` shell that uses
/// the file secret store, as a fresh start of the TUI.
pub(crate) fn load(tmp: &tempfile::TempDir, width: u16, height: u16) -> State {
    let config = tmp.path().join("config");
    fs::create_dir_all(&config).unwrap();
    let (mut state, effects) = crate::initial_state_with(
        &tmp.path().join("ws"),
        Overrides::default(),
        Some(&config),
        ColorMode::NoColor,
    );
    state.secret_backend = Some(SecretBackend::File);
    run(&mut state, effects);
    update(&mut state, crate::app::Action::Resize { width, height });
    state
}

/// A multi-threaded runtime for the test server; the test itself stays
/// synchronous so sends can block on their own runtime.
pub(crate) fn server() -> (tokio::runtime::Runtime, TestServer) {
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();
    let server = rt.block_on(TestServer::http());
    (rt, server)
}

/// Whether some line of `screen` has `words` one after the other.
fn has_row(screen: &str, words: &[&str]) -> bool {
    screen.lines().any(|line| {
        let tokens: Vec<&str> = line.split_whitespace().collect();
        tokens.windows(words.len()).any(|w| w == words)
    })
}

fn status_line(state: &State) -> String {
    text(state).lines().last().unwrap_or_default().to_owned()
}

const TOML: &str = "[workspace]\nname = \"demo\"\nversion = 1\n\n[defaults]\n\
    default_environment = \"dev\"\n\n[variables]\nshared = \"from-toml\"\n";

#[test]
fn the_switcher_lists_variables_and_the_next_send_uses_the_new_environment() {
    // Scenario "Switch environment".
    let (_rt, server) = server();
    let dev = format!(
        "variables:\n  base_url: {}\n  who: dev-user\nsecrets: [token]\n",
        server.url("")
    );
    let staging = format!(
        "variables:\n  base_url: {}\n  who: staging-user\n",
        server.url("")
    );
    let tmp = workspace(&[
        ("rustman.toml", TOML),
        ("environments/dev.yaml", &dev),
        ("environments/staging.yaml", &staging),
        (
            "who.rman.yaml",
            "version: 1\nkind: http\nname: Who\nmethod: GET\nurl: '{{base_url}}/echo?who={{who}}'\n",
        ),
    ]);
    let mut state = load(&tmp, 120, 40);
    crate::workbench::open_path(&mut state, Path::new("who.rman.yaml"));
    assert!(status_line(&state).contains("env: dev"));

    press(&mut state, "Alt+e");
    let screen = text(&state);
    assert!(screen.contains("Switch environment"), "{screen}");
    assert!(has_row(&screen, &["dev", "active"]), "{screen}");
    assert!(screen.contains("Variables with dev"), "{screen}");
    assert!(
        has_row(&screen, &["who", "environment", "dev-user"]),
        "{screen}"
    );
    assert!(
        has_row(&screen, &["token", "environment", "••••••••"]),
        "{screen}"
    );
    assert!(
        has_row(&screen, &["shared", "rustman.toml", "from-toml"]),
        "{screen}"
    );

    press(&mut state, "Down");
    let screen = text(&state);
    assert!(screen.contains("Variables with staging"), "{screen}");
    assert!(
        has_row(&screen, &["who", "environment", "staging-user"]),
        "{screen}"
    );
    press(&mut state, "Enter");
    assert!(state.overlay.is_none());
    assert!(status_line(&state).contains("env: staging"));

    press(&mut state, "Ctrl+r");
    let tab = state.tabs.active().unwrap();
    let exchange = tab.exchange.as_ref().unwrap().record.exchange.as_ref();
    let url = &exchange.expect("sent").request.url;
    assert!(url.ends_with("/echo?who=staging-user"), "{url}");
}

/// A workspace whose environment `dev` holds the secret `token`, set to
/// [`CANARY`] in the file store, with `staging` beside it.
fn secret_workspace() -> tempfile::TempDir {
    let tmp = workspace(&[
        ("rustman.toml", TOML),
        (
            "environments/dev.yaml",
            "variables:\n  base_url: http://localhost:8080\nsecrets: [token]\n",
        ),
        (
            "environments/staging.yaml",
            "variables:\n  base_url: http://staging.invalid\nsecrets: [token]\n",
        ),
        (
            "me.rman.yaml",
            "version: 1\nkind: http\nname: Me\nmethod: GET\nurl: '{{base_url}}/me'\n\
             headers:\n- name: Authorization\n  value: Bearer {{token}}\n",
        ),
    ]);
    SecretStore::open(&tmp.path().join("ws"), SecretBackend::File)
        .set("dev", &Secret::new("token", CANARY.to_owned()))
        .unwrap();
    tmp
}

#[test]
fn secrets_show_only_while_revealed_and_never_after_a_restart() {
    // Scenarios "Reveal is session-only" and "Badge while revealed".
    let tmp = secret_workspace();
    let mut state = load(&tmp, 120, 40);
    crate::workbench::open_path(&mut state, Path::new("me.rman.yaml"));
    press(&mut state, "Alt+e");
    let masked = text(&state);
    assert!(
        has_row(&masked, &["token", "environment", "••••••••"]),
        "{masked}"
    );
    assert!(!masked.contains(CANARY), "{masked}");
    insta::assert_snapshot!("environment_switcher_variables", masked);
    press(&mut state, "Esc");

    press(&mut state, "Alt+r");
    assert!(status_line(&state).contains("SECRETS VISIBLE"));
    press(&mut state, "Alt+e");
    let revealed = text(&state);
    assert!(
        has_row(&revealed, &["token", "environment", CANARY]),
        "{revealed}"
    );
    // Another environment's secret was not read: it stays masked.
    press(&mut state, "Down");
    assert!(!text(&state).contains(CANARY));
    press(&mut state, "Esc");

    press(&mut state, "Alt+r");
    assert!(!status_line(&state).contains("SECRETS VISIBLE"));
    press(&mut state, "Alt+e");
    let hidden = text(&state);
    assert!(!hidden.contains(CANARY), "{hidden}");
    assert!(
        !format!("{state:?}").contains(CANARY),
        "Debug never shows it"
    );

    // A restart starts masked.
    press(&mut state, "Esc Alt+r");
    assert!(state.status.secrets_visible);
    drop(state);
    let mut state = load(&tmp, 120, 40);
    assert!(!state.status.secrets_visible);
    press(&mut state, "Alt+e");
    assert!(matches!(state.overlay, Some(Overlay::Palette(_))));
    let screen = text(&state);
    assert!(
        !screen.contains(CANARY) && !screen.contains("SECRETS VISIBLE"),
        "{screen}"
    );
}

#[test]
fn switching_environment_while_revealed_reads_the_new_secrets() {
    let tmp = secret_workspace();
    SecretStore::open(&tmp.path().join("ws"), SecretBackend::File)
        .set(
            "staging",
            &Secret::new("token", "staging-canary".to_owned()),
        )
        .unwrap();
    let mut state = load(&tmp, 120, 40);
    press(&mut state, "Alt+r");
    assert_eq!(state.revealed.value(Some("dev"), "token"), Some(CANARY));
    press(&mut state, "Alt+e Down Enter");
    assert_eq!(state.revealed.value(Some("dev"), "token"), None);
    assert_eq!(
        state.revealed.value(Some("staging"), "token"),
        Some("staging-canary")
    );
}

#[test]
fn the_tooltip_shows_a_revealed_secret() {
    let tmp = secret_workspace();
    let state = load(&tmp, 120, 40);
    let ws = crate::workbench::workspace(&state).unwrap();
    let glyphs = &crate::glyphs::UNICODE;
    let path = Path::new("me.rman.yaml");
    let masked = VarScope::new(ws, Some("dev"), Some(path), None);
    assert_eq!(
        masked.tooltip("token", glyphs),
        "{{token}} = •••••••• · secret (environment)"
    );
    let mut revealed = Revealed::default();
    revealed.set("dev".into(), vec![Secret::new("token", CANARY.to_owned())]);
    let shown = VarScope::new(ws, Some("dev"), Some(path), None).with_revealed(Some(&revealed));
    assert_eq!(
        shown.tooltip("token", glyphs),
        format!("{{{{token}}}} = {CANARY} · secret (environment)")
    );
    let other = VarScope::new(ws, Some("staging"), Some(path), None).with_revealed(Some(&revealed));
    assert!(!other.tooltip("token", glyphs).contains(CANARY));
}

#[test]
fn an_unreadable_store_is_an_error_toast() {
    let tmp = secret_workspace();
    let mut state = load(&tmp, 120, 40);
    fs::write(tmp.path().join("ws/.rustman/secrets.yaml"), "{not yaml").unwrap();
    press(&mut state, "Alt+r");
    let toast = state.notifications.visible().next().unwrap();
    assert!(
        toast.message.starts_with("Cannot read secrets"),
        "{}",
        toast.message
    );
}

#[test]
fn tls_insecure_badge_for_the_workspace() {
    // Scenario "Verification disabled": `verify_tls = false` is the
    // workspace default every environment sends with.
    let tmp = workspace(&[
        (
            "rustman.toml",
            "[workspace]\nname = \"demo\"\nversion = 1\n\n[defaults]\n\
             default_environment = \"dev\"\nverify_tls = false\n",
        ),
        ("environments/dev.yaml", "variables: {}\n"),
    ]);
    let mut state = load(&tmp, 100, 24);
    insta::assert_snapshot!("tls_insecure_status", status_line(&state));
    press(&mut state, "Ctrl+t");
    assert!(status_line(&state).contains("TLS: INSECURE"), "stays on");
}

#[test]
fn tls_insecure_badge_follows_the_active_request() {
    // Scenario "Verification disabled for one request".
    let tmp = workspace(&[
        ("rustman.toml", TOML),
        ("environments/dev.yaml", "variables: {}\n"),
        (
            "insecure.rman.yaml",
            "version: 1\nkind: http\nname: Insecure\nmethod: GET\nurl: https://self-signed.invalid/\n\
             settings:\n  verify_tls: false\n",
        ),
    ]);
    let mut state = load(&tmp, 100, 24);
    assert!(status_line(&state).contains("TLS verified"));
    crate::workbench::open_path(&mut state, Path::new("insecure.rman.yaml"));
    let now = state.now;
    update(&mut state, crate::app::Action::Tick(now));
    assert!(status_line(&state).contains("TLS: INSECURE"));
    press(&mut state, "Ctrl+t");
    assert!(status_line(&state).contains("TLS verified"));
    press(&mut state, "Alt+1");
    assert!(status_line(&state).contains("TLS: INSECURE"));
}
