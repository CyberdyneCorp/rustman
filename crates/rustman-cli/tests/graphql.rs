//! `rustman graphql introspect` and `rustman graphql query`, and graphql
//! requests under `rustman run` (add-graphql-client tasks 5.4, 6.3, 6.4),
//! against in-process servers on their own runtime.

mod common;

use std::fs;
use std::path::Path;

use common::{closed_port, rustman, write};
use rustman_test_support::graphql::INTROSPECTION_DISABLED;
use rustman_test_support::{
    GraphqlHttpServer, GraphqlServer, GraphqlWsServer, Introspection, RecordedRequest, Reply,
    WsScript,
};
use serde_json::{Value, json};

const SDL: &str = "type Query { user(id: ID!): User }\ntype User { name: String email: String }\n";
const SECRET: &str = "s3cr3t-graphql-cli-canary";

/// A server and the runtime it is served from (dropped after it).
struct Served<S> {
    server: S,
    _runtime: tokio::runtime::Runtime,
}

fn serve<S>(start: impl Future<Output = S>) -> Served<S> {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();
    let server = runtime.block_on(start);
    Served {
        server,
        _runtime: runtime,
    }
}

/// A workspace whose `dev` environment points `base` at `base`, with the
/// secret `api_token` set to [`SECRET`].
fn workspace(base: &str) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    write(
        dir.path(),
        "rustman.toml",
        "[workspace]\nname = \"t\"\nversion = 1\n",
    );
    write(
        dir.path(),
        "environments/dev.yaml",
        &format!("variables:\n  base: {base}\nsecrets: [api_token]\n"),
    );
    rustman(dir.path())
        .args(["secret", "set", "api_token", "--env", "dev"])
        .write_stdin(SECRET)
        .assert()
        .success();
    dir
}

fn graphql_request(name: &str, extra: &str) -> String {
    format!("version: 1\nkind: graphql\nname: {name}\nendpoint: '{{{{base}}}}/graphql'\n{extra}")
}

