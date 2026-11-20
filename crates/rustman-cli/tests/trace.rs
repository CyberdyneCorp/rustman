//! Trace ids in `rustman send`, `history show` and `rustman trace open`
//! (add-trace-propagation tasks 3.5 and 3.6), against loopback servers only.

mod common;

use std::path::Path;

use common::{Server, rustman, workspace, write};
use predicates::prelude::*;
use rustman_test_support::routes::BACKEND_TRACE_ID;

/// A workspace whose `dev` environment has the `trace:` block `trace`, a
/// secret `api_key` holding the canary, and the request `traced`.
fn traced_workspace(server: &Server, trace: &str) -> tempfile::TempDir {
    traced_workspace_with(server, trace, "bgg-c4n4ry-Qz9")
}

/// [`traced_workspace`] with `secret` as the value of `api_key`.
fn traced_workspace_with(server: &Server, trace: &str, secret: &str) -> tempfile::TempDir {
    let ws = workspace(server);
    let env = format!(
        "variables:\n  base_url: {}\nsecrets: [api_key]\n{trace}",
        server.url("")
    );
    write(ws.path(), "environments/dev.yaml", &env);
    write(
        ws.path(),
        "traced.rman.yaml",
        "version: 1\nkind: http\nname: traced\nurl: '{{base_url}}/traced'\n",
    );
    rustman(ws.path())
        .args(["secret", "set", "api_key", "--env", "dev"])
        .write_stdin(secret.to_owned())
        .assert()
        .success();
    ws
}

