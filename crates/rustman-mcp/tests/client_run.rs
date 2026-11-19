//! `kind: mcp` requests under the `rustman-http` runner (add-mcp-client
//! tasks 7.2 to 7.4): MCP assertions and captures, one session per server
//! for the whole run, and history entries with notifications, redacted.

use std::fs;
use std::path::Path;

use rustman_core::history::HistoryStore;
use rustman_core::run::{RequestRecord, RequestStatus, plan};
use rustman_core::{Secret, SecretStore, Workspace};
use rustman_http::runner::{RunContext, RunOptions, Runner};
use std::sync::Arc;

use rustman_http::NO_ENVIRONMENT;
use rustman_mcp::client::oauth::{OAuthOptions, store_key};
use rustman_mcp::client::{ConnectOptions, McpRunner};
use rustman_test_support::McpHttpServer;
use rustman_test_support::mcp_oauth::DYNAMIC_CLIENT_ID;
use serde_json::Value;

const FIXTURE: &str = env!("CARGO_BIN_EXE_rustman-mcp-fixture");

const SECRET: &str = "s3cr3t-mcp-runner-canary";

const LIST: &str = "\
version: 1
kind: mcp
name: List
server: files
operation: tools/list
assertions:
  - tool_exists: search
  - tool_exists: missing_tool
";

const COUNT: &str = "\
version: 1
kind: mcp
name: Count
server: files
operation: tools/call
params:
  name: count
  arguments: {value: 3}
assertions:
  - is_error: false
  - structured: {path: $.count, equals: 3}
  - content_text_contains: counted
captures:
  - {var: n, from: json, path: $.count}
";

const PROGRESS: &str = "\
version: 1
kind: mcp
name: Progress
server: files
operation: tools/call
params:
  name: progress5
  arguments: {token: '{{api_token}}', n: '{{n}}'}
assertions:
  - content_text_contains: done
";

const BAD_COUNT: &str = "\
version: 1
kind: mcp
name: Bad count
server: files
operation: tools/call
params:
  name: count
  arguments: {value: 'three'}
";

const FAIL: &str = "\
version: 1
kind: mcp
name: Fail
server: files
operation: tools/call
params: {name: fail}
assertions:
  - is_error: true
  - content_text_contains: no such file
";

fn write(root: &Path, rel: &str, text: &str) {
    let path = root.join(rel);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, text).unwrap();
}

/// A workspace whose server `files` is the fixture counting its spawns in
/// `count.txt`, with a `prod` secret `api_token`.
fn workspace(files: &[(&str, &str)]) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let count = root.join("count.txt");
    write(
        root,
        "rustman.toml",
        &format!(
            "[workspace]\nname = \"t\"\nversion = 1\n\n[mcp_servers.files]\ncommand = '{FIXTURE}'\nargs = ['--count', '{}']\n",
            count.display()
        ),
    );
    write(root, "environments/prod.yaml", "secrets: [api_token]\n");
    for (rel, text) in files {
        write(root, rel, text);
    }
    let store = SecretStore::with_keychain(root, None);
    store
        .set("prod", &Secret::new("api_token", SECRET.to_owned()))
        .unwrap();
    dir
}

/// Run every request of the workspace at `root`, recording history.
async fn run(root: &Path, trust: bool) -> Vec<RequestRecord> {
    run_with(root, trust, ConnectOptions::default()).await
}

async fn run_with(root: &Path, trust: bool, connect: ConnectOptions) -> Vec<RequestRecord> {
    let ws = Workspace::load(root).unwrap_or_else(|e| panic!("{e}"));
    let env = ws.active_environment(Some("prod"), None).unwrap();
    let store = SecretStore::with_keychain(root, None);
    let mcp = McpRunner::new(trust, connect)
        .with_trust_key(rustman_core::trust::TrustKey::in_dir(root.join(".keys")));
    let options = RunOptions {
        history: Some(HistoryStore::for_workspace(&ws.root, &ws.settings.history)),
        mcp: Some(&mcp),
        ..RunOptions::default()
    };
    let ctx = RunContext {
        root: &ws.root,
        settings: &ws.settings,
        environment: env,
        secrets: &store,
        in_workspace: true,
    };
    let runner = Runner::new(ctx, options).unwrap();
    let plan = plan(&ws, &[], &[]);
    let records = runner.run(&plan).await.requests;
    mcp.shutdown().await;
    records
}

fn history(root: &Path) -> Vec<Value> {
    let dir = root.join(".rustman/history");
    let mut entries: Vec<Value> = fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.extension().is_some_and(|e| e == "json"))
        .map(|p| serde_json::from_str(&fs::read_to_string(p).unwrap()).unwrap())
        .collect();
    entries.sort_by_key(|e| e["id"].as_str().unwrap().to_owned());
    entries
}

fn messages(record: &RequestRecord) -> Vec<String> {
    record.assertions.iter().map(|a| a.to_string()).collect()
}