/// Partial data with one error for `Users`; the `password` variable echoed
/// back for `Echo`; `{"ok": true}` on `/health`.
fn respond(hit: &RecordedRequest) -> Reply {
    if hit.uri == "/health" {
        return Reply::raw(200, "application/json", r#"{"ok":true}"#);
    }
    let body = hit.json();
    if body["operationName"] == "Echo" {
        let echoed = body["variables"]["password"].clone();
        return Reply::graphql(json!({"data": {"echo": echoed}}));
    }
    Reply::graphql(json!({
        "data": {"user": {"name": "Ada", "email": null}},
        "errors": [{
            "message": "email is private",
            "path": ["user", "email"],
            "locations": [{"line": 1, "column": 22}],
            "extensions": {"code": "FORBIDDEN"},
        }],
    }))
}

fn stdout(output: &std::process::Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn stderr(output: &std::process::Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

fn history_text(root: &Path) -> String {
    let dir = root.join(".rustman/history");
    let mut text = String::new();
    for entry in fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.extension().is_some_and(|x| x == "json") {
            text.push_str(&fs::read_to_string(path).unwrap());
        }
    }
    text
}

#[test]
fn introspect_caches_the_schema_and_writes_sdl() {
    let served = serve(GraphqlServer::start(SDL));
    let endpoint = served.server.url("/graphql");
    let ws = workspace(&served.server.url(""));
    let output = rustman(ws.path())
        .args([
            "graphql",
            "introspect",
            &endpoint,
            "--out",
            "schema.graphql",
        ])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
    let out = stdout(&output);
    let first = out.lines().next().unwrap();
    assert!(
        first.starts_with(&format!("introspected {endpoint}: 2 types, fetched at 20")),
        "{out}"
    );
    let cached = out.lines().nth(1).unwrap();
    assert!(
        cached.starts_with("cached in .rustman/graphql/schemas/"),
        "{out}"
    );
    let cache = cached.trim_start_matches("cached in ");
    assert!(ws.path().join(cache).is_file());
    assert!(out.contains("wrote schema.graphql"), "{out}");
    let sdl = fs::read_to_string(ws.path().join("schema.graphql")).unwrap();
    assert!(sdl.contains("type User"), "{sdl}");
}

#[test]
fn introspect_resolves_a_request_file() {
    let served = serve(GraphqlServer::start(SDL));
    let ws = workspace(&served.server.url(""));
    let request = graphql_request("Users", "document: '{ user(id: 1) { name } }'\n");
    write(ws.path(), "api/users.rman.yaml", &request);
    let output = rustman(ws.path())
        .args([
            "graphql",
            "introspect",
            "api/users.rman.yaml",
            "--env",
            "dev",
        ])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
    assert!(stdout(&output).contains(": 2 types"), "{}", stdout(&output));
}

#[test]
fn a_server_rejecting_introspection_exits_non_zero_with_the_hint() {
    let served = serve(GraphqlServer::with_introspection(
        SDL,
        Introspection::Disabled,
    ));
    let ws = workspace(&served.server.url(""));
    let output = rustman(ws.path())
        .args(["graphql", "introspect", &served.server.url("/graphql")])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(3));
    assert_eq!(
        stderr(&output).trim_end(),
        format!(
            "error: graphql: introspection failed: {INTROSPECTION_DISABLED}; set schema_file to validate offline"
        )
    );
}

#[test]
fn a_query_with_errors_prints_data_and_errors_and_exits_1() {
    let served = serve(GraphqlHttpServer::start(respond));
    let ws = workspace(&served.server.url(""));
    let request = graphql_request("Users", "document: '{ user(id: 1) { name email } }'\n");
    write(ws.path(), "users.rman.yaml", &request);
    let output = rustman(ws.path())
        .args(["graphql", "query", "users.rman.yaml", "--env", "dev"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1), "{}", stderr(&output));
    let out = stdout(&output);
    assert!(out.starts_with("HTTP 200 · partial · "), "{out}");
    assert!(
        out.contains("data:\n{\n  \"user\": {\n    \"name\": \"Ada\""),
        "{out}"
    );
    assert!(
        out.contains("errors:\n  - email is private (path user.email, at 1:22, code FORBIDDEN)"),
        "{out}"
    );
    let json = rustman(ws.path())
        .args([
            "graphql",
            "query",
            "users.rman.yaml",
            "--env",
            "dev",
            "--json",
        ])
        .output()
        .unwrap();
    assert_eq!(json.status.code(), Some(1));
    let value: Value = serde_json::from_slice(&json.stdout).unwrap();
    assert_eq!(value["result"], "partial");
    assert_eq!(value["data"]["user"]["name"], "Ada");
    assert_eq!(value["errors"][0]["path"], json!(["user", "email"]));
}

#[test]
fn a_query_without_errors_exits_0_and_secrets_stay_redacted() {
    let served = serve(GraphqlHttpServer::start(respond));
    let ws = workspace(&served.server.url(""));
    let request = graphql_request(
        "Echo",
        "document: 'query Echo($password: String) { echo(password: $password) }'\n\
         operation_name: Echo\nvariables:\n  password: '{{api_token}}'\n",
    );
    write(ws.path(), "echo.rman.yaml", &request);
    for extra in [&[][..], &["--json"][..]] {
        let output = rustman(ws.path())
            .args(["graphql", "query", "echo.rman.yaml", "--env", "dev", "-vvv"])
            .args(extra)
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
        let shown = format!("{}{}", stdout(&output), stderr(&output));
        assert!(!shown.contains(SECRET), "{shown}");
        assert!(stdout(&output).contains("«redacted:api_token»"), "{shown}");
    }
    assert!(served.server.hits()[0].body_text().contains(SECRET));
    let history = history_text(ws.path());
    assert!(!history.contains(SECRET), "{history}");
    assert!(
        history.contains(r#""operation_type": "query""#)
            || history.contains(r#""operation_type":"query""#),
        "{history}"
    );
}

#[test]
fn transport_failures_exit_3_and_invalid_documents_are_not_sent() {
    let served = serve(GraphqlHttpServer::start(respond));
    let ws = workspace(&served.server.url(""));
    let closed = format!("http://127.0.0.1:{}", closed_port());
    let unreachable = format!(
        "version: 1\nkind: graphql\nname: Down\nendpoint: '{closed}/graphql'\ndocument: '{{ a }}'\n"
    );
    write(ws.path(), "down.rman.yaml", &unreachable);
    let output = rustman(ws.path())
        .args(["graphql", "query", "down.rman.yaml"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(3));
    let error = stderr(&output);
    assert!(
        error.starts_with("error: ") && error.contains("127.0.0.1"),
        "{error}"
    );

    let broken = graphql_request("Broken", "document: '{ user(id: 1) { name }'\n");
    write(ws.path(), "broken.rman.yaml", &broken);
    let output = rustman(ws.path())
        .args(["graphql", "query", "broken.rman.yaml", "--env", "dev"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(3));
    assert!(
        stderr(&output).contains("error: graphql: the document is invalid: 1:"),
        "{}",
        stderr(&output)
    );
    assert!(served.server.hits().is_empty());
    rustman(ws.path())
        .args([
            "graphql",
            "query",
            "broken.rman.yaml",
            "--env",
            "dev",
            "--no-validate",
        ])
        .assert()
        .code(1);
    assert_eq!(served.server.hits().len(), 1);
}

#[test]
fn an_ad_hoc_query_needs_no_workspace() {
    let served = serve(GraphqlHttpServer::start(respond));
    let dir = tempfile::tempdir().unwrap();
    let endpoint = served.server.url("/graphql");
    let output = rustman(dir.path())
        .args([
            "graphql",
            "query",
            "--endpoint",
            &endpoint,
            "--query",
            "{ user(id: 1) { name } }",
        ])
        .args(["--no-history"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1), "{}", stderr(&output));
    assert_eq!(
        served.server.hits()[0].json()["query"],
        "{ user(id: 1) { name } }"
    );
}

#[test]
fn subscriptions_print_their_events_and_protocol() {
    let served = serve(GraphqlWsServer::start(WsScript::Legacy { events: 2 }));
    let ws = workspace(&served.server.url("ws", ""));
    let request = graphql_request(
        "Live",
        "transport: ws\ndocument: 'subscription { onMessage { n } }'\n",
    );
    write(ws.path(), "live.rman.yaml", &request);
    let output = rustman(ws.path())
        .args(["graphql", "query", "live.rman.yaml", "--env", "dev"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
    let out = stdout(&output);
    assert!(
        out.starts_with("legacy subscriptions-transport-ws · 2 events · ended: complete"),
        "{out}"
    );
    assert!(out.contains("event 2 at "), "{out}");
}

#[test]
fn a_subscription_closed_with_an_error_code_exits_1_and_stays_redacted() {
    let served = serve(GraphqlWsServer::start(WsScript::Close {
        code: 4403,
        before_ack: false,
    }));
    let ws = workspace(&served.server.url("ws", ""));
    let request = graphql_request(
        "Live",
        "transport: ws\ndocument: 'subscription { onMessage { n } }'\n\
         auth:\n  type: bearer\n  token: '{{api_token}}'\n",
    );
    write(ws.path(), "live.rman.yaml", &request);
    for extra in [&[][..], &["--json"][..]] {
        let output = rustman(ws.path())
            .args(["graphql", "query", "live.rman.yaml", "--env", "dev"])
            .args(extra)
            .output()
            .unwrap();
        let shown = format!("{}{}", stdout(&output), stderr(&output));
        assert_eq!(output.status.code(), Some(1), "{shown}");
        assert!(
            shown.contains("closed the connection without complete (4403"),
            "{shown}"
        );
        assert!(shown.contains("«redacted:api_token»"), "{shown}");
        assert!(!shown.contains(SECRET), "{shown}");
    }
    let output = rustman(ws.path())
        .args(["graphql", "query", "live.rman.yaml", "--env", "dev"])
        .output()
        .unwrap();
    assert!(
        stdout(&output).contains("ended: closed"),
        "{}",
        stdout(&output)
    );
    assert!(stdout(&output).contains("failed: the server closed"));
    let history = history_text(ws.path());
    assert!(!history.contains(SECRET), "{history}");
    assert!(history.contains("4403 Forbidden"), "{history}");
}

#[test]
fn max_events_bounds_cli_subscriptions() {
    let served = serve(GraphqlWsServer::start(WsScript::Modern {
        events: 50,
        complete: false,
    }));
    let ws = workspace(&served.server.url("ws", ""));
    let request = graphql_request(
        "Live",
        "transport: ws\ndocument: 'subscription { onMessage { n } }'\n",
    );
    write(ws.path(), "live.rman.yaml", &request);
    let output = rustman(ws.path())
        .args([
            "graphql",
            "query",
            "live.rman.yaml",
            "--env",
            "dev",
            "--json",
        ])
        .args(["--max-events", "3"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
    let value: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["protocol"], "graphql-transport-ws");
    assert_eq!(value["event_count"], 3);
    assert_eq!(value["ended"], "max_events");
}

#[test]
fn run_executes_a_collection_mixing_http_and_graphql() {
    let served = serve(GraphqlHttpServer::start(respond));
    let ws = workspace(&served.server.url(""));
    let health = "version: 1\nkind: http\nname: Health\nurl: '{{base}}/health'\nassertions:\n  - json: {path: $.ok, equals: true}\n";
    let users = graphql_request(
        "Users",
        "document: '{ user(id: 1) { name email } }'\nassertions:\n  - data: {path: $.user.name, equals: Ada}\n  - error_code: FORBIDDEN\n",
    );
    write(ws.path(), "mixed/1-health.rman.yaml", health);
    write(ws.path(), "mixed/2-users.rman.yaml", &users);
    let output = rustman(ws.path())
        .args(["run", "mixed", "--env", "dev", "--reporter", "json"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(0), "{}", stdout(&output));
    let report = common::valid_report(&output.stdout);
    let statuses: Vec<&str> = report["requests"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["status"].as_str().unwrap())
        .collect();
    assert_eq!(statuses, ["passed", "passed"]);
    let kinds: Vec<&str> = report["requests"][1]["assertions"]
        .as_array()
        .unwrap()
        .iter()
        .map(|a| a["kind"].as_str().unwrap())
        .collect();
    assert_eq!(kinds, ["data", "error_code"]);
}

#[test]
fn run_bounds_subscriptions_with_max_events() {
    let served = serve(GraphqlWsServer::start(WsScript::Modern {
        events: 50,
        complete: false,
    }));
    let ws = workspace(&served.server.url("ws", ""));
    let request = graphql_request(
        "Live",
        "transport: ws\ndocument: 'subscription { onMessage { n } }'\nassertions:\n  - data: {path: $, length: 4}\n",
    );
    write(ws.path(), "live.rman.yaml", &request);
    rustman(ws.path())
        .args(["run", "--env", "dev", "--max-events", "4"])
        .assert()
        .code(0);
}
