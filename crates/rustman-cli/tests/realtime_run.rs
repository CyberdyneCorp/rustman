//! `rustman run` over `kind: ws` requests (add-realtime-client tasks 2.1,
//! 4.3, 4.4): a session that passes, a refused handshake that exits with
//! code 3, and a failed `close_code` assertion that exits with code 1 in a
//! report valid against `rustman report schema`.

mod common;

use common::{rustman, write};
use rustman_test_support::{RealtimeWsServer, WsAction, WsBehavior};

/// A server and the runtime it is served from (dropped after it).
struct Served {
    server: RealtimeWsServer,
    _runtime: tokio::runtime::Runtime,
}

fn serve(behavior: WsBehavior) -> Served {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();
    let server = runtime.block_on(RealtimeWsServer::start(behavior));
    Served {
        server,
        _runtime: runtime,
    }
}

fn workspace(url: &str) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    write(
        root,
        "rustman.toml",
        "[workspace]\nname = \"t\"\nversion = 1\n",
    );
    write(
        root,
        "feed.rman.yaml",
        &format!(
            "version: 1\nkind: ws\nname: Feed\nurl: {url}\nsteps:\n- send: {{text: hi}}\n- wait_for: {{regex: hi}}\n- close: {{}}\n"
        ),
    );
    dir
}

#[test]
fn a_session_that_completes_passes() {
    let served = serve(WsBehavior::echo());
    let dir = workspace(&served.server.url("/feed"));
    rustman(dir.path())
        .args(["run", "feed.rman.yaml"])
        .assert()
        .code(0);
    assert_eq!(served.server.sessions()[0].texts(), ["hi"]);
}

#[test]
fn a_refused_handshake_exits_with_code_3() {
    let served = serve(WsBehavior::default().refuse(401, "token required"));
    let dir = workspace(&served.server.url("/feed"));
    let output = rustman(dir.path())
        .args(["run", "feed.rman.yaml"])
        .assert()
        .code(3)
        .get_output()
        .clone();
    let shown = String::from_utf8_lossy(&output.stdout).into_owned()
        + &String::from_utf8_lossy(&output.stderr);
    assert!(
        shown.contains("ws: handshake failed: 401 Unauthorized"),
        "{shown}"
    );
}

/// `rustman report schema`, parsed.
fn report_schema(dir: &std::path::Path) -> serde_json::Value {
    let output = rustman(dir)
        .args(["report", "schema"])
        .assert()
        .code(0)
        .get_output()
        .stdout
        .clone();
    serde_json::from_slice(&output).unwrap()
}

#[test]
fn an_unexpected_close_code_exits_1_with_a_valid_report() {
    let behavior = WsBehavior::default().on_connect(vec![WsAction::Close(1011, "internal".into())]);
    let served = serve(behavior);
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    write(
        root,
        "rustman.toml",
        "[workspace]\nname = \"t\"\nversion = 1\n",
    );
    let url = served.server.url("/feed");
    write(
        root,
        "close.rman.yaml",
        &format!(
            "version: 1\nkind: ws\nname: Close\nurl: {url}\nassertions:\n- close_code: 1000\n"
        ),
    );
    let output = rustman(root)
        .args(["run", "--reporter", "json"])
        .assert()
        .code(1)
        .get_output()
        .clone();
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    let schema = report_schema(root);
    let validator = jsonschema::validator_for(&schema).unwrap();
    let errors: Vec<String> = validator
        .iter_errors(&report)
        .map(|e| e.to_string())
        .collect();
    assert!(errors.is_empty(), "{errors:?}");
    let request = &report["requests"][0];
    assert_eq!(
        request["assertions"][0]["message"],
        "close_code: expected 1000, got 1011"
    );
    let realtime = &request["realtime"];
    assert_eq!(realtime["protocol"], "ws");
    assert_eq!(realtime["end"]["close_code"], 1011);
    assert_eq!(realtime["end"]["reason"], "closed by server: 1011 internal");
}

/// A request without `limits` stops at the headless default of 50 received
/// messages rather than listening for the default 30 s.
#[test]
fn a_session_without_limits_stops_at_fifty_messages() {
    let behavior = WsBehavior::default().on_connect(vec![WsAction::Forever {
        text: r#"{"n":{n}}"#.into(),
        every: std::time::Duration::from_millis(1),
    }]);
    let served = serve(behavior);
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    write(
        root,
        "rustman.toml",
        "[workspace]\nname = \"t\"\nversion = 1\n",
    );
    let url = served.server.url("/feed");
    write(
        root,
        "feed.rman.yaml",
        &format!("version: 1\nkind: ws\nname: Feed\nurl: {url}\n"),
    );
    let output = rustman(root)
        .args(["run", "--reporter", "json"])
        .assert()
        .code(0)
        .get_output()
        .clone();
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    let realtime = &report["requests"][0]["realtime"];
    assert_eq!(realtime["received"], 50, "{realtime}");
    assert_eq!(realtime["end"]["reason"], "max_messages reached (50)");
}
