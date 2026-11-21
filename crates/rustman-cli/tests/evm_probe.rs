//! `rustman evm probe` against stub providers (add-evm-toolkit tasks
//! 6.1-6.5): exit codes, `--report`, the secret canary, `--viz` with a
//! stub companion and without one, and the examples of docs/evm-probe.md.
//!
//! This test has no libtest harness (`harness = false`) so that its own
//! executable can be the stub companion (see `common::viz_stub`).

mod common;

use std::path::Path;

use common::viz_stub::{STUB, STUB_LOG, Test, saved as stub_lines};
use common::{rustman, valid_report, write};
use rustman_test_support::{JsonrpcHttpServer, RpcReply};
use serde_json::json;

const CANARY: &str = "probeCANARYk3y8d";
const HEAD: u64 = 20_000_000;
/// Keeps the drift check short (5 samples).
const FAST_DRIFT: [&str; 2] = ["--drift-interval", "10ms"];

const TESTS: &[Test] = &[
    (
        "disagreeing_providers_exit_1_with_a_report_and_viz_events",
        disagreeing_providers_exit_1_with_a_report_and_viz_events,
    ),
    (
        "providers_on_different_chains_exit_2",
        providers_on_different_chains_exit_2,
    ),
    (
        "api_keys_in_a_literal_url_are_redacted",
        api_keys_in_a_literal_url_are_redacted,
    ),
    (
        "a_write_method_is_refused_before_anything_is_sent",
        a_write_method_is_refused_before_anything_is_sent,
    ),
    (
        "bad_arguments_are_named_plainly",
        bad_arguments_are_named_plainly,
    ),
    (
        "viz_without_a_companion_completes_and_suggests_html",
        viz_without_a_companion_completes_and_suggests_html,
    ),
    (
        "the_documented_examples_run_against_stubs",
        the_documented_examples_run_against_stubs,
    ),
];

fn main() {
    common::viz_stub::main(TESTS);
}

/// A stub mainnet node at `head` (chain `chain`), on its own runtime.
struct Node {
    server: JsonrpcHttpServer,
    _runtime: tokio::runtime::Runtime,
}

impl Node {
    fn start(chain: &'static str, head: u64, balance: &'static str) -> Self {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .unwrap();
        let server = runtime.block_on(JsonrpcHttpServer::start(move |req, _| {
            let body = req.json();
            let method = body["method"].as_str().unwrap_or_default();
            let n = body["params"][0]
                .as_str()
                .and_then(|s| s.strip_prefix("0x"))
                .and_then(|s| u64::from_str_radix(s, 16).ok())
                .unwrap_or(head);
            let result = match method {
                "eth_chainId" => json!(chain),
                "web3_clientVersion" => json!("Geth/v1.14.11-stable/linux-amd64/go1.23.2"),
                "eth_blockNumber" => json!(format!("{head:#x}")),
                "eth_getBlockByNumber" => json!({
                    "number": format!("{n:#x}"), "hash": format!("0x{n:064x}"),
                    "timestamp": "0x6650a000", "gasUsed": "0x1", "gasLimit": "0x2",
                    "transactions": []
                }),
                "eth_getBalance" => json!(balance),
                "debug_traceCall" => json!({"type": "CALL", "to": "0xa0b86991c6218b36c1d19d4a2e9eb0ce3606eb48", "gasUsed": "0x5208"}),
                m if m.starts_with("trace_") => {
                    return RpcReply::json(&json!({"jsonrpc": "2.0", "id": body["id"],
                        "error": {"code": -32601, "message": format!("the method {m} does not exist/is not available")}}));
                }
                _ => json!("0x0"),
            };
            RpcReply::json(&json!({"jsonrpc": "2.0", "id": body["id"], "result": result}))
        }));
        Self {
            server,
            _runtime: runtime,
        }
    }

    fn url(&self) -> String {
        self.server.url("")
    }

