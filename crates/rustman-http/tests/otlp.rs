//! OTLP export of client spans (add-trace-propagation tasks 4.1-4.3),
//! against the in-process collector of rustman-test-support only.
#![cfg(feature = "otlp")]

mod support;

use std::path::Path;
use std::time::Duration;

use rustman_core::run::{RequestRecord, RequestStatus, plan};
use rustman_core::{SecretStore, Workspace};
use rustman_http::runner::{RunContext, RunOptions, Runner};
use rustman_test_support::otlp::{CollectedSpan, OtlpCollector, unreachable_endpoint};
use support::collection::write;
use support::recorder::Recorder;

const WAIT: Duration = Duration::from_secs(20);

/// A workspace whose `rustman.toml` holds `toml` and whose `dev`
/// environment points at `server`, with `count` requests to
/// `/ok?token=abc`.
fn workspace(server: &Recorder, toml: &str, count: usize) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let settings = format!("[workspace]\nname = \"t\"\nversion = 1\n{toml}");
    let env = format!("variables:\n  base_url: {}\n", server.url(""));
    write(
        dir.path(),
        &[
            ("rustman.toml", &settings),
            ("environments/dev.yaml", &env),
            (".gitignore", ".rustman/\n"),
        ],
    );
    for i in 0..count {
        let request =
            format!("version: 1\nkind: http\nname: r{i}\nurl: '{{{{base_url}}}}/ok?token=abc'\n");
        write(dir.path(), &[(&format!("r{i:02}.rman.yaml"), &request)]);
    }
    dir
}

fn export(endpoint: &str, protocol: &str) -> String {
    format!(
        "[trace]\nenabled = true\nscope = \"run\"\n[trace.export]\nenabled = true\nendpoint = \"{endpoint}\"\nprotocol = \"{protocol}\"\n"
    )
}

/// Run every request, flush the spans and return the records and the
/// export warnings.
async fn run(root: &Path) -> (Vec<RequestRecord>, Vec<String>) {
    let ws = Workspace::load(root).unwrap_or_else(|e| panic!("{e}"));
    let env = ws.active_environment(Some("dev"), None).unwrap();
    let store = SecretStore::with_keychain(root, None);
    let ctx = RunContext {
        root: &ws.root,
        settings: &ws.settings,
        environment: env,
        secrets: &store,
        in_workspace: true,
    };
    let runner = Runner::new(ctx, RunOptions::default()).unwrap();
    let record = runner.run(&plan(&ws, &[], &[])).await;
    runner.flush_spans(WAIT);
    (record.requests, runner.export_warnings())
}

