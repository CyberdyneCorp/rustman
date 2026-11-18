//! Snapshot testing end to end (snapshot-testing task 7.1): an `http` and a
//! `graphql` request in nested folders are recorded, a mutated response
//! fails in CI mode, `--update-snapshots` fixes it, and CI mode then
//! passes. Snapshot paths are printed with `/` on every platform. The
//! `jsonrpc` and `mcp` kinds are covered with stub exchanges in
//! `crates/rustman-core/tests/snapshot_kinds.rs`.

mod common;

use std::fs;
use std::path::Path;

use assert_cmd::Command;
use common::{Server, request, write};
use rustman_test_support::{GraphqlHttpServer, RecordedRequest, Reply};
use serde_json::json;

const HTTP_SNAPSHOT: &str = "api/users/__snapshots__/get-user.default.snap.yaml";
const GRAPHQL_SNAPSHOT: &str = "api/graph/__snapshots__/user.default.snap.yaml";

/// `rustman` without the caller's `CI` variable, which turns CI mode on.
fn rustman(dir: &Path) -> Command {
    let mut cmd = common::rustman(dir);
    cmd.env_remove("CI");
    cmd
}

/// The GraphQL server's answer: the `v` variable echoed back.
fn respond(hit: &RecordedRequest) -> Reply {
    let v = hit.json()["variables"]["v"].clone();
    Reply::graphql(json!({"data": {"user": {"name": "Ada", "v": v}}}))
}

/// A workspace with `api/users/get-user` (http, `/echo?v={{v}}`) and
/// `api/graph/user` (graphql, variable `v`), both with a snapshot.
fn workspace(http: &Server, graphql: &GraphqlHttpServer) -> tempfile::TempDir {
    let ws = tempfile::tempdir().unwrap();
    let root = ws.path();
    write(
        root,
        "rustman.toml",
        "[workspace]\nname = \"t\"\nversion = 1\n",
    );
    write(
        root,
        "environments/dev.yaml",
        &format!(
            "variables:\n  base_url: {}\n  graph: {}\n  v: '1'\n",
            http.url(""),
            graphql.url("")
        ),
    );
    write(
        root,
        "api/users/get-user.rman.yaml",
        &request(
            "get-user",
            "{{base_url}}/echo?v={{v}}",
            "assertions:\n  - status: 200\n  - snapshot\n",
        ),
    );
    write(
        root,
        "api/graph/user.rman.yaml",
        "version: 1\nkind: graphql\nname: user\nendpoint: '{{graph}}/graphql'\n\
         document: 'query User($v: String) { user(v: $v) { name v } }'\n\
         variables:\n  v: '{{v}}'\nassertions:\n  - snapshot\n",
    );
    ws
}

fn run(ws: &Path, args: &[&str]) -> (Option<i32>, String) {
    let output = rustman(ws)
        .args(["run", "api", "--env", "dev"])
        .args(args)
        .output()
        .unwrap();
    let mut text = String::from_utf8_lossy(&output.stdout).into_owned();
    text.push_str(&String::from_utf8_lossy(&output.stderr));
    (output.status.code(), text)
}

fn files(ws: &Path) -> [String; 2] {
    [HTTP_SNAPSHOT, GRAPHQL_SNAPSHOT].map(|p| fs::read_to_string(ws.join(p)).unwrap())
}

/// Run with `args`, expect the exit `code` and every one of `needles`.
fn expect(ws: &Path, args: &[&str], code: i32, needles: &[&str]) {
    let (actual, out) = run(ws, args);
    assert_eq!(actual, Some(code), "{out}");
    for needle in needles {
        assert!(out.contains(needle), "missing {needle:?}: {out}");
    }
}

#[test]
fn record_fail_in_ci_then_update_over_http_and_graphql() {
    let http = Server::http();
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();
    let graphql = runtime.block_on(GraphqlHttpServer::start(respond));
    let ws = workspace(&http, &graphql);
    let ws = ws.path();

    // Record; paths use '/' on every platform.
    expect(
        ws,
        &[],
        0,
        &["2 snapshots recorded", HTTP_SNAPSHOT, GRAPHQL_SNAPSHOT],
    );
    let recorded = files(ws);
    assert!(recorded[0].contains("kind: http"), "{}", recorded[0]);
    assert!(recorded[1].contains("kind: graphql"), "{}", recorded[1]);

    // A mutated response fails in CI mode and writes nothing.
    let http_mismatch = format!("snapshot mismatch: {HTTP_SNAPSHOT}");
    let graphql_mismatch = format!("snapshot mismatch: {GRAPHQL_SNAPSHOT}");
    expect(
        ws,
        &["--ci", "--var", "v=2"],
        1,
        &[&http_mismatch, &graphql_mismatch, "2 snapshots failed"],
    );
    assert_eq!(files(ws), recorded);

    // Updating fixes it.
    expect(ws, &["-u", "--var", "v=2"], 0, &["2 snapshots updated"]);
    let updated = files(ws);
    assert_ne!(updated, recorded);

    // CI mode (here from the `CI` variable) now matches.
    let output = rustman(ws)
        .args(["run", "api", "--env", "dev", "--var", "v=2"])
        .env("CI", "true")
        .output()
        .unwrap();
    let out = String::from_utf8_lossy(&output.stdout);
    assert_eq!(output.status.code(), Some(0), "{out}");
    assert!(out.contains("2 snapshots matched"), "{out}");
    assert_eq!(files(ws), updated);
    drop(graphql);
}

#[test]
fn graphql_query_honors_ci_mode_and_notes_recorded_snapshots() {
    let http = Server::http();
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();
    let graphql = runtime.block_on(GraphqlHttpServer::start(respond));
    let ws = workspace(&http, &graphql);
    let ws = ws.path();
    let query = |args: &[&str], ci: Option<&str>| {
        let mut cmd = rustman(ws);
        cmd.args([
            "graphql",
            "query",
            "api/graph/user.rman.yaml",
            "--env",
            "dev",
        ])
        .args(args);
        if let Some(value) = ci {
            cmd.env("CI", value);
        }
        let output = cmd.output().unwrap();
        (
            output.status.code(),
            String::from_utf8_lossy(&output.stderr).into_owned(),
        )
    };
    let missing = format!("snapshot missing: {GRAPHQL_SNAPSHOT}");
    for (args, ci) in [(&["--ci"][..], None), (&[][..], Some("1"))] {
        let (code, err) = query(args, ci);
        assert_eq!(code, Some(1), "{err}");
        assert!(err.contains(&missing), "{err}");
    }
    assert!(
        !ws.join(GRAPHQL_SNAPSHOT).exists(),
        "CI mode wrote a snapshot"
    );
    let (code, err) = query(&[], None);
    assert_eq!(code, Some(0), "{err}");
    let note = format!("snapshot default recorded: {GRAPHQL_SNAPSHOT}");
    assert!(err.contains(&note), "{err}");
    drop(graphql);
}