    fn methods(&self) -> Vec<String> {
        self.server
            .hits()
            .iter()
            .map(|h| h.json()["method"].as_str().unwrap_or_default().to_owned())
            .collect()
    }
}

/// A workspace with environments `mainnet-a` and `mainnet-b` whose `rpc`
/// is `node_a`/`node_b` with the secret `key` (the canary) in the path.
fn workspace(a: &Node, b: &Node) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    write(
        root,
        "rustman.toml",
        "[workspace]\nname = \"t\"\nversion = 1\n",
    );
    write(root, ".gitignore", ".rustman/\n");
    for (name, node) in [("mainnet-a", a), ("mainnet-b", b)] {
        let env = format!(
            "variables:\n  rpc: \"{}/v3/{{{{key}}}}\"\nsecrets: [key]\n",
            node.url()
        );
        write(root, &format!("environments/{name}.yaml"), &env);
        rustman(root)
            .args(["secret", "set", "key", "--env", name])
            .write_stdin(CANARY)
            .assert()
            .success();
    }
    dir
}

struct Outcome {
    code: Option<i32>,
    stdout: String,
    stderr: String,
}

fn probe(dir: &Path, args: &[&str], envs: &[(&str, &str)]) -> Outcome {
    let mut cmd = rustman(dir);
    cmd.args(["evm", "probe"])
        .args(args)
        .env_remove("RUSTMAN_VIZ");
    for (k, v) in envs {
        cmd.env(k, v);
    }
    let output = cmd.output().unwrap();
    let outcome = Outcome {
        code: output.status.code(),
        stdout: String::from_utf8(output.stdout).unwrap(),
        stderr: String::from_utf8(output.stderr).unwrap(),
    };
    for text in [&outcome.stdout, &outcome.stderr] {
        assert!(!text.contains(CANARY), "{text}");
    }
    outcome
}

fn disagreeing_providers_exit_1_with_a_report_and_viz_events() {
    let a = Node::start("0x1", HEAD, "0xde0b6b3a7640000");
    let b = Node::start("0x1", HEAD, "0x2");
    let ws = workspace(&a, &b);
    let log = ws.path().join("viz.log");
    let stub_exe = std::env::current_exe().unwrap();
    let mut args = vec![
        "mainnet-a",
        "mainnet-b",
        "--report",
        "json=out/probe.json",
        "--report",
        "html=out/probe.html",
        "--call",
        "0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48:0x18160ddd",
        "--viz",
    ];
    args.extend(FAST_DRIFT);
    let out = probe(
        ws.path(),
        &args,
        &[
            ("RUSTMAN_VIZ", stub_exe.to_str().unwrap()),
            (STUB, "chain_probe"),
            (STUB_LOG, log.to_str().unwrap()),
        ],
    );
    assert_eq!(out.code, Some(1), "{}\n{}", out.stdout, out.stderr);
    assert!(out.stdout.contains("evm-state-mismatch"), "{}", out.stdout);
    assert!(out.stdout.contains("archive"), "{}", out.stdout);
    check_probe_reports(ws.path());
    check_viz_events(&log, &out.stderr);
    for node in [&a, &b] {
        assert!(node.methods().iter().all(|m| !m.starts_with("eth_send")));
    }
}

/// The JSON and HTML reports of the disagreeing providers: valid, the
/// mismatch a high finding, environments named, the secret hidden.
fn check_probe_reports(root: &Path) {
    let text = std::fs::read(root.join("out/probe.json")).unwrap();
    assert!(!String::from_utf8_lossy(&text).contains(CANARY));
    let report = valid_report(&text);
    assert_eq!(report["kind"], "exploration");
    assert_eq!(report["subject"], "evm-rpc");
    assert_eq!(report["findings"][0]["severity"], "high");
    let html = std::fs::read_to_string(root.join("out/probe.html")).unwrap();
    assert!(!html.contains(CANARY));
    let providers = report["evm"]["providers"].as_array().unwrap();
    assert_eq!(providers[0]["environment"], "mainnet-a");
    let shown = providers[0]["endpoint"].as_str().unwrap();
    assert!(shown.ends_with("/v3/{{key}}"), "{shown}");
}