fn assert_client_span(span: &CollectedSpan, record: &RequestRecord) {
    let trace = record.trace.as_ref().unwrap();
    assert_eq!(
        (&span.trace_id, &span.span_id),
        (&trace.trace_id, &trace.span_id)
    );
    assert_eq!(span.kind, CollectedSpan::CLIENT);
    assert_eq!(span.service, "rustman");
    assert_eq!(span.name, "GET");
    assert_eq!(span.attribute("http.request.method"), Some("GET"));
    assert_eq!(span.attribute("http.response.status_code"), Some("200"));
    assert_eq!(span.attribute("server.address"), Some("127.0.0.1"));
    let url = span.attribute("url.full").unwrap();
    assert!(url.ends_with("/ok?token=«redacted:token»"), "{url}");
    assert!(!url.contains("abc"), "{url}");
    assert!(
        span.events.iter().any(|e| e == "waiting"),
        "{:?}",
        span.events
    );
    assert!(
        span.attributes
            .iter()
            .all(|(k, v)| !k.contains("header") && !v.contains("rustman/")),
        "no header is exported: {:?}",
        span.attributes
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn grpc_export_sends_one_client_span_per_exchange() {
    let server = Recorder::start().await;
    let collector = OtlpCollector::start().await;
    let dir = workspace(&server, &export(&collector.endpoint(), "grpc"), 2);
    let (records, warnings) = run(dir.path()).await;
    assert!(warnings.is_empty(), "{warnings:?}");
    let spans = collector.wait_for_spans(2, WAIT).await;
    assert_eq!(spans.len(), 2, "{spans:?}");
    for (record, span) in records.iter().zip(&spans) {
        assert_client_span(span, record);
    }
    assert_eq!(spans[0].trace_id, spans[1].trace_id, "scope: run");
    let hit = &collector.hits()[0];
    assert_eq!(
        hit.path,
        "/opentelemetry.proto.collector.trace.v1.TraceService/Export"
    );
    assert_eq!(hit.content_type, "application/grpc");
}

#[tokio::test(flavor = "multi_thread")]
async fn http_protobuf_export_posts_to_v1_traces() {
    let server = Recorder::start().await;
    let collector = OtlpCollector::start().await;
    let dir = workspace(&server, &export(&collector.endpoint(), "http/protobuf"), 1);
    let (records, warnings) = run(dir.path()).await;
    assert!(warnings.is_empty(), "{warnings:?}");
    let spans = collector.wait_for_spans(1, WAIT).await;
    assert_eq!(spans.len(), 1);
    assert_client_span(&spans[0], &records[0]);
    let hit = &collector.hits()[0];
    assert_eq!(
        (hit.path.as_str(), hit.content_type.as_str()),
        ("/v1/traces", "application/x-protobuf")
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn without_export_nothing_reaches_the_collector() {
    let server = Recorder::start().await;
    let collector = OtlpCollector::start().await;
    let dir = workspace(&server, "[trace]\nenabled = true\n", 1);
    let (records, warnings) = run(dir.path()).await;
    assert!(records[0].trace.is_some() && warnings.is_empty());
    // `run` flushed the spans and waited for the collector's answer, which
    // comes after the collector records the request: a span exported by
    // mistake would already be here.
    assert!(collector.hits().is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn a_collector_that_is_down_never_changes_the_run() {
    let server = Recorder::start().await;
    let endpoint = unreachable_endpoint().await;
    let dir = workspace(&server, &export(&endpoint, "grpc"), 10);
    let (records, warnings) = run(dir.path()).await;
    assert_eq!(records.len(), 10);
    assert!(
        records
            .iter()
            .all(|r| r.status == RequestStatus::Passed && r.warnings.is_empty()),
        "{records:?}"
    );
    assert_eq!(server.hits().len(), 10);
    assert_eq!(warnings.len(), 1, "{warnings:?}");
    assert!(
        warnings[0].starts_with("trace export to 127.0.0.1:")
            && warnings[0].contains("requests are unaffected"),
        "{warnings:?}"
    );
    // A second run within the minute fails again, silently.
    let (again, warnings) = run(dir.path()).await;
    assert!(again.iter().all(|r| r.status == RequestStatus::Passed));
    assert!(warnings.is_empty(), "one warning per minute: {warnings:?}");
}

/// A secret in the URL path, the query and `tracestate` never reaches
/// the collector.
#[tokio::test(flavor = "multi_thread")]
async fn exported_spans_carry_no_secret() {
    const CANARY: &str = "c4n4ry-0tlp-s3cret";
    let server = Recorder::start().await;
    let collector = OtlpCollector::start().await;
    let dir = workspace(&server, &export(&collector.endpoint(), "grpc"), 0);
    write(
        dir.path(),
        &[
            (
                "environments/dev.yaml",
                &format!(
                    "variables:\n  base_url: {}\nsecrets: [api_key]\ntrace:\n  tracestate:\n    rojo: '{{{{api_key}}}}'\n",
                    server.url("")
                ),
            ),
            (
                "leak.rman.yaml",
                "version: 1\nkind: http\nname: leak\nurl: '{{base_url}}/ok/{{api_key}}?key={{api_key}}'\n",
            ),
        ],
    );
    let store = SecretStore::with_keychain(dir.path(), None);
    store
        .set(
            "dev",
            &rustman_core::Secret::new("api_key", CANARY.to_owned()),
        )
        .unwrap();
    let (records, _) = run(dir.path()).await;
    assert_eq!(records.len(), 1);
    let spans = collector.wait_for_spans(1, WAIT).await;
    assert_eq!(spans.len(), 1);
    let shown = format!("{:?}", spans[0]);
    assert!(!shown.contains(CANARY), "{shown}");
    assert_eq!(spans[0].trace_state, "rojo=«redacted:api_key»");
    assert!(
        server.hits()[0].path.contains(CANARY),
        "the request itself carried it"
    );
}
