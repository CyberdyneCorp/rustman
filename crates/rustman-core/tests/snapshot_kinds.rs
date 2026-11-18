//! Snapshot testing over every request kind (snapshot-testing task 7.1):
//! record, a mutated response fails in CI mode, update fixes it, and CI
//! mode then matches. Snapshots only see the shared exchange model, so the
//! kinds without a client yet (`jsonrpc`, `mcp`) are driven with stub
//! exchanges: a JSON-RPC response body and an MCP event list. `http` and
//! `graphql` also run end to end through `rustman run` in
//! `crates/rustman-cli/tests/snapshot_e2e.rs`.

use std::fs;
use std::path::Path;

use rustman_core::Redactor;
use rustman_core::model::{Checked, SnapshotAssertion};
use rustman_core::snapshot::{
    Exchange, SnapshotContext, SnapshotMode, SnapshotOutcome, SnapshotState, check,
};
use serde_json::{Value, json};

/// A stubbed response: a body, or an event list.
struct Stub {
    kind: &'static str,
    status: Option<u16>,
    body: Vec<u8>,
    events: Option<Vec<Value>>,
}

impl Stub {
    fn body(kind: &'static str, status: Option<u16>, body: &Value) -> Self {
        Self {
            kind,
            status,
            body: serde_json::to_vec(body).unwrap(),
            events: None,
        }
    }

    fn events(kind: &'static str, events: Vec<Value>) -> Self {
        Self {
            kind,
            status: None,
            body: Vec::new(),
            events: Some(events),
        }
    }
}

const HEADERS: &[(&str, &str)] = &[("Content-Type", "application/json")];

fn spec() -> SnapshotAssertion {
    let checked: Checked<SnapshotAssertion> =
        rustman_core::format::from_yaml(Path::new("s.yaml"), "{}").unwrap();
    checked.into_inner()
}

fn check_stub(root: &Path, request: &str, stub: &Stub, mode: SnapshotMode) -> SnapshotOutcome {
    let headers: Vec<(String, String)> = HEADERS
        .iter()
        .map(|(n, v)| ((*n).to_owned(), (*v).to_owned()))
        .collect();
    let exchange = Exchange {
        kind: stub.kind,
        status: stub.status,
        headers: &headers,
        body: &stub.body,
        events: stub.events.as_deref(),
    };
    let request_file = root.join(request);
    let ctx = SnapshotContext {
        request_file: &request_file,
        request,
        mode,
        redactor: &Redactor::new(),
    };
    check(&spec(), &exchange, &ctx).unwrap()
}

/// Record `before`, fail on `after` in CI mode, update to `after`, match it
/// in CI mode. Returns the failure.
fn cycle(request: &str, before: &Stub, after: &Stub) -> SnapshotOutcome {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    let recorded = check_stub(root, request, before, SnapshotMode::Record);
    assert_eq!(recorded.state, SnapshotState::Recorded);
    let stem = Path::new(request).file_name().unwrap().to_string_lossy();
    let stem = stem.trim_end_matches(".rman.yaml");
    let folder = request.rsplit_once('/').map_or("", |(dir, _)| dir);
    let expected = format!("{folder}/__snapshots__/{stem}.default.snap.yaml");
    assert_eq!(recorded.display_path, expected, "paths use '/'");
    assert!(recorded.path.is_file(), "{}", recorded.path.display());
    assert!(recorded.path.ends_with(Path::new(&expected)));
    let text = fs::read_to_string(&recorded.path).unwrap();
    assert!(text.contains(&format!("kind: {}", before.kind)), "{text}");

    let failed = check_stub(root, request, after, SnapshotMode::Ci);
    assert_eq!(failed.state, SnapshotState::Failed, "{}", before.kind);
    assert!(!failed.message.is_empty());
    assert_eq!(
        fs::read_to_string(&recorded.path).unwrap(),
        text,
        "CI writes nothing"
    );

    let updated = check_stub(root, request, after, SnapshotMode::Update);
    assert_eq!(updated.state, SnapshotState::Updated);
    let matched = check_stub(root, request, after, SnapshotMode::Ci);
    assert_eq!(matched.state, SnapshotState::Matched, "{}", before.kind);
    failed
}

#[test]
fn http_body_snapshots_record_fail_and_update() {
    let failed = cycle(
        "api/users/get-user.rman.yaml",
        &Stub::body("http", Some(200), &json!({"id": 1, "role": "admin"})),
        &Stub::body("http", Some(200), &json!({"id": 1, "role": "viewer"})),
    );
    assert!(
        failed.message.contains("changed $.role"),
        "{}",
        failed.message
    );
}

#[test]
fn graphql_result_and_subscription_snapshots() {
    let failed = cycle(
        "api/graph/user.rman.yaml",
        &Stub::body(
            "graphql",
            Some(200),
            &json!({"data": {"user": {"name": "Ada"}}}),
        ),
        &Stub::body(
            "graphql",
            Some(200),
            &json!({"data": {"user": {"name": "Bob"}}}),
        ),
    );
    assert!(
        failed.message.contains("$.data.user.name"),
        "{}",
        failed.message
    );
    let tick = |n: u64| json!({"data": {"tick": n}});
    let failed = cycle(
        "api/graph/ticks.rman.yaml",
        &Stub::events("graphql", vec![tick(1), tick(2), tick(3)]),
        &Stub::events("graphql", vec![tick(1), tick(2)]),
    );
    assert!(
        failed.message.contains("event count changed: 3 → 2"),
        "{}",
        failed.message
    );
}

#[test]
fn jsonrpc_stub_body_snapshots() {
    let response =
        |balance: u64| json!({"jsonrpc": "2.0", "id": 1, "result": {"balance": balance}});
    let failed = cycle(
        "rpc/get-balance.rman.yaml",
        &Stub::body("jsonrpc", Some(200), &response(10)),
        &Stub::body("jsonrpc", Some(200), &response(12)),
    );
    assert!(
        failed.message.contains("$.result.balance"),
        "{}",
        failed.message
    );
}

#[test]
fn mcp_stub_event_list_snapshots() {
    let progress = |p: u64| json!({"method": "notifications/progress", "params": {"progress": p}});
    let result = |text: &str| json!({"result": {"content": [{"type": "text", "text": text}]}});
    let failed = cycle(
        "mcp/tools/search.rman.yaml",
        &Stub::events("mcp", vec![progress(50), progress(100), result("3 hits")]),
        &Stub::events("mcp", vec![progress(100), result("4 hits")]),
    );
    assert!(
        failed.message.contains("event count changed: 3 → 2"),
        "{}",
        failed.message
    );
}