/// The chain-probe events the stub companion received, without the secret.
fn check_viz_events(log: &Path, stderr: &str) {
    let lines = stub_lines(log);
    let kinds: Vec<&str> = lines.iter().filter_map(|l| l["type"].as_str()).collect();
    assert_eq!(kinds.first(), Some(&"open_view"), "{stderr}");
    assert_eq!(lines[0]["view"], "chain_probe");
    for kind in [
        "probe_latency",
        "chain_block",
        "call_trace",
        "finding",
        "exploration_finished",
    ] {
        assert!(kinds.contains(&kind), "{kind} missing: {kinds:?}");
    }
    assert_eq!(kinds.last(), Some(&"close"));
    let raw = std::fs::read_to_string(log).unwrap();
    assert!(!raw.contains(CANARY));
}

fn providers_on_different_chains_exit_2() {
    let mainnet = Node::start("0x1", HEAD, "0x1");
    let sepolia = Node::start("0xaa36a7", HEAD, "0x1");
    let dir = tempfile::tempdir().unwrap();
    let out = probe(dir.path(), &[&mainnet.url(), &sepolia.url()], &[]);
    assert_eq!(out.code, Some(2), "{}", out.stderr);
    assert!(
        out.stderr
            .contains("providers report different chains (1, 11155111)"),
        "{}",
        out.stderr
    );
}

fn a_write_method_is_refused_before_anything_is_sent() {
    let node = Node::start("0x1", HEAD, "0x1");
    let dir = tempfile::tempdir().unwrap();
    let out = probe(
        dir.path(),
        &[&node.url(), "--methods", "eth_call,eth_sendRawTransaction"],
        &[],
    );
    assert_eq!(out.code, Some(4), "{}", out.stderr);
    assert!(out.stderr.contains("signing gate"), "{}", out.stderr);
    assert!(node.methods().is_empty());
}

fn bad_arguments_are_named_plainly() {
    let dir = tempfile::tempdir().unwrap();
    let out = probe(dir.path(), &["ftp://x"], &[]);
    assert_eq!(out.code, Some(2), "{}", out.stderr);
    assert!(
        out.stderr
            .contains("error: ftp://x: endpoint must be an http, https, ws or wss URL"),
        "{}",
        out.stderr
    );
    assert!(!out.stderr.contains("ad-hoc"), "{}", out.stderr);
    let out = probe(
        dir.path(),
        &["http://127.0.0.1:1", "--methods", "eth_foo"],
        &[],
    );
    assert_eq!(out.code, Some(2), "{}", out.stderr);
    assert!(
        out.stderr
            .contains("'eth_foo' is not a probe method; probe methods: eth_chainId"),
        "{}",
        out.stderr
    );
    let out = probe(dir.path(), &["http://127.0.0.1:1", "--budget", "0"], &[]);
    assert_eq!(out.code, Some(2), "{}", out.stderr);
    assert!(out.stderr.contains("--budget"), "{}", out.stderr);
}

