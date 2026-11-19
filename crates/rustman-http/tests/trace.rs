//! Trace context injection through the engine and the runner
//! (add-trace-propagation tasks 2.1 and 2.2), against loopback servers only.

mod support;

use std::path::Path;

use http::HeaderMap;
use rustman_core::history::HistoryStore;
use rustman_core::run::{RequestStatus, plan};
use rustman_core::trace::{PropagationFormat, TraceId, TracePropagation};
use rustman_core::{Secret, SecretStore, Workspace};
use rustman_http::runner::{RunContext, RunOptions, Runner};
use rustman_http::{Exchange, HttpEngine, SendOptions};
use support::collection::write;
use support::recorder::Recorder;
use support::request::{echoed_header, get, pair};
use support::{TestServer, routes};

const TRACE_HEADERS: [&str; 8] = [
    "traceparent",
    "tracestate",
    "baggage",
    "b3",
    "x-b3-traceid",
    "x-b3-spanid",
    "x-b3-sampled",
    "x-b3-flags",
];

const EXPLICIT: &str = "00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01";

fn traced(propagation: TracePropagation) -> SendOptions {
    SendOptions {
        trace: Some(propagation),
        ..SendOptions::default()
    }
}

async fn send_echo(
    server: &TestServer,
    headers: &[(&str, &str)],
    options: &SendOptions,
) -> (Exchange, serde_json::Value) {
    let mut request = get(&server.url("/echo"));
    request.headers = headers.iter().map(|(n, v)| pair(n, v)).collect();
    let exchange = HttpEngine::new().send(&request, options).await.unwrap();
    let echo = serde_json::from_slice(&exchange.body).unwrap();
    (exchange, echo)
}

/// `^00-[0-9a-f]{32}-[0-9a-f]{16}-<flags>$`, with neither id all zeros.
fn assert_traceparent(value: &str, flags: &str) {
    let parts: Vec<&str> = value.split('-').collect();
    assert_eq!(parts.len(), 4, "{value}");
    assert_eq!(parts[0], "00", "{value}");
    let hex = |s: &str, len: usize| {
        s.len() == len && s.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
    };
    assert!(hex(parts[1], 32) && hex(parts[2], 16), "{value}");
    assert_ne!(parts[1], "0".repeat(32));
    assert_ne!(parts[2], "0".repeat(16));
    assert_eq!(parts[3], flags, "{value}");
}

#[tokio::test]
async fn no_trace_header_is_sent_by_default() {
    let server = TestServer::http().await;
    let (exchange, echo) = send_echo(&server, &[], &SendOptions::default()).await;
    for name in TRACE_HEADERS {
        assert!(echoed_header(&echo, name).is_empty(), "{name}: {echo}");
    }
    assert!(exchange.trace.is_none());
}

#[tokio::test]
async fn an_enabled_trace_sends_a_sampled_traceparent() {
    let server = TestServer::http().await;
    let (exchange, echo) = send_echo(&server, &[], &traced(TracePropagation::default())).await;
    let sent = echoed_header(&echo, "traceparent");
    assert_eq!(sent.len(), 1);
    assert_traceparent(&sent[0], "01");
    let ctx = exchange.trace.unwrap().context;
    assert!(sent[0].contains(&ctx.trace_id.to_string()));
    assert!(sent[0].contains(&ctx.span_id.to_string()));
    let (_, again) = send_echo(&server, &[], &traced(TracePropagation::default())).await;
    assert_ne!(echoed_header(&again, "traceparent"), sent);
}

#[tokio::test]
async fn an_unsampled_trace_sends_flags_00() {
    let server = TestServer::http().await;
    let propagation = TracePropagation {
        sampled: false,
        ..TracePropagation::default()
    };
    let (_, echo) = send_echo(&server, &[], &traced(propagation)).await;
    assert_traceparent(&echoed_header(&echo, "traceparent")[0], "00");
}

#[tokio::test]
async fn an_explicit_traceparent_is_sent_unchanged() {
    let server = TestServer::http().await;
    let (exchange, echo) = send_echo(
        &server,
        &[("traceparent", EXPLICIT)],
        &traced(TracePropagation::default()),
    )
    .await;
    assert_eq!(echoed_header(&echo, "traceparent"), [EXPLICIT]);
    let ctx = exchange.trace.unwrap().context;
    assert_eq!(ctx.trace_id.to_string(), "4bf92f3577b34da6a3ce929d0e0e4736");
}

