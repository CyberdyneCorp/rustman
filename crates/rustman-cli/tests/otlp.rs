//! OTLP export from the `rustman` binary (add-trace-propagation tasks 4.1,
//! 4.3 and 5.1's in-process variant): a run against the loopback test
//! server, exporting to the in-process collector. Built with `--features
//! otlp` only.
#![cfg(feature = "otlp")]

mod common;

use std::path::Path;
use std::time::Duration;

use common::{Server, closed_port, request, rustman, workspace, write};
use rustman_test_support::otlp::{CollectedSpan, OtlpCollector};

/// The collector and the runtime it is served from.
struct Collector {
    collector: OtlpCollector,
    runtime: tokio::runtime::Runtime,
}

impl Collector {
    fn start() -> Self {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .enable_all()
            .build()
            .unwrap();
        let collector = runtime.block_on(OtlpCollector::start());
        Self { collector, runtime }
    }

    fn spans(&self, count: usize) -> Vec<CollectedSpan> {
        let wait = self
            .collector
            .wait_for_spans(count, Duration::from_secs(20));
        self.runtime.block_on(wait)
    }
}

fn settings(ws: &Path, toml: &str) {
    write(
        ws,
        "rustman.toml",
        &format!("[workspace]\nname = \"t\"\nversion = 1\n{toml}"),
    );
}

fn export(endpoint: &str) -> String {
    format!(
        "[trace]\nenabled = true\nscope = \"run\"\n\n[trace.export]\nenabled = true\nendpoint = \"{endpoint}\"\n"
    )
}

fn requests(ws: &Path, count: usize, url: &str, extra: &str) {
    for i in 0..count {
        let name = format!("r{i:02}");
        write(
            ws,
            &format!("{name}.rman.yaml"),
            &request(&name, url, extra),
        );
    }
}