fn api_keys_in_a_literal_url_are_redacted() {
    const PATH_KEY: &str = "9aa3d95b3bc440fa88ea12eaa4456161";
    let dir = tempfile::tempdir().unwrap();
    let url = format!("http://127.0.0.1:1/v3/{PATH_KEY}?apikey=QKEY456");
    let out = probe(dir.path(), &[&url, "--budget", "5"], &[]);
    assert_eq!(out.code, Some(3), "{}", out.stderr);
    assert!(out.stderr.contains("eth_chainId"), "{}", out.stderr);
    for text in [&out.stdout, &out.stderr] {
        assert!(!text.contains(PATH_KEY), "{text}");
        assert!(!text.contains("QKEY456"), "{text}");
    }
    // A probe that runs: the report's endpoint hides the keys and the
    // userinfo, and the warning suggesting a secret is printed once.
    let node = Node::start("0x1", HEAD, "0x1");
    let base = node.url();
    let host = base.strip_prefix("http://").unwrap();
    let url = format!("http://user:hunter2@{host}/v3/{PATH_KEY}?access_token=TOK789&chain=1");
    let mut args = vec![
        url.as_str(),
        "--budget",
        "40",
        "--report",
        "json=probe.json",
    ];
    args.extend(FAST_DRIFT);
    let out = probe(dir.path(), &args, &[]);
    assert!(matches!(out.code, Some(0 | 1)), "{}", out.stderr);
    let report = std::fs::read_to_string(dir.path().join("probe.json")).unwrap();
    for text in [&out.stdout, &out.stderr, &report] {
        for secret in [PATH_KEY, "TOK789", "hunter2"] {
            assert!(!text.contains(secret), "{secret} in {text}");
        }
    }
    assert!(report.contains("chain=1"), "{report}");
    assert_eq!(
        out.stderr.matches("looks like an API key").count(),
        1,
        "{}",
        out.stderr
    );
    assert!(out.stderr.contains("{{secret:"), "{}", out.stderr);
}

fn viz_without_a_companion_completes_and_suggests_html() {
    let node = Node::start("0x1", HEAD, "0x1");
    let dir = tempfile::tempdir().unwrap();
    let missing = dir.path().join("no-such-viz");
    let out = probe(
        dir.path(),
        &[&node.url(), "--methods", "eth_chainId", "--viz"],
        &[("RUSTMAN_VIZ", missing.to_str().unwrap())],
    );
    assert_eq!(out.code, Some(0), "{}", out.stderr);
    assert!(out.stderr.contains("--report html="), "{}", out.stderr);
    assert!(
        out.stdout.contains("evm probe: Ethereum Mainnet (1)"),
        "{}",
        out.stdout
    );
    // A companion without the chain_probe view gets nothing.
    let log = dir.path().join("viz.log");
    let stub_exe = std::env::current_exe().unwrap();
    let out = probe(
        dir.path(),
        &[&node.url(), "--methods", "eth_chainId", "--viz"],
        &[
            ("RUSTMAN_VIZ", stub_exe.to_str().unwrap()),
            (STUB, "bench"),
            (STUB_LOG, log.to_str().unwrap()),
        ],
    );
    assert_eq!(out.code, Some(0), "{}", out.stderr);
    assert!(
        out.stderr.contains("viz companion has no chain_probe view"),
        "{}",
        out.stderr
    );
    assert!(out.stderr.contains("--report html="), "{}", out.stderr);
    let lines = stub_lines(&log);
    assert!(lines.iter().all(|l| l["type"] == "close"), "{lines:?}");
}

/// Every `rustman evm probe` line of docs/evm-probe.md, with the example
/// hosts replaced by stub nodes.
fn the_documented_examples_run_against_stubs() {
    let a = Node::start("0x1", HEAD, "0x1");
    let b = Node::start("0x1", HEAD, "0x1");
    let ws = workspace(&a, &b);
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/evm-probe.md");
    let doc = std::fs::read_to_string(path).unwrap();
    let examples: Vec<&str> = doc
        .lines()
        .filter(|l| l.starts_with("rustman evm probe "))
        .collect();
    assert!(examples.len() >= 6, "{examples:?}");
    for example in examples {
        let line = example
            .replace("https://rpc-a.example", &a.url())
            .replace("https://rpc-b.example", &b.url());
        let mut args: Vec<&str> = line.split_whitespace().skip(3).collect();
        args.extend(FAST_DRIFT);
        let out = probe(ws.path(), &args, &[]);
        assert_eq!(
            out.code,
            Some(0),
            "{example}\n{}\n{}",
            out.stdout,
            out.stderr
        );
    }
}