#[tokio::test]
async fn three_requests_share_one_session_and_one_spawn() {
    let dir = workspace(&[
        ("1-list.rman.yaml", LIST),
        ("2-count.rman.yaml", COUNT),
        ("3-progress.rman.yaml", PROGRESS),
    ]);
    let records = run(dir.path(), true).await;
    assert_eq!(records.len(), 3);
    let count = fs::read_to_string(dir.path().join("count.txt")).unwrap();
    assert_eq!(count.lines().collect::<Vec<_>>(), ["spawn", "initialize"]);
    let (count, progress) = (&records[1], &records[2]);
    assert_eq!(count.status, RequestStatus::Passed, "{:?}", messages(count));
    assert_eq!(count.captures[0].var, "n");
    assert_eq!(
        progress.status,
        RequestStatus::Passed,
        "{:?}",
        messages(progress)
    );
    let mcp = progress.mcp.as_ref().unwrap();
    assert_eq!(mcp.server, "files");
    assert_eq!(mcp.transport, "stdio");
    assert_eq!(mcp.method, "tools/call");
    assert_eq!(mcp.params["arguments"]["n"], "3", "the capture of 2-count");
}

#[tokio::test]
async fn tool_exists_fails_listing_the_available_tools() {
    let dir = workspace(&[("1-list.rman.yaml", LIST)]);
    let records = run(dir.path(), true).await;
    let list = &records[0];
    assert_eq!(list.status, RequestStatus::Failed);
    assert!(list.assertions[0].passed());
    let failure = list.assertions[1].to_string();
    assert!(
        failure.starts_with(
            "tool_exists: expected a tool named \"missing_tool\", got available tools: env, cwd, fail,"
        ),
        "{failure}"
    );
    assert!(failure.contains("search"), "{failure}");
}

#[tokio::test]
async fn output_schema_is_checked_implicitly_and_tool_errors_are_asserted() {
    let dir = workspace(&[("1-bad.rman.yaml", BAD_COUNT), ("2-fail.rman.yaml", FAIL)]);
    let records = run(dir.path(), true).await;
    let (bad, fail) = (&records[0], &records[1]);
    assert_eq!(bad.status, RequestStatus::Failed);
    let implicit = &bad.assertions[0];
    assert_eq!(implicit.kind, "output_schema");
    assert!(
        implicit.message.contains("/count: expected integer"),
        "{}",
        implicit.message
    );
    assert_eq!(fail.status, RequestStatus::Passed, "{:?}", messages(fail));
    assert_eq!(fail.mcp.as_ref().unwrap().outcome, "tool error");
}

#[tokio::test]
async fn history_keeps_the_notifications_and_redacts_the_params() {
    let dir = workspace(&[
        ("2-count.rman.yaml", COUNT),
        ("3-progress.rman.yaml", PROGRESS),
    ]);
    run(dir.path(), true).await;
    let entries = history(dir.path());
    assert_eq!(entries.len(), 2);
    let entry = &entries[1];
    assert_eq!(entry["request"], "3-progress.rman.yaml");
    assert_eq!(entry["sent"]["method"], "tools/call");
    let mcp = &entry["record"]["mcp"];
    assert_eq!(mcp["server"], "files");
    assert_eq!(mcp["transport"], "stdio");
    assert!(mcp["protocol_version"].is_string());
    assert_eq!(mcp["result"]["content"][0]["text"], "done");
    let progress: Vec<&Value> = mcp["notifications"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|n| n["type"] == "progress")
        .collect();
    assert_eq!(progress.len(), 5, "{mcp}");
    assert_eq!(mcp["params"]["arguments"]["token"], "«redacted:api_token»");
    let text = serde_json::to_string(&entries).unwrap();
    assert!(!text.contains(SECRET), "a secret reached history: {text}");
}

#[tokio::test]
async fn untrusted_servers_are_refused_without_trust() {
    let dir = workspace(&[("1-list.rman.yaml", LIST)]);
    let records = run(dir.path(), false).await;
    assert_eq!(records[0].status, RequestStatus::Errored);
    assert_eq!(
        records[0].error.as_deref(),
        Some("mcp: server files is not trusted; review it or pass --trust")
    );
    assert!(
        !dir.path().join("count.txt").exists(),
        "nothing was spawned"
    );
}

