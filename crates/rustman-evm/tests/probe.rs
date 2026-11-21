//! `rustman evm probe` against stub providers on loopback (add-evm-toolkit
//! tasks 6.1-6.4): the read-only allowlist, the method matrix, archive
//! detection, subscriptions, rate discovery, multi-provider consistency,
//! the budget, and the report. No test touches the internet.

mod common;

use std::num::NonZeroU32;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use rustman_core::safety::{Limits, SafetySettings};
use rustman_evm::blocklist::is_send_method;
use rustman_evm::probe::catalog::{MethodSelection, is_allowed};
use rustman_evm::probe::{
    ProbeCall, ProbeOptions, Provider, ProviderSpec, Timing, governed, probe, provider,
};
use rustman_http::SendOptions;
use rustman_report::exploration::{EvmProvider, ExplorationReport, Severity};
use rustman_test_support::jsonrpc::{JsonrpcWsServer, WsFrame};
use rustman_test_support::{JsonrpcHttpServer, RpcReply};
use serde_json::{Value, json};

const HEAD: u64 = 20_000_000;
const TOKEN: &str = "0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48";

/// What a stub provider answers besides the defaults.
type Answer = Option<Result<Value, (i64, String)>>;
type Custom = Arc<dyn Fn(&str, &Value) -> Answer + Send + Sync>;

/// A stub mainnet Geth node at `head`; `custom` answers first.
fn answer(
    method: &str,
    params: &Value,
    head: u64,
    custom: &Custom,
) -> Result<Value, (i64, String)> {
    if let Some(answer) = custom(method, params) {
        return answer;
    }
    let hex = |n: u64| json!(format!("{n:#x}"));
    match method {
        "eth_chainId" => Ok(json!("0x1")),
        "web3_clientVersion" => Ok(json!("Geth/v1.14.11-stable/linux-amd64/go1.23.2")),
        "eth_blockNumber" => Ok(hex(head)),
        "eth_getBlockByNumber" => {
            let number = params[0]
                .as_str()
                .and_then(|s| u64::from_str_radix(&s[2..], 16).ok());
            let n = number.unwrap_or(head);
            Ok(json!({
                "number": format!("{n:#x}"),
                "hash": format!("0x{n:064x}"),
                "timestamp": format!("{:#x}", 1_700_000_000 + n * 12),
                "gasUsed": "0xe4e1c0",
                "gasLimit": "0x1c9c380",
                "transactions": ["0x5c504ed432cb51138bcf09aa5e8a410dd4a1e204ef84bfed1be16dfba1b22060"]
            }))
        }
        "eth_getBalance" => Ok(json!("0xde0b6b3a7640000")),
        "debug_traceCall"
        | "trace_call"
        | "trace_block"
        | "trace_transaction"
        | "debug_traceTransaction"
        | "txpool_status" => Err((
            -32601,
            format!("the method {method} does not exist/is not available"),
        )),
        "eth_syncing" | "net_listening" => Ok(json!(false)),
        _ => Ok(json!("0x0")),
    }
}

fn reply(body: &Value, answer: Result<Value, (i64, String)>) -> RpcReply {
    let id = &body["id"];
    RpcReply::json(&match answer {
        Ok(result) => json!({"jsonrpc": "2.0", "id": id, "result": result}),
        Err((code, message)) => {
            json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": message}})
        }
    })
}

async fn node(
    head: u64,
    custom: impl Fn(&str, &Value) -> Answer + Send + Sync + 'static,
) -> JsonrpcHttpServer {
    let custom: Custom = Arc::new(custom);
    JsonrpcHttpServer::start(move |req, _| {
        let body = req.json();
        let method = body["method"].as_str().unwrap_or_default().to_owned();
        reply(&body, answer(&method, &body["params"], head, &custom))
    })
    .await
}

fn limits(rate: u32) -> Limits {
    Limits {
        rate: NonZeroU32::new(rate).unwrap(),
        ..Limits::default()
    }
}

fn governed_provider(url: &str, rate: u32) -> Provider {
    let policy =
        provider::policy(url, &SafetySettings::default(), None, Default::default()).unwrap();
    governed(ProviderSpec {
        endpoint: common::endpoint(url),
        options: SendOptions::default(),
        environment: None,
        pin: None,
        policy,
        limits: limits(rate),
        max_rate: NonZeroU32::new(40).unwrap(),
    })
}

