//! `rustman explore plan` (task 3.3): a dry run that discovers and plans
//! against the `bad-api` fixture while it records every request, and must
//! record none.

mod common;

use std::path::Path;

use common::{rustman, write};
use rustman_test_support::bad_api::{BadApi, OPENAPI};

/// `bad-api` and the runtime it is served from.
struct Api {
    // Dropped before the runtime, which the server's task runs on.
    server: BadApi,
    _runtime: tokio::runtime::Runtime,
}

impl Api {
    fn start() -> Self {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .unwrap();
        let server = runtime.block_on(BadApi::start());
        Self {
            server,
            _runtime: runtime,
        }
    }
}

/// A workspace with the document at `api.yaml`, a `dev` environment and a
/// `production` one.
fn workspace() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    write(
        dir.path(),
        "rustman.toml",
        "[workspace]\nname = \"t\"\nversion = 1\n",
    );
    write(dir.path(), "api.yaml", OPENAPI);
    write(
        dir.path(),
        "environments/dev.yaml",
        "variables:\n  tenant: a\n",
    );
    write(
        dir.path(),
        "environments/production.yaml",
        "variables:\n  tenant: a\n",
    );
    dir
}

fn plan(dir: &Path, api: &Api, extra: &[&str]) -> std::process::Output {
    let target = api.server.url("");
    let mut args = vec![
        "explore",
        "plan",
        "--spec",
        "api.yaml",
        "--target",
        target.as_str(),
    ];
    args.extend_from_slice(extra);
    rustman(dir).args(&args).output().unwrap()
}

#[test]
fn plan_sends_nothing_and_lists_blocked_and_allowed_probes() {
    let api = Api::start();
    let ws = workspace();
    let output = plan(ws.path(), &api, &["--seed", "1234"]);
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        api.server.hits().is_empty(),
        "the plan sent {:?}",
        api.server.hits()
    );
    assert!(stdout.starts_with("plan: "), "{stdout}");
    assert!(stdout.contains("seed 1234"), "{stdout}");
    assert!(stdout.contains("ID     FAMILY"), "{stdout}");
    assert!(
        stdout.contains("conformance         POST /users"),
        "{stdout}"
    );
    assert!(
        stdout.contains("blocked: mutations not allowed"),
        "{stdout}"
    );
    assert!(
        stdout.contains("blocked: destructive not allowed"),
        "{stdout}"
    );
    assert!(stdout.contains("skipped: no producer"), "{stdout}");
    assert!(stdout.contains(" allowed\n"), "{stdout}");
}

#[test]
fn json_output_has_every_planned_probe() {
    let api = Api::start();
    let ws = workspace();
    let output = plan(
        ws.path(),
        &api,
        &[
            "--json",
            "--graph",
            "--seed",
            "7",
            "--probes",
            "conformance,auth",
        ],
    );
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let json: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(json["seed"], 7);
    let probes = json["probes"].as_array().unwrap();
    assert!(!probes.is_empty());
    for probe in probes {
        for key in [
            "id",
            "family",
            "operation",
            "method",
            "expected",
            "disposition",
        ] {
            assert!(probe.get(key).is_some(), "{key} missing in {probe}");
        }
        let family = probe["family"].as_str().unwrap();
        assert!(family == "conformance" || family == "auth", "{family}");
    }
    let create = probes
        .iter()
        .find(|p| p["family"] == "conformance" && p["operation"] == "POST /users")
        .unwrap();
    assert_eq!(
        create["disposition"],
        serde_json::json!({"state": "blocked", "reason": "mutations not allowed"})
    );
    let edges = json["graph"]["edges"].as_array().unwrap();
    assert!(
        edges
            .iter()
            .any(|e| e["consumer"] == "GET /users/{id}" && e["producer"] == "POST /users")
    );
    assert!(api.server.hits().is_empty());
}

#[test]
fn the_same_seed_prints_the_same_plan() {
    let api = Api::start();
    let ws = workspace();
    let args = [
        "--json",
        "--seed",
        "42",
        "--probes",
        "fuzz",
        "--allow-mutations",
        "--fuzz-cases",
        "6",
    ];
    let first = plan(ws.path(), &api, &args);
    let second = plan(ws.path(), &api, &args);
    assert!(first.status.success());
    assert_eq!(first.stdout, second.stdout);
}

#[test]
fn the_graph_is_printed_on_request() {
    let api = Api::start();
    let ws = workspace();
    let output = plan(ws.path(), &api, &["--graph", "--probes", "conformance"]);
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(
        stdout.contains("\ngraph:\nGET /users/{id}: id <- POST /users /id (heuristic)\n"),
        "{stdout}"
    );
    assert!(
        stdout.contains("GET /reports/{reportId}: reportId <- no producer"),
        "{stdout}"
    );
}

#[test]
fn a_production_environment_is_refused_with_exit_4() {
    let api = Api::start();
    let ws = workspace();
    let output = plan(ws.path(), &api, &["--env", "production"]);
    assert_eq!(output.status.code(), Some(4));
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(
        stderr.contains("refusing to send requests to production host '127.0.0.1'"),
        "{stderr}"
    );
    let confirmed = plan(
        ws.path(),
        &api,
        &["--env", "production", "--production", "127.0.0.1"],
    );
    assert!(
        confirmed.status.success(),
        "{}",
        String::from_utf8_lossy(&confirmed.stderr)
    );
    let destructive = plan(
        ws.path(),
        &api,
        &[
            "--env",
            "production",
            "--production",
            "127.0.0.1",
            "--allow-destructive",
        ],
    );
    assert_eq!(destructive.status.code(), Some(4));
    assert!(api.server.hits().is_empty());
}

#[test]
fn bad_input_exits_2() {
    let api = Api::start();
    let ws = workspace();
    let unknown = plan(ws.path(), &api, &["--probes", "conformance,nope"]);
    assert_eq!(unknown.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&unknown.stderr).contains("unknown probe family 'nope'"));
    let identity = plan(ws.path(), &api, &["--identity-b", "missing"]);
    assert_eq!(identity.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&identity.stderr).contains("environment 'missing' not found"));
    let target = rustman(ws.path())
        .args([
            "explore",
            "plan",
            "--spec",
            "api.yaml",
            "--target",
            "not a url",
        ])
        .output()
        .unwrap();
    assert_eq!(target.status.code(), Some(2));
}

#[test]
fn variable_values_never_reach_the_plan() {
    let api = Api::start();
    let ws = workspace();
    let canary = "canary-report-7f3a9c";
    write(
        ws.path(),
        "environments/dev.yaml",
        &format!("variables:\n  reportId: {canary}\n"),
    );
    for format in [&["--json"][..], &[][..]] {
        let mut args = vec!["--env", "dev", "--probes", "conformance"];
        args.extend_from_slice(format);
        let output = plan(ws.path(), &api, &args);
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let stdout = String::from_utf8(output.stdout).unwrap();
        assert!(!stdout.contains(canary), "{stdout}");
        assert!(!String::from_utf8_lossy(&output.stderr).contains(canary));
        if !format.is_empty() {
            assert!(stdout.contains("/reports/{{reportId}}"), "{stdout}");
        }
    }
}
