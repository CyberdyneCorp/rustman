//! Agents read bench reports (add-load-testing "Agents cannot start a
//! bench", task 5.1): `get_report` takes a report file of the workspace,
//! summarizes and renders a `kind: bench` report, and refuses paths that
//! leave the workspace. No tool starts a bench (see `tools/mod.rs`).

use std::path::Path;

use rmcp::model::{CallToolRequestParams, CallToolResult};
use rmcp::service::RunningService;
use rmcp::{RoleClient, ServiceExt};
use rustman_core::{SecretStore, Workspace};
use rustman_mcp::server::Sessions;
use rustman_report::bench::{
    BenchProfile, BenchReport, BenchTotals, KIND_BENCH, LatencyKind, LatencySummary, StatsSummary,
    ThresholdResult,
};
use rustman_report::{Report, SCHEMA};
use serde_json::{Value, json};

type Client = RunningService<RoleClient, ()>;

fn write(root: &Path, rel: &str, text: &str) {
    let path = root.join(rel);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, text).unwrap();
}

fn report() -> Report {
    Report::Bench(Box::new(BenchReport {
        schema: SCHEMA.into(),
        kind: KIND_BENCH.into(),
        rustman_version: "0".into(),
        started_at: "2026-11-19T10:00:00Z".into(),
        finished_at: "2026-11-19T10:00:05Z".into(),
        target: "http://127.0.0.1:9".into(),
        environment: None,
        profile: BenchProfile {
            executor: "constant-vus".into(),
            vus: Some(2),
            ..BenchProfile::default()
        },
        aborted: None,
        totals: BenchTotals {
            latency_kind: LatencyKind::Service,
            duration_ms: 5000.0,
            iterations: 10,
            iteration_duration: LatencySummary::default(),
            interrupted: 0,
            scheduled: 0,
            dropped: 0,
            rps: 2.0,
            generator_lag_p99_ms: 0.5,
        },
        overall: StatsSummary {
            requests: 10,
            ..StatsSummary::default()
        },
        files: Vec::new(),
        series: Vec::new(),
        thresholds: vec![ThresholdResult {
            expression: "p95<300ms".into(),
            passed: false,
            actual: "412ms".into(),
            abort: false,
        }],
        error_samples: Vec::new(),
        warnings: Vec::new(),
    }))
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

async fn call(client: &Client, arguments: Value) -> CallToolResult {
    let params = CallToolRequestParams::new("get_report")
        .with_arguments(arguments.as_object().unwrap().clone());
    client.call_tool(params).await.unwrap()
}

#[tokio::test(flavor = "multi_thread")]
async fn get_report_reads_a_bench_report_file() {
    let dir = tempfile::tempdir().unwrap();
    write(
        dir.path(),
        "rustman.toml",
        "[workspace]\nname = \"t\"\nversion = 1\n",
    );
    write(dir.path(), ".gitignore", ".rustman/\n");
    write(dir.path(), "out/bench.json", &report().to_json());
    let client = connect(dir.path()).await;

    let summary = call(&client, json!({"id": "out/bench.json"})).await;
    assert_eq!(summary.is_error, Some(false), "{summary:?}");
    let summary = summary.structured_content.unwrap();
    assert_eq!(summary["kind"], "bench");
    assert_eq!(summary["summary"]["passed"], false);
    assert_eq!(summary["summary"]["thresholds"][0]["actual"], "412ms");

    let markdown = call(
        &client,
        json!({"id": "out/bench.json", "format": "markdown"}),
    )
    .await;
    let text = markdown.structured_content.unwrap()["text"]
        .as_str()
        .unwrap()
        .to_owned();
    assert!(text.starts_with("# Bench report\n"), "{text}");

    for bad in ["../bench.json", "missing.json"] {
        let refused = call(&client, json!({ "id": bad })).await;
        assert_eq!(refused.is_error, Some(true), "{bad}");
    }
    let _ = client.cancel().await;
}

/// A report file is read only inside the workspace, up to a size, and a
/// file that is not a report is refused without quoting it.
#[tokio::test(flavor = "multi_thread")]
async fn get_report_refuses_odd_report_files() {
    let dir = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    write(
        dir.path(),
        "rustman.toml",
        "[workspace]\nname = \"t\"\nversion = 1\n",
    );
    write(dir.path(), ".gitignore", ".rustman/\n");
    write(
        dir.path(),
        "out/odd.json",
        r#"{"schema": "canary-q8Zt-not-a-schema", "kind": "canary-q8Zt"}"#,
    );
    let big = std::fs::File::create(dir.path().join("out/big.json")).unwrap();
    big.set_len(33 * 1024 * 1024).unwrap();
    write(outside.path(), "bench.json", &report().to_json());
    #[cfg(unix)]
    std::os::unix::fs::symlink(
        outside.path().join("bench.json"),
        dir.path().join("out/link.json"),
    )
    .unwrap();
    let client = connect(dir.path()).await;

    let odd = call(&client, json!({"id": "out/odd.json"})).await;
    assert_eq!(odd.is_error, Some(true));
    let text = format!("{odd:?}");
    assert!(text.contains("is not a rustman report"), "{text}");
    assert!(!text.contains("canary-q8Zt"), "{text}");

    let big = call(&client, json!({"id": "out/big.json"})).await;
    assert_eq!(big.is_error, Some(true));
    assert!(format!("{big:?}").contains("over 32 MiB"), "{big:?}");

    #[cfg(unix)]
    {
        let link = call(&client, json!({"id": "out/link.json"})).await;
        assert_eq!(link.is_error, Some(true));
        assert!(
            format!("{link:?}").contains("not a path inside the workspace"),
            "{link:?}"
        );
    }
    let _ = client.cancel().await;
}