fn options() -> ProbeOptions {
    ProbeOptions {
        limits: limits(500),
        timing: Timing {
            drift_samples: 2,
            drift_interval: Duration::from_millis(20),
            subscription_timeout: Duration::from_millis(600),
            rate_window: Duration::from_secs(1),
        },
        ..ProbeOptions::default()
    }
}

fn methods_of(server: &JsonrpcHttpServer) -> Vec<String> {
    server
        .hits()
        .iter()
        .map(|h| h.json()["method"].as_str().unwrap_or_default().to_owned())
        .collect()
}

fn evm(report: &ExplorationReport) -> &[EvmProvider] {
    &report.evm.as_ref().unwrap().providers
}

fn support<'a>(p: &'a EvmProvider, method: &str) -> &'a str {
    &p.methods
        .iter()
        .find(|m| m.method == method)
        .unwrap()
        .support
}

#[tokio::test]
async fn the_mock_node_never_sees_a_write_method() {
    let a = node(HEAD, |_, _| None).await;
    let b = node(HEAD, |_, _| None).await;
    let providers = [
        governed_provider(&a.url("/"), 500),
        governed_provider(&b.url("/"), 500),
    ];
    let options = ProbeOptions {
        methods: MethodSelection::parse("all").unwrap(),
        call: Some(ProbeCall::parse(&format!("{TOKEN}:0x18160ddd")).unwrap()),
        rate_discovery: true,
        max_rate: 5,
        ..options()
    };
    let report = probe(&providers, &options).await.unwrap();
    assert_eq!(report.summary.findings.total(), 0, "{:#?}", report.findings);
    for server in [&a, &b] {
        let methods = methods_of(server);
        assert!(methods.len() > 40, "{methods:?}");
        for method in &methods {
            assert!(!is_send_method(method), "{method} was sent");
            assert!(is_allowed(method), "{method} is not in the allowlist");
        }
    }
}

#[tokio::test]
async fn stub_providers_are_classified() {
    let archive = node(HEAD, |_, _| None).await;
    let pruned = node(HEAD, |method, params| {
        (method == "eth_getBalance" && params[1] == "0xf4240").then(|| {
            Err((
                -32000,
                "missing trie node 2b4c (path ) state is not available".into(),
            ))
        })
    })
    .await;
    let tracing = node(HEAD, |method, _| {
        matches!(method, "debug_traceCall" | "trace_call").then(|| Ok(json!({"type": "CALL"})))
    })
    .await;
    let restricted = node(HEAD, |method, _| {
        (method == "eth_getLogs").then(|| {
            Err((
                -32600,
                "eth_getLogs is not available on the Free plan".into(),
            ))
        })
    })
    .await;
    let providers: Vec<Provider> = [&archive, &pruned, &tracing, &restricted]
        .iter()
        .map(|s| governed_provider(&s.url("/"), 500))
        .collect();
    let report = probe(&providers, &options()).await.unwrap();
    let p = evm(&report);
    assert_eq!(report.evm.as_ref().unwrap().archive_block, 1_000_000);
    assert_eq!(p[0].archive, "archive");
    assert_eq!(p[1].archive, "full");
    assert!(
        p[1].archive_detail
            .as_deref()
            .unwrap()
            .contains("missing trie node")
    );
    assert_eq!(support(&p[0], "debug_traceCall"), "unsupported");
    assert_eq!(support(&p[0], "trace_call"), "unsupported");
    assert_eq!(support(&p[2], "debug_traceCall"), "supported");
    assert_eq!(support(&p[3], "eth_getLogs"), "restricted");
    assert_eq!(support(&p[0], "eth_call"), "supported");
    assert_eq!(p[0].client, "geth 1.14.11");
    let row = report
        .coverage
        .matrix
        .iter()
        .find(|r| r.operation == "debug_traceCall")
        .unwrap();
    assert_eq!(row.statuses.len(), 4);
    let classes: Vec<_> = row.statuses.iter().map(|c| c.class.as_deref()).collect();
    assert_eq!(
        classes,
        [
            Some("unsupported"),
            Some("unsupported"),
            Some("supported"),
            Some("unsupported")
        ],
        "the coverage matrix keeps each provider's class"
    );
    for provider in p {
        let latency = provider
            .latency
            .iter()
            .find(|l| l.method == "eth_call")
            .unwrap();
        assert!(latency.samples >= 1 && latency.max_ms >= latency.p50_ms);
        assert_eq!(provider.blocks.len(), 8);
        assert_eq!(provider.blocks.last().unwrap().number, HEAD);
    }
    assert_eq!(report.subject.as_deref(), Some("evm-rpc"));
}

