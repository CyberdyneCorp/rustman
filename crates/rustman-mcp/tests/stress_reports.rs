//! Agents read stress reports (add-stress-testing "Agents cannot start
//! stress runs", task 6.5): `get_report` summarizes and renders a
//! `kind: stress` report file of the workspace. No tool starts a stress
//! run (see `tools/mod.rs`).

use std::path::Path;

use rmcp::model::{CallToolRequestParams, CallToolResult};
use rmcp::service::RunningService;
use rmcp::{RoleClient, ServiceExt};
use rustman_core::{SecretStore, Workspace};
use rustman_mcp::server::Sessions;
use serde_json::{Value, json};

type Client = RunningService<RoleClient, ()>;

fn write(root: &Path, rel: &str, text: &str) {
    let path = root.join(rel);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, text).unwrap();
}

/// A stress report with a `no-recovery` finding.
fn report() -> Value {
    json!({
        "schema": "rustman.report/v1",
        "kind": "stress",
        "rustman_version": "0",
        "id": "str-20261120-000001",
        "started_at": "2026-11-20T10:00:00Z",
        "finished_at": "2026-11-20T10:01:00Z",
        "target": "http://127.0.0.1:9",
        "environment": null,
        "mode": "breakpoint",
        "profile": {"settings": {"start_rps": 10.0}},
        "seed": 7,
        "slos": ["p95<1s"],
        "overall": {
            "requests": 10, "error_rate": 0.0, "errors": {},
            "latency": {"min_ms": 0.0, "avg_ms": 0.0, "p50_ms": 0.0, "p90_ms": 0.0, "p95_ms": 0.0, "p99_ms": 0.0, "max_ms": 0.0},
            "checks": {"passed": 0, "failed": 0}, "phases": {}, "bytes_sent": 0, "bytes_received": 0
        },
        "files": [],
        "series": [],
        "steps": [],
        "breakpoint": {
            "max_sustainable_rps": 40.0, "breaking_rps": 48.0, "failure_mode": "latency",
            "first_failing_endpoint": null, "stop": "refined", "max_rps": 500.0
        },
        "rate_limited_at": null,
        "killed": null,
        "recovery": {
            "status": "not-recovered", "after_ms": null, "baseline_ms": 3.0, "health": "api/h.rman.yaml",
            "last_status": 503, "last_latency_ms": 4.0, "timeline": []
        },
        "summary": {"findings": {"critical": 0, "high": 1, "medium": 0, "low": 0, "info": 0}, "requests": 10, "fuzzed_requests": 0, "duration_ms": 60000},
        "findings": [{
            "id": "f001", "rule": "no-recovery", "severity": "high", "operation": "api/h.rman.yaml",
            "description": "d", "message": "api/h.rman.yaml had not recovered 2m after load stopped",
            "occurrences": 1, "probes": [],
            "evidence": {"method": "", "url": "", "request_headers": [], "status": 0, "response_headers": [], "body": ""}
        }]
    })
}

async fn connect(root: &Path) -> Client {
    rustman_core::secrets::workspace_id::load_or_create(root).unwrap();
    let ws = Workspace::load(root).unwrap();
    let sessions = Sessions::new(ws, SecretStore::with_keychain(root, None), None);
    let server = sessions.open();
    let (server_io, client_io) = tokio::io::duplex(1 << 20);
    tokio::spawn(async move {
        let service = server.serve(server_io).await.unwrap();
        let _ = service.waiting().await;
    });
    ().serve(client_io).await.unwrap()
}

async fn call(client: &Client, tool: &str, arguments: Value) -> CallToolResult {
    let params = CallToolRequestParams::new(tool.to_owned())
        .with_arguments(arguments.as_object().unwrap().clone());
    client.call_tool(params).await.unwrap()
}

#[tokio::test(flavor = "multi_thread")]
async fn get_report_reads_a_stress_report_and_no_tool_starts_one() {
    let dir = tempfile::tempdir().unwrap();
    write(
        dir.path(),
        "rustman.toml",
        "[workspace]\nname = \"t\"\nversion = 1\n",
    );
    write(dir.path(), ".gitignore", ".rustman/\n");
    write(dir.path(), "out/stress.json", &report().to_string());
    let client = connect(dir.path()).await;

    let tools = client.list_all_tools().await.unwrap();
    for tool in &tools {
        assert!(!tool.name.contains("stress"), "{}", tool.name);
    }

    let summary = call(&client, "get_report", json!({"id": "out/stress.json"})).await;
    assert_eq!(summary.is_error, Some(false), "{summary:?}");
    let summary = summary.structured_content.unwrap();
    assert_eq!(summary["kind"], "stress");
    assert_eq!(summary["summary"]["breakpoint"]["breaking_rps"], 48.0);
    assert_eq!(summary["summary"]["recovery"]["status"], "not-recovered");
    assert_eq!(summary["summary"]["findings"][0]["rule"], "no-recovery");

    let markdown = call(
        &client,
        "get_report",
        json!({"id": "out/stress.json", "format": "markdown"}),
    )
    .await;
    let text = markdown.structured_content.unwrap()["text"]
        .as_str()
        .unwrap()
        .to_owned();
    assert!(text.starts_with("# Stress report\n"), "{text}");
    assert!(text.contains("`no-recovery`"), "{text}");
}