#[tokio::test]
async fn an_explicit_traceparent_is_recorded_with_tracing_off() {
    let server = TestServer::http().await;
    let headers = [("traceparent", EXPLICIT)];
    let (exchange, echo) = send_echo(&server, &headers, &SendOptions::default()).await;
    assert_eq!(echoed_header(&echo, "traceparent"), [EXPLICIT]);
    let sent = exchange.trace.expect("the request's own trace context");
    assert_eq!(
        sent.context.trace_id.to_string(),
        "4bf92f3577b34da6a3ce929d0e0e4736"
    );
    assert_eq!(sent.baggage, None);
}

#[tokio::test]
async fn every_format_carries_the_same_trace_id_with_tracestate_and_baggage() {
    let server = TestServer::http().await;
    let propagation = TracePropagation {
        formats: vec![
            PropagationFormat::W3c,
            PropagationFormat::B3,
            PropagationFormat::B3Multi,
        ],
        tracestate: vec![("rojo".into(), "00f067aa0ba902b7".into())],
        baggage: vec![("tenant".into(), "acme".into())],
        ..TracePropagation::default()
    };
    let (exchange, echo) = send_echo(&server, &[], &traced(propagation)).await;
    let sent = exchange.trace.unwrap();
    assert_eq!(sent.tracestate.as_deref(), Some("rojo=00f067aa0ba902b7"));
    assert_eq!(sent.baggage.as_deref(), Some("tenant=acme"));
    let trace = sent.context.trace_id.to_string();
    assert!(echoed_header(&echo, "traceparent")[0].contains(&trace));
    assert!(echoed_header(&echo, "b3")[0].starts_with(&trace));
    assert_eq!(echoed_header(&echo, "x-b3-traceid"), [trace]);
    assert_eq!(
        echoed_header(&echo, "tracestate"),
        ["rojo=00f067aa0ba902b7"]
    );
    assert_eq!(echoed_header(&echo, "baggage"), ["tenant=acme"]);
}

const RUN_REQUESTS: [(&str, &str); 3] = [
    (
        "flow/1-login.rman.yaml",
        "version: 1\nkind: http\nname: login\nmethod: POST\nurl: '{{base_url}}/ok'\n",
    ),
    (
        "flow/2-list.rman.yaml",
        "version: 1\nkind: http\nname: list\nurl: '{{base_url}}/ok'\n",
    ),
    (
        "flow/3-pay.rman.yaml",
        "version: 1\nkind: http\nname: pay\nmethod: POST\nurl: '{{base_url}}/ok'\n",
    ),
];

/// A workspace whose `dev` environment points at `server` with the
/// `trace:` block `trace` (YAML, indented by two spaces), a secret
/// `api_key`, and the three requests of [`RUN_REQUESTS`].
fn run_workspace(server: &Recorder, trace: &str) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let env = format!(
        "variables:\n  base_url: {}\n  tenant_id: acme\nsecrets: [api_key]\n{trace}",
        server.url("")
    );
    write(
        dir.path(),
        &[
            ("rustman.toml", "[workspace]\nname = \"t\"\nversion = 1\n"),
            ("environments/dev.yaml", &env),
            (".gitignore", ".rustman/\n"),
        ],
    );
    write(dir.path(), &RUN_REQUESTS);
    let store = SecretStore::with_keychain(dir.path(), None);
    store
        .set(
            "dev",
            &Secret::new("api_key", "baggage-secret-canary".to_owned()),
        )
        .unwrap();
    dir
}

async fn run(root: &Path, forced: bool) -> Vec<rustman_core::run::RequestRecord> {
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
    let options = RunOptions {
        trace: forced,
        ..RunOptions::default()
    };
    let runner = Runner::new(ctx, options).unwrap();
    let record = runner.run(&plan(&ws, &[], &[])).await;
    for request in &record.requests {
        assert_eq!(request.status, RequestStatus::Passed, "{request:?}");
    }
    record.requests
}