/// A workspace for `fake` whose stored OAuth grant has expired: the run
/// refreshes it with `refresh-0`, and `requests` are written as given.
fn oauth_workspace(fake: &McpHttpServer, root: &Path, requests: &[(&str, &str)]) -> ConnectOptions {
    write(
        root,
        "rustman.toml",
        &format!(
            "[workspace]\nname = \"t\"\nversion = 1\n\n[mcp_servers.remote]\nurl = '{}'\n",
            fake.url()
        ),
    );
    for (name, text) in requests {
        write(root, name, text);
    }
    write(root, "environments/prod.yaml", "variables: {}\n");
    let base = fake.url().trim_end_matches("/mcp").to_owned();
    let expired = serde_json::json!({
        "access_token": "stale-access",
        "refresh_token": "refresh-0",
        "expires_at": 1,
        "client": {
            "token_endpoint": format!("{base}/as/token"),
            "client_id": DYNAMIC_CLIENT_ID,
            "resource": fake.url()
        }
    });
    let store = Arc::new(SecretStore::with_keychain(root, None));
    store
        .set(
            NO_ENVIRONMENT,
            &Secret::new(store_key("remote", &fake.url()), expired.to_string()),
        )
        .unwrap();
    fake.accept_refresh_token("refresh-0");
    ConnectOptions {
        oauth: Some(OAuthOptions {
            store,
            prompt: None,
        }),
        ..ConnectOptions::default()
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn an_oauth_call_is_recorded_with_its_authorization_redacted() {
    let fake = McpHttpServer::oauth(true).await;
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let list = "version: 1\nkind: mcp\nname: List\nserver: remote\noperation: tools/list\nassertions:\n  - tool_exists: echo\n";
    let connect = oauth_workspace(&fake, root, &[("list.rman.yaml", list)]);
    let records = run_with(root, false, connect).await;
    assert_eq!(
        records[0].status,
        RequestStatus::Passed,
        "{:?}",
        records[0].error
    );
    let entries = history(root);
    let sent = &entries[0]["sent"];
    assert_eq!(sent["url"], fake.url());
    let authorization: Vec<&Value> = sent["headers"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|h| h["name"] == "Authorization")
        .collect();
    assert_eq!(authorization.len(), 1, "{sent}");
    assert_eq!(authorization[0]["value"], "«redacted:Authorization»");
    assert_eq!(entries[0]["record"]["mcp"]["transport"], "streamable HTTP");
    let text = serde_json::to_string(&entries).unwrap();
    for token in ["access-1", "refresh-0", "refresh-1"] {
        assert!(!text.contains(token), "{token} reached history: {text}");
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn oauth_tokens_are_redacted_in_their_encoded_forms() {
    let fake = McpHttpServer::oauth(true).await;
    fake.issue_access_tokens_like("zq/x+");
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    // The token the refresh issues is `zq/x+1`: the call sends (and the
    // server echoes) it percent-encoded, JSON-escaped and as HTML entities.
    let echo = "version: 1\nkind: mcp\nname: Echo\nserver: remote\noperation: tools/call\n\
                params:\n  name: echo\n  arguments:\n    text: 'pct=zq%2Fx%2B1 lower=zq%2fx%2b1 json=zq\\/x+1 html=zq&#47;x&#43;1'\n";
    let connect = oauth_workspace(&fake, root, &[("echo.rman.yaml", echo)]);
    let records = run_with(root, false, connect).await;
    assert_eq!(
        records[0].status,
        RequestStatus::Passed,
        "{:?}",
        records[0].error
    );
    assert!(bearers_sent(&fake).iter().any(|b| b == "Bearer zq/x+1"));
    let shown = serde_json::to_string(&records).unwrap();
    let text = serde_json::to_string(&history(root)).unwrap();
    for output in [&shown, &text] {
        assert!(!output.contains("zq"), "an encoded token leaked: {output}");
    }
}

fn bearers_sent(fake: &McpHttpServer) -> Vec<String> {
    fake.hits()
        .iter()
        .filter_map(|h| h.header("authorization").map(str::to_owned))
        .collect()
}

const SNAPSHOT: &str = "\
version: 1
kind: mcp
name: Snapshot
server: files
operation: resources/read
params:
  uri: file:///README.md
assertions:
  - snapshot
";

#[tokio::test]
async fn a_snapshot_assertion_records_then_compares_the_result() {
    let dir = workspace(&[("snap.rman.yaml", SNAPSHOT)]);
    let first = run(dir.path(), true).await;
    assert_eq!(
        first[0].status,
        RequestStatus::Passed,
        "{:?}",
        messages(&first[0])
    );
    let file = dir.path().join("__snapshots__/snap.default.snap.yaml");
    let stored = fs::read_to_string(&file).unwrap();
    assert!(stored.contains("kind: mcp"), "{stored}");
    assert!(stored.contains("file:///README.md"), "{stored}");
    let second = run(dir.path(), true).await;
    assert_eq!(
        second[0].status,
        RequestStatus::Passed,
        "{:?}",
        messages(&second[0])
    );
    // A changed result fails the comparison.
    fs::write(
        &file,
        stored.replace("file:///README.md", "file:///OTHER.md"),
    )
    .unwrap();
    let third = run(dir.path(), true).await;
    assert_eq!(
        third[0].status,
        RequestStatus::Failed,
        "{:?}",
        messages(&third[0])
    );
}