#[test]
fn a_run_scoped_trace_reaches_the_server_and_the_collector() {
    let server = Server::http();
    let collector = Collector::start();
    let ws = workspace(&server);
    settings(ws.path(), &export(&collector.collector.endpoint()));
    requests(ws.path(), 3, "{{base_url}}/echo?token=abc", "");
    let output = rustman(ws.path())
        .args(["run", "--env", "dev", "--reporter", "json"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(0), "{output:?}");
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    let requests = report["requests"].as_array().unwrap();
    let trace_id = requests[0]["trace"]["trace_id"]
        .as_str()
        .unwrap()
        .to_owned();
    let spans = collector.spans(3);
    assert_eq!(spans.len(), 3, "{spans:?}");
    for request in requests {
        let trace = &request["trace"];
        assert_eq!(trace["trace_id"], trace_id.as_str(), "scope: run");
        let span = spans
            .iter()
            .find(|s| s.span_id == trace["span_id"].as_str().unwrap())
            .expect("a client span per exchange");
        assert_eq!(
            (span.trace_id.as_str(), span.kind),
            (trace_id.as_str(), CollectedSpan::CLIENT)
        );
        let url = span.attribute("url.full").unwrap();
        assert!(url.ends_with("?token=«redacted:token»"), "{url}");
        // The server saw the same trace: the echoed `traceparent`.
        let echo: serde_json::Value =
            serde_json::from_str(request["exchange"]["response"]["body"].as_str().unwrap())
                .unwrap();
        let traceparent = echo["headers"]
            .as_array()
            .unwrap()
            .iter()
            .find(|h| h[0] == "traceparent")
            .map(|h| h[1].as_str().unwrap().to_owned())
            .unwrap();
        assert_eq!(traceparent, format!("00-{trace_id}-{}-01", span.span_id));
    }
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(!stderr.contains("warning"), "{stderr}");
}

#[test]
fn otel_variables_are_ignored_without_use_env() {
    let server = Server::http();
    let collector = Collector::start();
    let ws = workspace(&server);
    settings(ws.path(), "[trace]\nenabled = true\n");
    requests(ws.path(), 1, "{{base_url}}/ok", "");
    let endpoint = collector.collector.endpoint();
    rustman(ws.path())
        .args(["run", "--env", "dev"])
        .env("OTEL_EXPORTER_OTLP_ENDPOINT", &endpoint)
        .env(
            "OTEL_EXPORTER_OTLP_TRACES_ENDPOINT",
            format!("{endpoint}/v1/traces"),
        )
        .assert()
        .success();
    // Export enabled to a collector that is down, with the variables
    // pointing at the live one: they are still not read.
    settings(
        ws.path(),
        &export(&format!("http://127.0.0.1:{}", closed_port())),
    );
    rustman(ws.path())
        .args(["run", "--env", "dev"])
        .env("OTEL_EXPORTER_OTLP_ENDPOINT", &endpoint)
        .assert()
        .success();
    // Each run flushed its spans before exiting, and the collector records
    // a request before answering it: anything exported is already here.
    assert!(collector.collector.hits().is_empty());
    // With use_env = true the variable wins.
    let toml = format!(
        "{}use_env = true\n",
        export(&format!("http://127.0.0.1:{}", closed_port()))
    );
    settings(ws.path(), &toml);
    rustman(ws.path())
        .args(["run", "--env", "dev"])
        .env("OTEL_EXPORTER_OTLP_ENDPOINT", &endpoint)
        .assert()
        .success();
    assert_eq!(collector.spans(1).len(), 1);
    assert_eq!(collector.collector.hits().len(), 1);
}

/// Ten requests with the collector down: every request completes, the exit
/// code follows the assertions alone, and one warning is printed.
#[test]
fn a_collector_that_is_down_only_warns_once() {
    let server = Server::http();
    let ws = workspace(&server);
    settings(
        ws.path(),
        &export(&format!("http://127.0.0.1:{}", closed_port())),
    );
    requests(ws.path(), 10, "{{base_url}}/ok", "");
    let output = rustman(ws.path())
        .args(["run", "--env", "dev", "--reporter", "json"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(0), "{output:?}");
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["summary"]["passed"], 10, "{report}");
    let stderr = String::from_utf8(output.stderr).unwrap();
    let warnings: Vec<&str> = stderr
        .lines()
        .filter(|l| l.contains("trace export to 127.0.0.1:"))
        .collect();
    assert_eq!(warnings.len(), 1, "{stderr}");
    assert!(warnings[0].starts_with("warning: "), "{stderr}");
    // A failing assertion still decides the exit code.
    write(
        ws.path(),
        "r00.rman.yaml",
        &request("r00", "{{base_url}}/ok", "assertions:\n  - status: 500\n"),
    );
    rustman(ws.path())
        .args(["run", "--env", "dev"])
        .assert()
        .code(1);
}

/// The URLs of the `trace-jaeger` CI job: Jaeger's UI and API, its OTLP
/// gRPC receiver, and an API instrumented with OpenTelemetry that exports
/// to the same Jaeger (HotROD).
struct JaegerSetup {
    ui: String,
    otlp: String,
    api: String,
}

fn jaeger_setup() -> Option<JaegerSetup> {
    let var = |name: &str| std::env::var(name).ok().filter(|v| !v.is_empty());
    Some(JaegerSetup {
        ui: var("RUSTMAN_JAEGER_URL")?,
        otlp: var("RUSTMAN_JAEGER_OTLP")?,
        api: var("RUSTMAN_TRACED_API_URL")?,
    })
}

/// The services of trace `trace_id` in Jaeger, once it has `want` of them
/// (Jaeger and the API export in batches) or 90 seconds passed.
fn jaeger_services(ws: &Path, ui: &str, trace_id: &str, want: &[&str]) -> Vec<String> {
    let mut services = Vec::new();
    for _ in 0..90 {
        let output = rustman(ws)
            .args(["send", &format!("{ui}/api/traces/{trace_id}")])
            .args(["--body-only", "--no-history"])
            .output()
            .unwrap();
        let body: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap_or_default();
        services = body["data"][0]["processes"]
            .as_object()
            .map(|p| {
                p.values()
                    .filter_map(|p| p["serviceName"].as_str().map(str::to_owned))
                    .collect()
            })
            .unwrap_or_default();
        if want.iter().all(|w| services.iter().any(|s| s == w)) {
            break;
        }
        std::thread::sleep(Duration::from_secs(1));
    }
    services
}

/// Task 5.1: `rustman run` with `scope: run` against HotROD, exporting to
/// Jaeger all-in-one. Ignored by default; the `trace-jaeger` CI job runs it
/// with `--ignored` and sets the URLs, and without them it fails.
#[test]
#[ignore = "needs Jaeger and HotROD (the trace-jaeger CI job)"]
fn jaeger_shows_rustman_and_the_api_in_one_trace() {
    let setup = jaeger_setup()
        .expect("RUSTMAN_JAEGER_URL, RUSTMAN_JAEGER_OTLP and RUSTMAN_TRACED_API_URL must be set");
    let ws = tempfile::tempdir().unwrap();
    settings(
        ws.path(),
        &format!(
            "[trace]\nenabled = true\nscope = \"run\"\nlink = \"jaeger\"\nlink_base = \"{}\"\n\n[trace.export]\nenabled = true\nendpoint = \"{}\"\nservice_name = \"rustman-e2e\"\n",
            setup.ui, setup.otlp
        ),
    );
    write(
        ws.path(),
        "environments/ci.yaml",
        &format!("variables:\n  base_url: {}\n", setup.api),
    );
    write(ws.path(), ".gitignore", ".rustman/\n");
    for (i, customer) in ["123", "392", "731"].iter().enumerate() {
        let url = format!("{{{{base_url}}}}/dispatch?customer={customer}");
        let name = format!("dispatch-{i}");
        write(
            ws.path(),
            &format!("flow/{name}.rman.yaml"),
            &request(&name, &url, "assertions:\n  - status: 200\n"),
        );
    }
    let output = rustman(ws.path())
        .args(["run", "flow", "--env", "ci", "--reporter", "json"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(0), "{output:?}");
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    let trace = &report["requests"][0]["trace"];
    let trace_id = trace["trace_id"].as_str().unwrap().to_owned();
    for request in report["requests"].as_array().unwrap() {
        assert_eq!(request["trace"]["trace_id"], trace_id.as_str());
    }
    let services = jaeger_services(
        ws.path(),
        &setup.ui,
        &trace_id,
        &["rustman-e2e", "frontend"],
    );
    assert!(
        services.iter().any(|s| s == "rustman-e2e") && services.iter().any(|s| s == "frontend"),
        "trace {trace_id} has {services:?}"
    );
    assert_eq!(
        trace["link"].as_str().unwrap(),
        format!("{}/trace/{trace_id}", setup.ui)
    );
    // The link `rustman send` prints on stderr points at a trace Jaeger has.
    // Jaeger's UI is a single-page app that answers 200 for any
    // `/trace/<id>`, so the proof is the trace in `/api/traces/<id>`.
    let sent = rustman(ws.path())
        .args([
            "send",
            "flow/dispatch-0.rman.yaml",
            "--env",
            "ci",
            "--no-history",
        ])
        .output()
        .unwrap();
    assert_eq!(sent.status.code(), Some(0), "{sent:?}");
    let stderr = String::from_utf8(sent.stderr).unwrap();
    let link = stderr
        .lines()
        .filter_map(|l| l.strip_prefix("trace: "))
        .find(|l| l.starts_with(&setup.ui))
        .unwrap_or_else(|| panic!("no printed link in {stderr}"))
        .to_owned();
    let linked = link
        .strip_prefix(&format!("{}/trace/", setup.ui))
        .unwrap_or_else(|| panic!("{link} is not a Jaeger trace link"));
    let services = jaeger_services(ws.path(), &setup.ui, linked, &["rustman-e2e"]);
    assert!(
        services.iter().any(|s| s == "rustman-e2e"),
        "linked trace {linked} has {services:?}"
    );
}