fn traceparents(hits: &[HeaderMap]) -> Vec<String> {
    hits.iter()
        .map(|h| h["traceparent"].to_str().unwrap().to_owned())
        .collect()
}

#[tokio::test]
async fn a_run_scoped_trace_shares_one_trace_id_with_distinct_spans() {
    let server = Recorder::start().await;
    let dir = run_workspace(&server, "trace:\n  enabled: true\n  scope: run\n");
    let records = run(dir.path(), false).await;
    let hits: Vec<HeaderMap> = server.hits().into_iter().map(|h| h.headers).collect();
    let sent = traceparents(&hits);
    // Each record (and so each report request) carries the ids it sent.
    for (record, header) in records.iter().zip(&sent) {
        let trace = record.trace.as_ref().expect("a trace record");
        assert_eq!(
            *header,
            format!("00-{}-{}-01", trace.trace_id, trace.span_id)
        );
        assert_eq!(
            (trace.backend_trace_id.as_ref(), trace.link.as_ref()),
            (None, None)
        );
    }
    assert_eq!(sent.len(), 3);
    let traces: Vec<&str> = sent.iter().map(|t| &t[3..35]).collect();
    let spans: Vec<&str> = sent.iter().map(|t| &t[36..52]).collect();
    assert!(TraceId::from_hex(traces[0]).is_some());
    assert!(traces.iter().all(|t| *t == traces[0]), "{sent:?}");
    assert!(spans[0] != spans[1] && spans[1] != spans[2] && spans[0] != spans[2]);
}

#[tokio::test]
async fn each_exchange_gets_its_own_trace_by_default() {
    let server = Recorder::start().await;
    let dir = run_workspace(&server, "trace:\n  enabled: true\n");
    run(dir.path(), false).await;
    let hits: Vec<HeaderMap> = server.hits().into_iter().map(|h| h.headers).collect();
    let sent = traceparents(&hits);
    assert_ne!(sent[0][3..35], sent[1][3..35]);
    assert_ne!(sent[1][3..35], sent[2][3..35]);
}

#[tokio::test]
async fn a_run_sends_no_trace_header_unless_enabled_or_forced() {
    let server = Recorder::start().await;
    let dir = run_workspace(&server, "");
    run(dir.path(), false).await;
    for hit in server.hits() {
        for name in TRACE_HEADERS {
            assert!(!hit.headers.contains_key(name), "{name}");
        }
    }
    let forced = Recorder::start().await;
    let dir = run_workspace(&forced, "trace:\n  enabled: false\n");
    run(dir.path(), true).await;
    let hits: Vec<HeaderMap> = forced.hits().into_iter().map(|h| h.headers).collect();
    assert_eq!(traceparents(&hits).len(), 3);
}

#[tokio::test]
async fn run_baggage_resolves_variables_and_b3multi_shares_the_trace_id() {
    let server = Recorder::start().await;
    let trace = "trace:\n  enabled: true\n  formats: [w3c, b3multi]\n  baggage:\n    tenant: '{{tenant_id}}'\n";
    let dir = run_workspace(&server, trace);
    run(dir.path(), false).await;
    let hit = &server.hits()[0].headers;
    assert_eq!(hit["baggage"], "tenant=acme");
    let traceparent = hit["traceparent"].to_str().unwrap();
    assert_eq!(&traceparent[3..35], hit["x-b3-traceid"].to_str().unwrap());
}

