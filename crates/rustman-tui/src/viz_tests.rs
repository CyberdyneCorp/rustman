//! The viz hand-off end to end (task 8.1): a request is sent to the
//! in-process test server, `v` hands the exchange to a stub companion that
//! answers the handshake and records what it receives, and without a
//! companion the toast offers the HTML report, which `H` writes. Both new
//! output paths are canary-checked for the secret the request sends.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use rustman_core::{Secret, SecretBackend, SecretStore};

use crate::app::State;
use crate::env_tests::{CANARY, load, server, workspace};
use crate::focus::Focus;
use crate::viz::Discovery;
use crate::workbench_tests::press;

const ACK: &str = r#"{"type":"hello_ack","protocol":"rustman-viz/1","viewer":"stub-viz 0.1","views":["response_inspector"]}"#;

/// A stub companion that answers `hello` with `reply`, appends every line
/// it reads to `<script>.log` and exits with `code`.
fn stub(dir: &Path, reply: &str, code: u8) -> PathBuf {
    if cfg!(windows) {
        let script = dir.join("stub-viz.cmd");
        let text = format!(
            "@echo off\r\nset /p hello=\r\n>\"%~f0.log\" echo %hello%\r\necho {reply}\r\n\
             more >>\"%~f0.log\"\r\nexit /b {code}\r\n"
        );
        fs::write(&script, text).unwrap();
        return script;
    }
    let script = dir.join("stub-viz");
    let text = format!(
        "#!/bin/sh\nIFS= read -r hello\nprintf '%s\\n' \"$hello\" > \"$0.log\"\n\
         printf '%s\\n' '{reply}'\ncat >> \"$0.log\"\nexit {code}\n"
    );
    fs::write(&script, text).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).unwrap();
    }
    script
}