fn send_stderr(ws: &Path) -> String {
    let output = rustman(ws)
        .args(["send", "traced", "--env", "dev"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(0), "{output:?}");
    String::from_utf8(output.stderr).unwrap()
}

/// The `trace: <32 hex>` line of `stderr`.
fn trace_id(stderr: &str) -> String {
    stderr
        .lines()
        .filter_map(|l| l.strip_prefix("trace: "))
        .find(|id| id.len() == 32 && id.bytes().all(|b| b.is_ascii_hexdigit()))
        .unwrap_or_else(|| panic!("no trace id in {stderr}"))
        .to_owned()
}

/// The id of the newest history entry.
fn newest_entry(ws: &Path) -> String {
    let output = rustman(ws)
        .args(["history", "list", "--json", "--limit", "1"])
        .output()
        .unwrap();
    let rows: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    rows[0]["id"].as_str().unwrap().to_owned()
}

#[test]
fn send_prints_the_trace_id_and_the_tempo_link_on_stderr() {
    let server = Server::http();
    let trace = "trace:\n  enabled: true\n  link: tempo\n  link_base: http://grafana:3000\n";
    let ws = traced_workspace(&server, trace);
    let stderr = send_stderr(ws.path());
    trace_id(&stderr);
    let tempo = format!(
        "trace: http://grafana:3000/explore?left=%7B%22datasource%22:%22tempo%22,%22queries%22:%5B%7B%22queryType%22:%22traceql%22,%22query%22:%22{BACKEND_TRACE_ID}%22%7D%5D%7D"
    );
    assert!(stderr.contains(&tempo), "{stderr}");
    assert!(
        stderr.contains(&format!("trace: backend {BACKEND_TRACE_ID}")),
        "{stderr}"
    );
}

#[test]
fn a_preset_link_and_its_base_merge_across_rustman_toml_and_the_environment() {
    let server = Server::http();
    let toml = "[workspace]\nname = \"t\"\nversion = 1\n";
    let jaeger = format!("trace: http://localhost:16686/trace/{BACKEND_TRACE_ID}");
    let configure = |ws: &Path, settings: &str, trace: &str| {
        write(ws, "rustman.toml", &format!("{toml}{settings}"));
        let env = format!(
            "variables:\n  base_url: {}\nsecrets: [api_key]\n{trace}",
            server.url("")
        );
        write(ws, "environments/dev.yaml", &env);
    };
    let cases = [
        (
            "[trace]\nenabled = true\nlink = \"jaeger\"\n",
            "trace:\n  link_base: http://localhost:16686\n",
        ),
        (
            "[trace]\nlink_base = \"http://localhost:16686\"\n",
            "trace:\n  enabled: true\n  link: jaeger\n",
        ),
    ];
    for (settings, env) in cases {
        let ws = traced_workspace(&server, "");
        configure(ws.path(), settings, env);
        let stderr = send_stderr(ws.path());
        assert!(stderr.contains(&jaeger), "{settings} / {env}: {stderr}");
    }
    let ws = traced_workspace(&server, "");
    configure(
        ws.path(),
        "[trace]\nlink = \"jaeger\"\n",
        "trace:\n  enabled: true\n",
    );
    rustman(ws.path())
        .args(["send", "traced", "--env", "dev"])
        .assert()
        .code(2)
        .stderr(predicate::str::contains(
            "dev.yaml: trace.link: preset 'jaeger' needs trace.link_base",
        ));
}

#[test]
fn send_prints_no_trace_line_when_tracing_is_off() {
    let server = Server::http();
    let ws = traced_workspace(&server, "");
    let stderr = send_stderr(ws.path());
    assert!(!stderr.contains("trace:"), "{stderr}");
}

#[test]
fn trace_open_exits_2_without_a_link_template() {
    let server = Server::http();
    let ws = traced_workspace(&server, "trace:\n  enabled: true\n");
    send_stderr(ws.path());
    let id = newest_entry(ws.path());
    rustman(ws.path())
        .args(["trace", "open", &id[..8]])
        .assert()
        .code(2)
        .stderr(predicate::str::contains(
            "trace: no link template configured ([trace].link)",
        ));
}

#[test]
fn trace_open_resolves_an_id_prefix_and_prints_the_backend_link() {
    let server = Server::http();
    let trace = "trace:\n  enabled: true\n  link: jaeger\n  link_base: http://localhost:16686\n";
    let ws = traced_workspace(&server, trace);
    send_stderr(ws.path());
    let id = newest_entry(ws.path());
    rustman(ws.path())
        .args(["trace", "open", &id.to_lowercase()[..10], "--no-browser"])
        .assert()
        .success()
        .stdout(format!("http://localhost:16686/trace/{BACKEND_TRACE_ID}\n"));
    rustman(ws.path())
        .args(["trace", "open", "ZZZZ", "--no-browser"])
        .assert()
        .code(2);
}

#[test]
fn trace_open_needs_a_traced_exchange() {
    let server = Server::http();
    let ws = traced_workspace(&server, "trace:\n  link: jaeger\n  link_base: http://j\n");
    send_stderr(ws.path());
    let id = newest_entry(ws.path());
    rustman(ws.path())
        .args(["trace", "open", &id, "--no-browser"])
        .assert()
        .code(2)
        .stderr(predicate::str::contains("carries no trace context"));
}

#[test]
fn secret_baggage_is_redacted_in_history_and_reports() {
    let server = Server::http();
    let trace = "trace:\n  enabled: true\n  baggage:\n    api_key: '{{api_key}}'\n";
    let ws = traced_workspace(&server, trace);
    let stderr = send_stderr(ws.path());
    assert!(!stderr.contains("bgg-c4n4ry-Qz9"), "{stderr}");
    let id = newest_entry(ws.path());
    let shown = rustman(ws.path())
        .args(["history", "show", &id])
        .output()
        .unwrap();
    let shown = String::from_utf8(shown.stdout).unwrap();
    assert!(
        shown.contains("baggage: api_key=«redacted:api_key»"),
        "{shown}"
    );
    assert!(
        shown.contains(&format!("trace: {}", trace_id(&stderr))),
        "{shown}"
    );
    let report = ws.path().join("report.json");
    rustman(ws.path())
        .args(["run", "traced.rman.yaml", "--env", "dev", "--reporter"])
        .arg(format!("json:{}", report.display()))
        .assert()
        .success();
    let json = std::fs::read_to_string(&report).unwrap();
    assert!(!json.contains("bgg-c4n4ry-Qz9"), "{json}");
    let value: serde_json::Value = serde_json::from_str(&json).unwrap();
    let trace = &value["requests"][0]["trace"];
    assert_eq!(trace["baggage"], "api_key=«redacted:api_key»");
    assert_eq!(trace["span_id"].as_str().unwrap().len(), 16);
    assert_no_canary(&ws.path().join(".rustman/history"));
}

/// Regression: a secret whose baggage encoding (`=`, `+`, `/` kept, space
/// and `;` escaped) matched no redaction form was written to history.
#[test]
fn secret_baggage_needing_encoding_is_redacted_in_history() {
    let server = Server::http();
    let trace = "trace:\n  enabled: true\n  baggage:\n    api_key: '{{api_key}}'\n";
    let ws = traced_workspace_with(&server, trace, "Zm9vYmFy/+== t;k=eq");
    let stderr = send_stderr(ws.path());
    let encoded = "Zm9vYmFy/+==%20t%3Bk=eq";
    assert!(!stderr.contains(encoded), "{stderr}");
    let id = newest_entry(ws.path());
    let shown = rustman(ws.path())
        .args(["history", "show", &id])
        .output()
        .unwrap();
    let shown = String::from_utf8(shown.stdout).unwrap();
    assert!(
        shown.contains("baggage: api_key=«redacted:api_key»"),
        "{shown}"
    );
    assert_none_contains(&ws.path().join(".rustman/history"), encoded);
    assert_none_contains(&ws.path().join(".rustman/history"), "Zm9vYmFy/+== t;k=eq");
}

/// No file under `dir` holds the canary secret.
fn assert_no_canary(dir: &Path) {
    assert_none_contains(dir, "bgg-c4n4ry-Qz9");
}

/// No file under `dir` holds `needle`.
fn assert_none_contains(dir: &Path, needle: &str) {
    for entry in std::fs::read_dir(dir).unwrap().flatten() {
        let path = entry.path();
        if path.is_dir() {
            assert_none_contains(&path, needle);
        } else if let Ok(text) = std::fs::read_to_string(&path) {
            assert!(!text.contains(needle), "{}", path.display());
        }
    }
}