/// Run the one request of a workspace against `/traced`, recording history,
/// and return its record and the history entry as JSON text.
async fn traced_run(trace: &str) -> (rustman_core::run::RequestRecord, String) {
    let server = TestServer::http().await;
    let dir = tempfile::tempdir().unwrap();
    let env = format!(
        "variables:\n  base_url: {}\nsecrets: [api_key]\n{trace}",
        server.url("")
    );
    write(
        dir.path(),
        &[
            ("rustman.toml", "[workspace]\nname = \"t\"\nversion = 1\n"),
            ("environments/dev.yaml", &env),
            (".gitignore", ".rustman/\n"),
            (
                "traced.rman.yaml",
                "version: 1\nkind: http\nname: traced\nurl: '{{base_url}}/traced'\n",
            ),
        ],
    );
    let store = SecretStore::with_keychain(dir.path(), None);
    let canary = Secret::new("api_key", "baggage-secret-canary".to_owned());
    store.set("dev", &canary).unwrap();
    let ws = Workspace::load(dir.path()).unwrap_or_else(|e| panic!("{e}"));
    let env = ws.active_environment(Some("dev"), None).unwrap();
    let history = || HistoryStore::for_workspace(&ws.root, &ws.settings.history);
    let ctx = RunContext {
        root: &ws.root,
        settings: &ws.settings,
        environment: env,
        secrets: &store,
        in_workspace: true,
    };
    let options = RunOptions {
        history: Some(history()),
        ..RunOptions::default()
    };
    let runner = Runner::new(ctx, options).unwrap();
    let mut run = runner.run(&plan(&ws, &[], &[])).await;
    let entries = history().list().unwrap().entries;
    assert_eq!(entries.len(), 1);
    let entry = serde_json::to_string(&entries[0]).unwrap();
    (run.requests.remove(0), entry)
}

#[tokio::test]
async fn records_carry_the_trace_backend_id_link_and_server_timings() {
    let trace = "trace:\n  enabled: true\n  link: jaeger\n  link_base: http://jaeger:16686\n  baggage:\n    tenant: acme\n    api_key: '{{api_key}}'\n";
    let (record, entry) = traced_run(trace).await;
    assert_eq!(record.status, RequestStatus::Passed, "{record:?}");
    let recorded = record.trace.as_ref().expect("a trace record");
    assert!(TraceId::from_hex(&recorded.trace_id).is_some());
    assert_eq!(recorded.span_id.len(), 16);
    assert!(recorded.sampled);
    let backend = routes::BACKEND_TRACE_ID;
    assert_eq!(recorded.backend_trace_id.as_deref(), Some(backend));
    assert_eq!(
        recorded.link.as_deref(),
        Some(format!("http://jaeger:16686/trace/{backend}").as_str())
    );
    assert!(recorded.end_ms >= recorded.start_ms && recorded.start_ms > 0);
    assert_eq!(
        recorded.baggage.as_deref(),
        Some("api_key=«redacted:api_key»,tenant=acme")
    );
    let timings = &record.exchange.as_ref().unwrap().server_timings;
    let shown: Vec<(&str, Option<f64>)> = timings
        .iter()
        .map(|t| (t.name.as_str(), t.dur_ms))
        .collect();
    assert_eq!(shown, [("db", Some(53.2)), ("app", Some(12.0))]);
    let report = serde_json::to_string(&record).unwrap();
    for text in [&report, &entry] {
        assert!(!text.contains("baggage-secret-canary"), "{text}");
        assert!(text.contains("api_key=«redacted:api_key»"), "{text}");
        assert!(text.contains(backend), "{text}");
    }
}

#[tokio::test]
async fn records_carry_no_trace_when_tracing_is_off() {
    let (record, entry) = traced_run("").await;
    assert!(record.trace.is_none());
    assert!(!entry.contains("\"trace\""), "{entry}");
    // Server timings are parsed whether or not tracing is on.
    assert_eq!(record.exchange.unwrap().server_timings.len(), 2);
}

/// `[trace.export]` turns propagation on; a build without the `otlp`
/// feature says it cannot export, and sends as usual.
#[tokio::test]
async fn an_enabled_export_turns_tracing_on_and_warns_without_otlp() {
    let server = Recorder::start().await;
    let dir = run_workspace(
        &server,
        "trace:\n  export:\n    enabled: true\n    endpoint: http://127.0.0.1:9\n",
    );
    let ws = Workspace::load(dir.path()).unwrap_or_else(|e| panic!("{e}"));
    let env = ws.active_environment(Some("dev"), None).unwrap();
    let store = SecretStore::with_keychain(dir.path(), None);
    let ctx = RunContext {
        root: &ws.root,
        settings: &ws.settings,
        environment: env,
        secrets: &store,
        in_workspace: true,
    };
    let runner = Runner::new(ctx, RunOptions::default()).unwrap();
    let record = runner.run(&plan(&ws, &[], &[])).await;
    assert!(record.requests.iter().all(|r| r.trace.is_some()));
    assert_eq!(traceparents(&headers(&server)).len(), 3);
    let warnings = runner.export_warnings();
    if cfg!(feature = "otlp") {
        runner.flush_spans(std::time::Duration::from_secs(20));
        assert_eq!(runner.export_warnings().len(), 1, "the collector is down");
    } else {
        assert_eq!(
            warnings,
            [
                "trace.export: this build cannot export spans (build rustman with the `otlp` feature)"
            ]
        );
        assert!(runner.export_warnings().is_empty(), "said once");
    }
}