/// The stub's log once it holds `until` (the stub writes it after the
/// producer is done, so it is polled with a generous limit).
fn log_of(script: &Path, until: &str) -> String {
    let log = PathBuf::from(format!("{}.log", script.display()));
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        let text = fs::read_to_string(&log).unwrap_or_default();
        if text.contains(until) || Instant::now() > deadline {
            return text;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

fn toast(state: &State) -> String {
    state
        .notifications
        .visible()
        .next()
        .map(|t| t.message.clone())
        .unwrap_or_default()
}

/// The echo request sent with the secret `token` in its URL and headers;
/// the response viewer focused.
fn sent(server: &rustman_test_support::TestServer) -> (tempfile::TempDir, State) {
    let env = format!(
        "variables:\n  base_url: {}\nsecrets: [token]\n",
        server.url("")
    );
    let tmp = workspace(&[
        (
            "rustman.toml",
            "[workspace]\nname = \"demo\"\nversion = 1\n\n[defaults]\ndefault_environment = \"local\"\n",
        ),
        ("environments/local.yaml", &env),
        (
            "echo.rman.yaml",
            "version: 1\nkind: http\nname: Echo\nmethod: GET\nurl: '{{base_url}}/echo?t={{token}}'\n\
             headers:\n- name: X-Echo\n  value: '{{token}}'\n",
        ),
    ]);
    SecretStore::open(&tmp.path().join("ws"), SecretBackend::File)
        .set("local", &Secret::new("token", CANARY.to_owned()))
        .unwrap();
    let mut state = load(&tmp, 160, 45);
    crate::workbench::open_path(&mut state, Path::new("echo.rman.yaml"));
    press(&mut state, "Ctrl+r");
    let received = state
        .tabs
        .active()
        .unwrap()
        .exchange
        .as_ref()
        .expect("sent");
    assert!(
        received.record.exchange.is_some(),
        "{:?}",
        received.record.error
    );
    state.shell.focus = Focus::Response;
    (tmp, state)
}

#[test]
fn v_hands_the_exchange_to_the_companion() {
    let (_rt, server) = server();
    let (tmp, mut state) = sent(&server);
    let script = stub(tmp.path(), ACK, 0);
    state.viz = Discovery {
        override_path: Some(script.clone()),
        ..Discovery::default()
    };
    press(&mut state, "v");
    assert_eq!(toast(&state), "Opened in stub-viz 0.1");
    let log = log_of(&script, "\"close\"");
    assert!(log.contains(r#""type":"hello""#), "{log}");
    assert!(log.contains(r#""protocol":"rustman-viz/1""#), "{log}");
    if cfg!(unix) {
        // The batch stub's `more` may not keep every line; Unix shows the
        // whole stream.
        assert!(log.contains(r#""type":"open_view""#), "{log}");
        assert!(log.contains(r#""view":"response_inspector""#), "{log}");
        assert!(log.contains(r#""type":"inspect_response""#), "{log}");
        assert!(log.contains("redacted:token"), "{log}");
        assert!(log.contains(r#""type":"close""#), "{log}");
    }
    assert!(!log.contains(CANARY), "{log}");
}

#[test]
fn without_a_companion_the_toast_offers_the_html_report() {
    // Scenario "Companion not installed".
    let (_rt, server) = server();
    let (tmp, mut state) = sent(&server);
    state.viz = Discovery::default();
    press(&mut state, "v");
    let message = toast(&state);
    assert!(message.starts_with("Viz companion not found"), "{message}");
    assert!(message.ends_with("Export HTML report: H"), "{message}");

    press(&mut state, "H");
    let message = toast(&state);
    assert!(message.starts_with("Wrote HTML report"), "{message}");
    let reports = tmp.path().join("ws/.rustman/reports");
    let file = fs::read_dir(&reports)
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    let html = fs::read_to_string(&file).unwrap();
    assert!(html.starts_with("<!doctype html>"), "{html}");
    assert!(html.contains("<h1>Echo</h1>"), "{html}");
    assert!(html.contains("redacted:token"), "{html}");
    assert!(!html.contains(CANARY), "{html}");
}

#[test]
fn an_invalid_override_is_named_in_the_toast() {
    let (_rt, server) = server();
    let (tmp, mut state) = sent(&server);
    let nope = tmp.path().join("nope").join("rustman-viz");
    state.viz = Discovery {
        override_path: Some(nope.clone()),
        path: std::env::var_os("PATH"),
        exe_dir: None,
    };
    press(&mut state, "v");
    let message = toast(&state);
    assert!(message.contains(&nope.display().to_string()), "{message}");
}

#[test]
fn a_rejecting_or_graphicless_companion_explains_why() {
    let (_rt, server) = server();
    let (tmp, mut state) = sent(&server);
    let reject = r#"{"type":"hello_reject","supported":["rustman-viz/2"]}"#;
    let script = stub(tmp.path(), reject, 0);
    state.viz = Discovery {
        override_path: Some(script),
        ..Discovery::default()
    };
    press(&mut state, "v");
    let message = toast(&state);
    assert!(
        message.starts_with("Viz companion speaks rustman-viz/2, rustman needs rustman-viz/1"),
        "{message}"
    );
    assert!(message.contains("Export HTML report"), "{message}");

    let dir = tmp.path().join("vulkan");
    fs::create_dir_all(&dir).unwrap();
    let vulkan = r#"{"type":"error","code":"vulkan_unavailable","message":"no Vulkan loader"}"#;
    state.viz.override_path = Some(stub(&dir, vulkan, 3));
    press(&mut state, "v");
    assert!(toast(&state).starts_with("Graphics unavailable (Vulkan)"));
}

#[test]
fn v_without_a_response_says_to_send_first() {
    let tmp = workspace(&[(
        "rustman.toml",
        "[workspace]\nname = \"demo\"\nversion = 1\n",
    )]);
    let mut state = load(&tmp, 120, 40);
    crate::workbench::new_tab(&mut state);
    state.shell.focus = Focus::Response;
    press(&mut state, "v");
    assert!(
        toast(&state).contains("Send the request"),
        "{}",
        toast(&state)
    );
    press(&mut state, "H");
    assert!(
        toast(&state).contains("Send the request"),
        "{}",
        toast(&state)
    );
}