/// The governor paces calls to 3 req/s (about 333 ms apart); a local stub
/// answers in a few milliseconds. Latency is the time on the wire, so the
/// pacing wait never shows in it.
#[tokio::test]
async fn latency_leaves_out_the_pacing_wait() {
    let server = node(HEAD, |_, _| None).await;
    let options = ProbeOptions {
        methods: MethodSelection::parse("eth_chainId").unwrap(),
        budget: 8,
        ..options()
    };
    let report = probe(&[governed_provider(&server.url("/"), 3)], &options)
        .await
        .unwrap();
    let p = &evm(&report)[0];
    assert_eq!(p.requests, 8);
    let samples: usize = p.latency.iter().map(|l| l.samples).sum();
    assert!(samples >= 7, "{:?}", p.latency);
    for latency in &p.latency {
        assert!(latency.max_ms < 200.0, "{latency:?}");
    }
}

/// A WebSocket provider answering calls, and `eth_subscribe` with a
/// subscription that sends `heads` new heads.
async fn ws_node(heads: usize) -> JsonrpcWsServer {
    JsonrpcWsServer::start(move |message| {
        let id = message["id"].clone();
        let method = message["method"].as_str().unwrap_or_default();
        let result = match method {
            "eth_subscribe" => json!("0xabc"),
            "eth_unsubscribe" => json!(true),
            _ => match answer(method, &message["params"], HEAD, &(Arc::new(|_: &str, _: &Value| None) as Custom)) {
                Ok(v) => v,
                Err((code, text)) => {
                    return vec![WsFrame::Json(json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": text}}))];
                }
            },
        };
        let mut frames = vec![WsFrame::Json(json!({"jsonrpc": "2.0", "id": id, "result": result}))];
        let subscribing = method == "eth_subscribe" && message["params"][0] == "newHeads";
        for _ in 0..if subscribing { heads } else { 0 } {
            frames.push(WsFrame::Pause(Duration::from_millis(20)));
            frames.push(WsFrame::Json(json!({
                "jsonrpc": "2.0", "method": "eth_subscription",
                "params": {"subscription": "0xabc", "result": {"number": "0x1"}}
            })));
        }
        frames
    })
    .await
}

#[tokio::test]
async fn a_silent_websocket_subscription_times_out() {
    let silent = ws_node(0).await;
    let live = ws_node(1).await;
    let providers = [
        governed_provider(&silent.url("/"), 500),
        governed_provider(&live.url("/"), 500),
    ];
    let options = ProbeOptions {
        methods: MethodSelection::parse("eth_chainId,eth_blockNumber").unwrap(),
        ..options()
    };
    let report = probe(&providers, &options).await.unwrap();
    let p = evm(&report);
    let status = |p: &EvmProvider, kind: &str| {
        p.subscriptions
            .iter()
            .find(|s| s.kind == kind)
            .unwrap()
            .status
            .clone()
    };
    assert_eq!(status(&p[0], "newHeads"), "timeout");
    assert_eq!(status(&p[0], "logs"), "supported");
    let logs = p[0]
        .subscriptions
        .iter()
        .find(|s| s.kind == "logs")
        .unwrap();
    assert_eq!(
        logs.detail.as_deref(),
        Some("subscribed; no log arrived before the timeout")
    );
    assert_eq!(status(&p[1], "newHeads"), "supported");
}

#[tokio::test]
async fn rate_discovery_finds_the_limit_and_honors_retry_after() {
    // 429 (Retry-After: 1) when the last five eth_blockNumber calls came
    // faster than 25 req/s: above 20 req/s, with a margin for slow timers.
    let times: Arc<Mutex<Vec<Instant>>> = Arc::default();
    let server = JsonrpcHttpServer::start(move |req, _| {
        let body = req.json();
        let method = body["method"].as_str().unwrap_or_default().to_owned();
        if method == "eth_blockNumber" {
            let mut times = times.lock().unwrap();
            times.push(Instant::now());
            if let [.., first, _, _, _, last] = times.as_slice() {
                let rate = 4.0 / last.duration_since(*first).as_secs_f64();
                if rate > 25.0 {
                    return RpcReply::raw(429, "text/plain", "slow down")
                        .with_header("retry-after", "1");
                }
            }
        }
        reply(
            &body,
            answer(
                &method,
                &body["params"],
                HEAD,
                &(Arc::new(|_: &str, _: &Value| None) as Custom),
            ),
        )
    })
    .await;
    let providers = [governed_provider(&server.url("/"), 10)];
    let options = ProbeOptions {
        methods: MethodSelection::parse("eth_chainId").unwrap(),
        rate_discovery: true,
        ..options()
    };
    let started = Instant::now();
    let report = probe(&providers, &options).await.unwrap();
    let limit = evm(&report)[0].rate_limit.clone().unwrap();
    assert_eq!(limit.limit_rps, Some(20), "{limit:?}");
    assert_eq!(limit.limited_at, Some(40));
    assert_eq!(limit.retry_after_ms, Some(1000));
    assert_eq!(limit.summary(), "rate limit \u{2248} 20 req/s");
    assert!(
        started.elapsed() >= Duration::from_secs(4),
        "{:?}",
        started.elapsed()
    );
    let finding = &report.findings[0];
    assert_eq!(finding.rule, "evm-rate-limit");
    assert_eq!(finding.severity, Severity::Info);
}

#[tokio::test]
async fn no_rate_discovery_without_the_flag() {
    let server = node(HEAD, |_, _| None).await;
    let options = ProbeOptions {
        methods: MethodSelection::parse("eth_chainId").unwrap(),
        ..options()
    };
    let report = probe(&[governed_provider(&server.url("/"), 500)], &options)
        .await
        .unwrap();
    assert!(evm(&report)[0].rate_limit.is_none());
    let block_numbers = methods_of(&server)
        .iter()
        .filter(|m| *m == "eth_blockNumber")
        .count();
    assert_eq!(block_numbers, 1);
}

#[tokio::test]
async fn providers_that_disagree_on_state_are_a_high_finding() {
    let a = node(HEAD, |_, _| None).await;
    let b = node(HEAD, |method, _| {
        (method == "eth_getBalance").then(|| Ok(json!("0x2")))
    })
    .await;
    let providers = [
        governed_provider(&a.url("/a"), 500),
        governed_provider(&b.url("/b"), 500),
    ];
    let options = ProbeOptions {
        methods: MethodSelection::parse("eth_chainId").unwrap(),
        ..options()
    };
    let report = probe(&providers, &options).await.unwrap();
    assert_eq!(report.failing(Severity::High), 1, "{:#?}", report.findings);
    let finding = &report.findings[0];
    assert_eq!(finding.rule, "evm-state-mismatch");
    assert_eq!(finding.severity, Severity::High);
    let common = HEAD - 10;
    assert!(
        finding.message.contains(&format!("at block {common}")),
        "{}",
        finding.message
    );
    let body = &finding.evidence.body;
    assert!(
        body.contains(&a.url("/a")) && body.contains("\"0xde0b6b3a7640000\""),
        "{body}"
    );
    assert!(
        body.contains(&b.url("/b")) && body.contains("\"0x2\""),
        "{body}"
    );
    let request = finding.evidence.request_body.as_deref().unwrap();
    assert!(request.contains(&format!("{common:#x}")), "{request}");
    assert_eq!(report.evm.as_ref().unwrap().common_block, Some(common));
}

#[tokio::test]
async fn a_lagging_provider_is_a_medium_drift_finding() {
    let a = node(HEAD, |_, _| None).await;
    let b = node(HEAD - 8, |_, _| None).await;
    let providers = [
        governed_provider(&a.url("/"), 500),
        governed_provider(&b.url("/"), 500),
    ];
    let options = ProbeOptions {
        methods: MethodSelection::parse("eth_chainId").unwrap(),
        ..options()
    };
    let report = probe(&providers, &options).await.unwrap();
    let drift: Vec<_> = report
        .findings
        .iter()
        .filter(|f| f.rule == "evm-head-drift")
        .collect();
    assert_eq!(drift.len(), 1, "{:#?}", report.findings);
    assert_eq!(drift[0].severity, Severity::Medium);
    assert_eq!(drift[0].message, "head drift 8 blocks");
    assert!(drift[0].operation.contains(&b.url("/")));
    assert_eq!(evm(&report)[1].drift, Some(8));
    // The block hashes of a stub depend on the number alone: no mismatch.
    assert_eq!(report.failing(Severity::High), 0);
}

#[tokio::test]
async fn providers_on_different_chains_stop_with_exit_2() {
    let mainnet = node(HEAD, |_, _| None).await;
    let sepolia = node(HEAD, |method, _| {
        (method == "eth_chainId").then(|| Ok(json!("0xaa36a7")))
    })
    .await;
    let providers = [
        governed_provider(&mainnet.url("/"), 500),
        governed_provider(&sepolia.url("/"), 500),
    ];
    let error = probe(&providers, &options()).await.unwrap_err();
    assert_eq!(
        error.to_string(),
        "providers report different chains (1, 11155111)"
    );
    assert_eq!(error.exit_code(), 2);
    for server in [&mainnet, &sepolia] {
        assert_eq!(methods_of(server), ["eth_chainId"]);
    }
}

#[tokio::test]
async fn an_exhausted_budget_skips_the_remaining_checks() {
    let server = node(HEAD, |_, _| None).await;
    let options = ProbeOptions {
        budget: 12,
        ..options()
    };
    let report = probe(&[governed_provider(&server.url("/"), 500)], &options)
        .await
        .unwrap();
    assert_eq!(server.hits().len(), 12);
    let p = &evm(&report)[0];
    assert!(p.budget_exhausted);
    assert_eq!(p.requests, 12);
    assert_eq!(support(p, "trace_call"), "skipped (budget)");
    assert_eq!(p.archive, "skipped (budget)");
    let skipped: Vec<_> = report
        .probes
        .iter()
        .filter(|r| r.outcome == "skipped")
        .collect();
    assert!(!skipped.is_empty());
    assert!(
        skipped
            .iter()
            .all(|r| r.reason.as_deref() == Some("budget"))
    );
    let note = report
        .findings
        .iter()
        .find(|f| f.rule == "evm-budget-exhausted")
        .unwrap();
    assert!(
        note.message.contains("skipped (budget)"),
        "{}",
        note.message
    );
}

#[tokio::test]
async fn the_report_validates_against_the_published_schema() {
    let a = node(HEAD, |_, _| None).await;
    let b = node(HEAD, |method, _| {
        (method == "eth_getBalance").then(|| Ok(json!("0x2")))
    })
    .await;
    let providers = [
        governed_provider(&a.url("/"), 500),
        governed_provider(&b.url("/"), 500),
    ];
    let options = ProbeOptions {
        call: Some(ProbeCall::parse(&format!("{TOKEN}:0x18160ddd")).unwrap()),
        ..options()
    };
    let report = probe(&providers, &options).await.unwrap();
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../docs/schemas/report.schema.json");
    let schema: Value = serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
    let validator = jsonschema::validator_for(&schema).unwrap();
    let value = serde_json::to_value(&report).unwrap();
    let errors: Vec<String> = validator
        .iter_errors(&value)
        .map(|e| e.to_string())
        .collect();
    assert!(errors.is_empty(), "{errors:#?}");
    assert_eq!(value["kind"], "exploration");
    assert_eq!(value["subject"], "evm-rpc");
    let text = serde_json::to_string(&report).unwrap();
    let parsed = rustman_report::Report::parse(&text).unwrap();
    assert!(matches!(parsed, rustman_report::Report::Exploration(_)));
    // The coverage section shows each provider's class of a method.
    let options = rustman_report::RenderOptions::default();
    let markdown =
        rustman_report::render(&parsed, rustman_report::RenderFormat::Markdown, &options);
    let cell = format!("{} unsupported", a.url("/"));
    assert!(markdown.contains(&cell), "{markdown}");
    let html = rustman_report::render(&parsed, rustman_report::RenderFormat::Html, &options);
    assert!(html.contains(" unsupported</span>"), "{html}");
}

#[tokio::test]
async fn a_secret_in_the_endpoint_never_reaches_the_report() {
    let canary = "probeCANARY7f3e";
    let server = node(HEAD, |_, _| None).await;
    let url = server.url(&format!("/v3/{canary}"));
    let mut endpoint = common::endpoint(&url);
    endpoint.secrets = vec![rustman_core::Secret::new("key", canary.to_owned())];
    let policy =
        provider::policy(&url, &SafetySettings::default(), None, Default::default()).unwrap();
    let provider = governed(ProviderSpec {
        endpoint,
        options: SendOptions::default(),
        environment: None,
        pin: None,
        policy,
        limits: limits(500),
        max_rate: NonZeroU32::new(40).unwrap(),
    });
    let report = probe(&[provider], &options()).await.unwrap();
    let text = serde_json::to_string(&report).unwrap();
    assert!(!text.contains(canary), "{text}");
    assert!(
        text.contains("redacted:key") || text.contains("{{key}}"),
        "{text}"
    );
    let shown = rustman_evm::probe::report::render_text(&report);
    assert!(!shown.contains(canary), "{shown}");
}