fn headers(server: &Recorder) -> Vec<HeaderMap> {
    server.hits().into_iter().map(|h| h.headers).collect()
}

/// Regression: a `baggage` header carrying a secret followed a cross-origin
/// redirect, while any other header carrying it was dropped.
#[tokio::test]
async fn secret_baggage_is_dropped_on_a_cross_origin_redirect() {
    let server = TestServer::http().await;
    let other = format!("http://127.0.0.1:{}/echo", server.port());
    let to: String = form_urlencoded::byte_serialize(other.as_bytes()).collect();
    let request = get(&server.url(&format!("/redirect/302?to={to}")));
    let secret = "s3cr3t canary;x=1";
    let propagation = TracePropagation {
        baggage: vec![("api_key".into(), secret.into())],
        secrets: vec![Secret::new("api_key", secret.to_owned())],
        ..TracePropagation::default()
    };
    let exchange = HttpEngine::new()
        .send(&request, &traced(propagation))
        .await
        .unwrap();
    let echo: serde_json::Value = serde_json::from_slice(&exchange.body).unwrap();
    assert!(echoed_header(&echo, "baggage").is_empty(), "{echo}");
    assert!(!echo.to_string().contains("s3cr3t"), "{echo}");
    assert_eq!(echoed_header(&echo, "traceparent").len(), 1, "{echo}");
    assert!(
        exchange.warnings.iter().any(|w| w.contains("baggage")),
        "{:?}",
        exchange.warnings
    );
}

#[tokio::test]
async fn oversized_baggage_and_an_invalid_explicit_header_warn() {
    let server = TestServer::http().await;
    let propagation = TracePropagation {
        baggage: (0..80).map(|i| (format!("k{i}"), "v".to_owned())).collect(),
        ..TracePropagation::default()
    };
    let (exchange, echo) = send_echo(&server, &[], &traced(propagation)).await;
    assert_eq!(echoed_header(&echo, "baggage").len(), 1);
    assert!(
        exchange.warnings.iter().any(|w| w.contains("80 members")),
        "{:?}",
        exchange.warnings
    );
    let options = traced(TracePropagation {
        formats: vec![PropagationFormat::W3c, PropagationFormat::B3],
        ..TracePropagation::default()
    });
    let upper = "00-4BF92F3577B34DA6A3CE929D0E0E4736-00F067AA0BA902B7-01";
    let (exchange, echo) = send_echo(&server, &[("traceparent", upper)], &options).await;
    assert_eq!(echoed_header(&echo, "traceparent"), [upper]);
    assert!(echoed_header(&echo, "b3").is_empty(), "{echo}");
    assert!(exchange.trace.is_none());
    assert!(
        exchange.warnings.iter().any(|w| w.contains("not valid")),
        "{:?}",
        exchange.warnings
    );
}

/// Regression: a streamed request's errors were redacted with the
/// request's own secrets only, not the secrets its trace values carry. The
/// refused address stands in for a trace secret the error would print.
#[tokio::test]
async fn stream_errors_redact_trace_secrets() {
    let canary = "127.0.0.1:1";
    let request = get(&format!("http://{canary}/"));
    let options = traced(TracePropagation {
        secrets: vec![Secret::new("api_key", canary.to_owned())],
        ..TracePropagation::default()
    });
    let Err(error) = HttpEngine::new().open_stream(&request, &options).await else {
        panic!("the request must fail");
    };
    let text = error.to_string();
    assert!(text.contains("cannot connect"), "{text}");
    assert!(!text.contains(canary), "{text}");
}
