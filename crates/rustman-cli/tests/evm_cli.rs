//! `rustman evm call|decode|logs|tx|block|ens|fork` (add-evm-toolkit task
//! 4.2) against stub nodes on loopback and recorded mainnet answers: exit
//! codes of the ROADMAP table, `--json`, `--env`/`--rpc`, assertions and
//! captures through `rustman run`, chain pinning, and redaction of a secret
//! held in the endpoint URL. No test touches the internet.

mod common;

use std::path::{Path, PathBuf};

use common::{rustman, write};
use rustman_test_support::{JsonrpcHttpServer, RpcReply};
use serde_json::{Value, json};

const USDC: &str = "0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48";
const VITALIK: &str = "0xd8dA6BF26964aF9D7eEd9e03E53415D37aA96045";
const TRANSFER_CALLDATA: &str = "0xa9059cbb000000000000000000000000d8da6bf26964af9d7eed9e03e53415d37aa96045000000000000000000000000000000000000000000000000000000000000000a";
const CANARY: &str = "evmCANARYkey9f3a";

/// A node error: code, message and data.
type NodeError = (i64, String, Option<Value>);

/// The answer of a stub node to one call.
type Answer = Result<Value, NodeError>;

struct Node {
    _runtime: tokio::runtime::Runtime,
    server: JsonrpcHttpServer,
}

impl Node {
    fn start(answer: impl Fn(&str, &Value) -> Answer + Send + Sync + 'static) -> Self {
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let server = runtime.block_on(JsonrpcHttpServer::start(move |req, _| {
            let body = req.json();
            let method = body["method"].as_str().unwrap_or_default();
            let reply = match answer(method, &body["params"]) {
                Ok(result) => json!({"jsonrpc": "2.0", "id": body["id"], "result": result}),
                Err((code, message, data)) => json!({"jsonrpc": "2.0", "id": body["id"],
                    "error": {"code": code, "message": message, "data": data}}),
            };
            RpcReply::json(&reply)
        }));
        Self {
            _runtime: runtime,
            server,
        }
    }

    /// A mainnet Geth node answering `calls` with `answer`.
    fn mainnet(answer: impl Fn(&str, &Value) -> Option<Answer> + Send + Sync + 'static) -> Self {
        Self::chain("0x1", answer)
    }

    fn chain(
        id: &'static str,
        answer: impl Fn(&str, &Value) -> Option<Answer> + Send + Sync + 'static,
    ) -> Self {
        Self::start(move |method, params| match method {
            "eth_chainId" => Ok(json!(id)),
            "web3_clientVersion" => Ok(json!("Geth/v1.14.11-stable/linux-amd64/go1.23.2")),
            _ => answer(method, params).unwrap_or_else(|| {
                Err((-32601, format!("the method {method} does not exist"), None))
            }),
        })
    }

    /// Answers recorded in `fixture` (relative to rustman-evm's fixtures).
    fn recorded(fixture: &str) -> Self {
        let path = evm_fixture(fixture);
        let calls: Vec<Value> =
            serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
        Self::start(move |method, params| {
            let found = calls.iter().find(|c| {
                c["method"] == method && (c.get("params").is_none() || &c["params"] == params)
            });
            match found {
                Some(call) if call.get("error").is_some() => {
                    let e = &call["error"];
                    Err((
                        e["code"].as_i64().unwrap(),
                        e["message"].as_str().unwrap().to_owned(),
                        None,
                    ))
                }
                Some(call) => Ok(call["result"].clone()),
                None => Err((-32601, format!("the method {method} does not exist"), None)),
            }
        })
    }

    fn url(&self) -> String {
        self.server.url("/")
    }

    fn methods(&self) -> Vec<String> {
        self.server
            .hits()
            .iter()
            .map(|h| h.json()["method"].as_str().unwrap_or_default().to_owned())
            .collect()
    }
}

fn evm_fixture(path: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../rustman-evm/tests/fixtures")
        .join(path)
}

fn word(hex: &str) -> String {
    format!("{hex:0>64}")
}

/// An ERC-20 that holds 250 USDC for anyone, with 6 decimals.
fn token(method: &str, params: &Value) -> Option<Answer> {
    if method != "eth_call" {
        return None;
    }
    let data = params[0]["data"].as_str().unwrap_or_default();
    let symbol = format!("0x{}{}{:0<64}", word("20"), word("4"), "55534443");
    let answer = match &data[..10.min(data.len())] {
        "0x70a08231" => format!("0x{}", word("ee6b280")),
        "0x313ce567" => format!("0x{}", word("6")),
        "0x95d89b41" => symbol,
        "0x8da5cb5b" => format!("0x{}", word(&VITALIK[2..].to_ascii_lowercase())),
        _ => {
            return Some(Err((
                3,
                "execution reverted".into(),
                Some(json!(
                    "0x4e487b710000000000000000000000000000000000000000000000000000000000000012"
                )),
            )));
        }
    };
    Some(Ok(json!(answer)))
}

fn output(cmd: &mut assert_cmd::Command) -> (Option<i32>, String, String) {
    let out = cmd.output().unwrap();
    (
        out.status.code(),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

/// A workspace with `dev` pointing `rpc` at `url`, pinned to `chain_id`.
fn workspace(url: &str, chain_id: Option<u64>) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    write(
        dir.path(),
        "rustman.toml",
        "[workspace]\nname = \"t\"\nversion = 1\n",
    );
    let pin = chain_id
        .map(|id| format!("chain_id: {id}\n"))
        .unwrap_or_default();
    write(
        dir.path(),
        "environments/dev.yaml",
        &format!("{pin}variables:\n  rpc: {url}\n"),
    );
    write(dir.path(), ".gitignore", ".rustman/\n");
    dir
}

#[test]
fn decode_works_offline() {
    // Scenario "Decode calldata offline": no workspace, no node.
    let dir = tempfile::tempdir().unwrap();
    let (code, stdout, stderr) =
        output(rustman(dir.path()).args(["evm", "decode", "--abi", "erc20", TRANSFER_CALLDATA]));
    assert_eq!(code, Some(0), "{stderr}");
    assert_eq!(stdout.trim(), format!("transfer(to={VITALIK}, amount=10)"));
    let (_, json_out, _) = output(rustman(dir.path()).args([
        "evm",
        "decode",
        "--abi",
        "erc20",
        "--json",
        TRANSFER_CALLDATA,
    ]));
    let value: Value = serde_json::from_str(&json_out).unwrap();
    assert_eq!(value["function"], "transfer(address,uint256)");
    assert_eq!(value["args"]["amount"], "10");
    let panic = "0x4e487b710000000000000000000000000000000000000000000000000000000000000012";
    let (_, stdout, _) =
        output(rustman(dir.path()).args(["evm", "decode", "--abi", "erc20", panic]));
    assert_eq!(
        stdout.trim(),
        "reverted: Panic(0x12) division or modulo by zero"
    );
    let (code, _, stderr) =
        output(rustman(dir.path()).args(["evm", "decode", "--abi", "erc20", "0xdeadbeef00"]));
    assert_eq!(code, Some(2), "{stderr}");
    assert!(
        stderr.contains("no function or error of the ABI has selector 0xdeadbeef"),
        "{stderr}"
    );
}

#[test]
fn an_ad_hoc_call_prints_token_units_and_json() {
    let node = Node::mainnet(token);
    let dir = tempfile::tempdir().unwrap();
    let args = [
        "evm",
        "call",
        "--to",
        USDC,
        "--abi",
        "erc20",
        "--function",
        "balanceOf",
        VITALIK,
        "--rpc",
        &node.url(),
        "--no-history",
    ];
    let (code, stdout, stderr) = output(rustman(dir.path()).args(args));
    assert_eq!(code, Some(0), "{stdout}{stderr}");
    assert!(stdout.contains("250.00 USDC (250000000)"), "{stdout}");
    assert!(
        stderr.contains("Ethereum Mainnet (1) · geth 1.14.11"),
        "{stderr}"
    );
    let (code, stdout, _) = output(rustman(dir.path()).args(args).arg("--json"));
    assert_eq!(code, Some(0));
    let value: Value = serde_json::from_str(&stdout).unwrap();
    assert_eq!(value["endpoint"]["client"], "geth");
    assert_eq!(value["endpoint"]["client_version"], "1.14.11");
    assert_eq!(value["result"]["outputs"]["balance"], "250000000");
    assert_eq!(value["result"]["token"]["symbol"], "USDC");
}

#[test]
fn a_revert_is_decoded_and_checked_by_assertions() {
    let node = Node::mainnet(token);
    let ws = workspace(&node.url(), Some(1));
    let file = |assertions: &str| {
        format!(
            "version: 1\nkind: evm-call\nname: Supply\nrpc: \"{{{{rpc}}}}\"\nto: \"{USDC}\"\nabi: erc20\nfunction: totalSupply\nassertions:\n{assertions}"
        )
    };
    write(
        ws.path(),
        "supply.rman.yaml",
        &file("  - evm: {reverts_with: Panic}\n"),
    );
    let (code, stdout, stderr) =
        output(rustman(ws.path()).args(["evm", "call", "supply", "--env", "dev", "--no-history"]));
    assert_eq!(code, Some(0), "{stdout}{stderr}");
    assert!(
        stdout.contains("reverted: Panic(0x12) division or modulo by zero"),
        "{stdout}"
    );
    write(
        ws.path(),
        "supply.rman.yaml",
        &file("  - evm: {output: 0, gt: 0}\n"),
    );
    let (code, _, stderr) =
        output(rustman(ws.path()).args(["evm", "call", "supply", "--env", "dev", "--no-history"]));
    assert_eq!(code, Some(1), "{stderr}");
    assert!(
        stderr.contains("the call reverted: Panic(0x12)"),
        "{stderr}"
    );
}

#[test]
fn assertions_fail_with_exit_1_and_captures_feed_later_requests() {
    let node = Node::mainnet(token);
    let ws = workspace(&node.url(), Some(1));
    let owner = format!(
        "version: 1\nkind: evm-call\nname: Owner\nrpc: \"{{{{rpc}}}}\"\nto: \"{USDC}\"\nabi: owner.json\nfunction: owner\ncaptures:\n  - {{var: owner, from: evm_output, output: 0}}\n"
    );
    write(ws.path(), "calls/1-owner.rman.yaml", &owner);
    write(
        ws.path(),
        "calls/owner.json",
        r#"[{"type":"function","name":"owner","stateMutability":"view","inputs":[],"outputs":[{"name":"","type":"address"}]}]"#,
    );
    let balance = format!(
        "version: 1\nkind: evm-call\nname: Balance\nrpc: \"{{{{rpc}}}}\"\nto: \"{USDC}\"\nabi: erc20\nfunction: balanceOf\nargs: [\"{{{{owner}}}}\"]\nassertions:\n  - evm: {{output: 0, gte: \"250 USDC\"}}\n"
    );
    write(ws.path(), "calls/2-balance.rman.yaml", &balance);
    let (code, stdout, stderr) =
        output(rustman(ws.path()).args(["run", "calls", "--env", "dev", "--no-history"]));
    assert_eq!(code, Some(0), "{stdout}{stderr}");
    let sent_owner = node.server.hits().iter().any(|h| {
        let data = h.json()["params"][0]["data"]
            .as_str()
            .unwrap_or_default()
            .to_owned();
        data.starts_with("0x70a08231") && data.ends_with(&VITALIK[2..].to_ascii_lowercase())
    });
    assert!(sent_owner, "balanceOf used the captured owner");
    let failing = balance.replace("\"250 USDC\"", "\"1 ether\"");
    write(ws.path(), "calls/2-balance.rman.yaml", &failing);
    let (code, stdout, stderr) = output(rustman(ws.path()).args([
        "evm",
        "call",
        "calls/2-balance.rman.yaml",
        "--env",
        "dev",
        "--no-history",
        "--var",
        &format!("owner={VITALIK}"),
    ]));
    assert_eq!(code, Some(1), "{stdout}{stderr}");
    assert!(
        // A token amount is shown in the token's units, whatever unit the
        // expected value was written in.
        stderr.contains("evm output '0': expected >= 1000000000000 USDC, got 250 USDC"),
        "{stderr}"
    );
}

const TRANSFER_TOPIC: &str = "0xddf252ad1be2c89b69c2b068fc378daa952ba7f163c4a11628f55a4df523b3ef";

/// A token whose `transfer` returns true and whose simulation (with
/// `debug_traceCall`, when `traces`) logs `Transfer(VITALIK, VITALIK, 1)`.
fn transferring(traces: bool) -> impl Fn(&str, &Value) -> Option<Answer> {
    move |method, _| {
        let holder = format!("0x{}", word(&VITALIK[2..].to_ascii_lowercase()));
        match method {
            "eth_call" => Some(Ok(json!(format!("0x{}", word("1"))))),
            "debug_traceCall" if traces => Some(Ok(json!({
                "type": "CALL", "to": USDC.to_ascii_lowercase(),
                "logs": [{
                    "address": USDC.to_ascii_lowercase(),
                    "topics": [TRANSFER_TOPIC, holder, holder],
                    "data": format!("0x{}", word("1")),
                }]
            }))),
            _ => None,
        }
    }
}

#[test]
fn emits_and_event_captures_read_the_simulated_call() {
    // Regression: a read-only call had no events, so `emits` and
    // `evm_event` could never succeed on an evm-call.
    let pay = format!(
        "version: 1\nkind: evm-call\nname: Pay\nrpc: \"{{{{rpc}}}}\"\nfrom: \"{VITALIK}\"\nto: \"{USDC}\"\nabi: erc20\nfunction: transfer\nargs: [\"{VITALIK}\", \"1\"]\nassertions:\n  - evm: {{emits: \"Transfer(address,address,uint256)\", where: {{value: 1}}}}\ncaptures:\n  - {{var: to, from: evm_event, event: \"Transfer(address,address,uint256)\", field: to}}\n"
    );
    let node = Node::mainnet(transferring(true));
    let ws = workspace(&node.url(), Some(1));
    write(ws.path(), "calls/1-pay.rman.yaml", &pay);
    let balance = format!(
        "version: 1\nkind: evm-call\nname: Balance\nrpc: \"{{{{rpc}}}}\"\nto: \"{USDC}\"\nabi: erc20\nfunction: balanceOf\nargs: [\"{{{{to}}}}\"]\n"
    );
    write(ws.path(), "calls/2-balance.rman.yaml", &balance);
    let (code, stdout, stderr) =
        output(rustman(ws.path()).args(["run", "calls", "--env", "dev", "--no-history"]));
    assert_eq!(code, Some(0), "{stdout}{stderr}");
    let used_capture = node.server.hits().iter().any(|h| {
        let data = h.json()["params"][0]["data"]
            .as_str()
            .unwrap_or_default()
            .to_owned();
        data.starts_with("0x70a08231") && data.ends_with(&VITALIK[2..].to_ascii_lowercase())
    });
    assert!(used_capture, "balanceOf used the captured event field");
    assert!(node.methods().iter().any(|m| m == "debug_traceCall"));
    // Without debug_traceCall: a warning, and the assertion fails.
    let node = Node::mainnet(transferring(false));
    let ws = workspace(&node.url(), Some(1));
    write(ws.path(), "pay.rman.yaml", &pay);
    let (code, stdout, stderr) =
        output(rustman(ws.path()).args(["run", "pay.rman.yaml", "--env", "dev", "--no-history"]));
    assert_eq!(code, Some(1), "{stdout}{stderr}");
    let all = format!("{stdout}{stderr}");
    assert!(
        all.contains("events of a call need debug_traceCall"),
        "{all}"
    );
}

#[test]
fn a_chain_mismatch_is_refused_with_exit_4() {
    // Scenario "Mismatch blocks the request".
    let node = Node::mainnet(token);
    let ws = workspace(&node.url(), Some(11_155_111));
    std::fs::rename(
        ws.path().join("environments/dev.yaml"),
        ws.path().join("environments/sepolia.yaml"),
    )
    .unwrap();
    let args = [
        "evm",
        "call",
        "--to",
        USDC,
        "--abi",
        "erc20",
        "--function",
        "totalSupply",
        "--env",
        "sepolia",
        "--no-history",
    ];
    let (code, _, stderr) = output(rustman(ws.path()).args(args));
    assert_eq!(code, Some(4), "{stderr}");
    assert!(stderr.contains(
        "chain mismatch: environment 'sepolia' expects 11155111 (Sepolia), endpoint reports 1 (Ethereum Mainnet)"
    ), "{stderr}");
    assert_eq!(
        node.methods(),
        ["eth_chainId"],
        "no call beyond eth_chainId"
    );
    let (code, _, stderr) = output(rustman(ws.path()).args(["evm", "block", "--env", "sepolia"]));
    assert_eq!(code, Some(4), "{stderr}");
    // Regression: `rustman run` exited 3 on the same mismatch.
    let file = format!(
        "version: 1\nkind: evm-call\nname: Supply\nrpc: \"{{{{rpc}}}}\"\nto: \"{USDC}\"\nabi: erc20\nfunction: totalSupply\n"
    );
    write(ws.path(), "supply.rman.yaml", &file);
    let (code, stdout, stderr) = output(rustman(ws.path()).args([
        "run",
        "supply.rman.yaml",
        "--env",
        "sepolia",
        "--no-history",
    ]));
    assert_eq!(code, Some(4), "{stdout}{stderr}");
    assert!(
        format!("{stdout}{stderr}").contains("chain mismatch"),
        "{stdout}{stderr}"
    );
}

#[test]
fn tx_decodes_a_recorded_usdc_transfer() {
    let node = Node::recorded("mainnet/usdc-transfer.json");
    let dir = tempfile::tempdir().unwrap();
    let hash = "0xdeed52ec1e92d0ca8834284f39ed5e37ff6106b6bb09a9dc37fb6d1c01b0a8f6";
    let (code, stdout, stderr) =
        output(rustman(dir.path()).args(["evm", "tx", hash, "--rpc", &node.url()]));
    assert_eq!(code, Some(0), "{stderr}");
    assert!(stdout.contains("value=250.00 USDC"), "{stdout}");
    let (_, json_out, _) =
        output(rustman(dir.path()).args(["evm", "tx", hash, "--json", "--rpc", &node.url()]));
    let value: Value = serde_json::from_str(&json_out).unwrap();
    assert_eq!(value["status"], "success");
    assert_eq!(value["endpoint"]["chain_id"], 1);
    let unknown = format!("0x{}", "11".repeat(32));
    let pending = Node::mainnet(|method, _| {
        method
            .starts_with("eth_getTransaction")
            .then(|| Ok(Value::Null))
    });
    let (code, _, stderr) =
        output(rustman(dir.path()).args(["evm", "tx", &unknown, "--rpc", &pending.url()]));
    assert_eq!(code, Some(1), "{stderr}");
    assert!(stderr.contains("transaction not found"), "{stderr}");
}

#[test]
fn block_prints_fields_and_json() {
    let node = Node::mainnet(|method, params| match method {
        "eth_getBlockByNumber" if params[0] == "latest" => Some(Ok(json!({
            "number": "0x10", "hash": "0xaa", "parentHash": "0xbb", "timestamp": "0x5",
            "gasUsed": "0x5208", "gasLimit": "0x1c9c380", "transactions": []
        }))),
        "eth_getBlockByNumber" => Some(Ok(Value::Null)),
        _ => None,
    });
    let dir = tempfile::tempdir().unwrap();
    let (code, stdout, stderr) =
        output(rustman(dir.path()).args(["evm", "block", "--rpc", &node.url()]));
    assert_eq!(code, Some(0), "{stderr}");
    assert!(stdout.contains("number: 16"), "{stdout}");
    let (_, json_out, _) = output(rustman(dir.path()).args([
        "evm",
        "block",
        "latest",
        "--json",
        "--rpc",
        &node.url(),
    ]));
    let value: Value = serde_json::from_str(&json_out).unwrap();
    assert_eq!(value["gas_used"], "21000");
    let (code, _, stderr) =
        output(rustman(dir.path()).args(["evm", "block", "99", "--rpc", &node.url()]));
    assert_eq!(code, Some(1), "{stderr}");
    assert!(stderr.contains("block not found"), "{stderr}");
    let (code, _, stderr) =
        output(rustman(dir.path()).args(["evm", "block", "newest", "--rpc", &node.url()]));
    assert_eq!(code, Some(2), "{stderr}");
}

fn transfer_log(removed: bool) -> Value {
    json!({
        "address": USDC,
        "topics": [
            "0xddf252ad1be2c89b69c2b068fc378daa952ba7f163c4a11628f55a4df523b3ef",
            format!("0x{}", word(&VITALIK[2..].to_ascii_lowercase())),
            format!("0x{}", word("ab")),
        ],
        "data": format!("0x{}", word("ee6b280")),
        "logIndex": "0x1",
        "removed": removed,
    })
}

#[test]
fn logs_query_a_bounded_range_and_stream_by_polling() {
    let node = Node::mainnet(|method, params| match method {
        "eth_getLogs" => Some(Ok(json!([transfer_log(false)]))),
        "eth_newFilter" => Some(Ok(json!("0x1"))),
        "eth_getFilterChanges" => Some(Ok(json!([transfer_log(true), transfer_log(false)]))),
        "eth_uninstallFilter" => Some(Ok(json!(true))),
        "eth_call" => token(method, params),
        _ => None,
    });
    let dir = tempfile::tempdir().unwrap();
    let url = node.url();
    let event = "Transfer(address,address,uint256)";
    // Regression: the agent tool's 10,000-block cap also applied here.
    let (code, _, stderr) = output(rustman(dir.path()).args([
        "evm",
        "logs",
        "--from-block",
        "0",
        "--to-block",
        "50000",
        "--rpc",
        &url,
    ]));
    assert_eq!(code, Some(0), "{stderr}");
    assert!(node.methods().contains(&"eth_getLogs".to_owned()));
    let (code, _, stderr) = output(rustman(dir.path()).args([
        "evm",
        "logs",
        "--from-block",
        "9",
        "--to-block",
        "8",
        "--rpc",
        &url,
    ]));
    assert_eq!(code, Some(2), "{stderr}");
    let (code, stdout, stderr) = output(rustman(dir.path()).args([
        "evm",
        "logs",
        "--address",
        USDC,
        "--event",
        event,
        "--from-block",
        "100",
        "--to-block",
        "200",
        "--rpc",
        &url,
    ]));
    assert_eq!(code, Some(0), "{stderr}");
    assert!(stdout.contains("Transfer(from="), "{stdout}");
    assert!(stdout.contains("value=250.00 USDC"), "{stdout}");
    // Scenarios "HTTP polling fallback" and "Reorg marked".
    let (code, stdout, stderr) = output(rustman(dir.path()).args([
        "evm",
        "logs",
        "--address",
        USDC,
        "--event",
        event,
        "--max-events",
        "1",
        "--poll-interval",
        "50ms",
        "--duration",
        "20s",
        "--rpc",
        &url,
    ]));
    assert_eq!(code, Some(0), "{stderr}");
    assert!(
        stderr.contains("polling every 50ms (no WebSocket)"),
        "{stderr}"
    );
    let lines: Vec<&str> = stdout.lines().collect();
    assert_eq!(lines.len(), 2, "{stdout}");
    assert!(lines[0].starts_with("REORGED "), "{stdout}");
    assert!(stderr.contains("1 event(s), 1 reorged"), "{stderr}");
}

#[test]
fn ens_resolves_from_recorded_answers_and_needs_mainnet() {
    let node = Node::recorded("mainnet/ens.json");
    let dir = tempfile::tempdir().unwrap();
    let (code, stdout, stderr) =
        output(rustman(dir.path()).args(["evm", "ens", "vitalik.eth", "--rpc", &node.url()]));
    assert_eq!(code, Some(0), "{stderr}");
    assert_eq!(stdout.trim(), VITALIK);
    let sepolia = Node::chain("0xaa36a7", |_, _| None);
    let (code, _, stderr) =
        output(rustman(dir.path()).args(["evm", "ens", "vitalik.eth", "--rpc", &sepolia.url()]));
    assert_eq!(code, Some(2), "{stderr}");
    assert!(
        stderr.contains("ENS needs a mainnet RPC; set [evm] ens_rpc"),
        "{stderr}"
    );
}

#[test]
fn an_unresolved_ens_rpc_is_reported_not_hidden() {
    let sepolia = Node::chain("0xaa36a7", |_, _| None);
    let ws = workspace(&sepolia.url(), None);
    let toml = "[workspace]\nname = \"t\"\nversion = 1\n\n[evm]\nens_rpc = \"{{mainnet_rpc}}\"\n";
    write(ws.path(), "rustman.toml", toml);
    let (code, _, stderr) =
        output(rustman(ws.path()).args(["evm", "ens", "vitalik.eth", "--rpc", &sepolia.url()]));
    assert_eq!(code, Some(2), "{stderr}");
    assert!(
        stderr.contains("[evm] ens_rpc") && stderr.contains("mainnet_rpc"),
        "{stderr}"
    );
    assert!(!stderr.contains("set [evm] ens_rpc"), "{stderr}");
}

#[test]
fn fork_without_anvil_exits_2_with_the_install_hint() {
    let dir = tempfile::tempdir().unwrap();
    let (code, _, stderr) = output(
        rustman(dir.path())
            .env("PATH", "")
            .args(["evm", "fork", "--chain", "mainnet"]),
    );
    assert_eq!(code, Some(2), "{stderr}");
    assert!(
        stderr.contains("anvil not found; install Foundry (https://getfoundry.sh)"),
        "{stderr}"
    );
}

/// A stand-in `anvil` that says it listens, then dies after a second.
#[cfg(unix)]
fn dying_anvil(dir: &std::path::Path) {
    use std::os::unix::fs::PermissionsExt;
    let path = dir.join("anvil");
    let script = "#!/bin/sh\necho 'Listening on 127.0.0.1:45999'\n/bin/sleep 1\nexit 5\n";
    std::fs::write(&path, script).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
}

#[cfg(unix)]
#[test]
fn fork_exits_when_anvil_dies_and_removes_its_environment() {
    // Regression: the fork waited only for Ctrl+C, leaving a fork-<chain>
    // environment that pointed at a dead node.
    let node = Node::mainnet(|_, _| None);
    let ws = workspace(&node.url(), None);
    let bin = tempfile::tempdir().unwrap();
    dying_anvil(bin.path());
    let (code, stdout, stderr) = output(
        rustman(ws.path())
            .env("PATH", bin.path())
            .args(["evm", "fork", "--env", "dev", "--port", "0"]),
    );
    assert_eq!(code, Some(3), "{stdout}{stderr}");
    assert!(stdout.contains("http://127.0.0.1:45999"), "{stdout}");
    assert!(stderr.contains("anvil exited"), "{stderr}");
    assert!(stderr.contains("process list"), "{stderr}");
    assert!(!ws.path().join("environments/fork-dev.yaml").exists());
}

#[test]
fn an_evm_tx_file_is_refused_by_evm_call() {
    let node = Node::mainnet(token);
    let ws = workspace(&node.url(), None);
    let tx = format!(
        "version: 1\nkind: evm-tx\nname: pay\nrpc: \"{{{{rpc}}}}\"\nto: \"{USDC}\"\nabi: erc20\nfunction: transfer\nargs: [\"{VITALIK}\", \"1\"]\n"
    );
    write(ws.path(), "pay.rman.yaml", &tx);
    let (code, _, stderr) = output(rustman(ws.path()).args(["evm", "call", "pay", "--env", "dev"]));
    assert_eq!(code, Some(2), "{stderr}");
    assert!(stderr.contains("is not an evm-call request"), "{stderr}");
    // Scenario "Runner skips evm-tx": skipped, not a failure.
    let (code, stdout, stderr) =
        output(rustman(ws.path()).args(["run", "pay.rman.yaml", "--env", "dev", "--no-history"]));
    assert_eq!(code, Some(0), "{stdout}{stderr}");
    assert!(
        stdout.contains("skipped (signing not requested)"),
        "{stdout}"
    );
    assert!(node.server.hits().is_empty(), "nothing is sent");
}

#[test]
fn a_secret_in_the_endpoint_never_shows() {
    // Canary: the node's URL is a secret holding an API key; it reaches neither
    // stdout, stderr, the JSON output nor the history.
    let node = Node::mainnet(token);
    let ws = workspace("unused", None);
    write(ws.path(), "environments/dev.yaml", "secrets: [rpc]\n");
    rustman(ws.path())
        .args(["secret", "set", "rpc", "--env", "dev"])
        .write_stdin(format!("{}{CANARY}", node.url()))
        .assert()
        .success();
    let file = format!(
        "version: 1\nkind: evm-call\nname: Supply\nrpc: \"{{{{rpc}}}}\"\nto: \"{USDC}\"\nabi: erc20\nfunction: balanceOf\nargs: [\"{VITALIK}\"]\n"
    );
    write(ws.path(), "supply.rman.yaml", &file);
    let mut seen = String::new();
    for args in [
        vec!["evm", "call", "supply", "--env", "dev", "--json"],
        vec!["evm", "call", "supply", "--env", "dev"],
        vec!["evm", "block", "--env", "dev", "--json"],
        vec!["evm", "tx", "0x12", "--env", "dev"],
        vec![
            "run",
            "supply.rman.yaml",
            "--env",
            "dev",
            "--reporter",
            "json:report.json",
        ],
    ] {
        let (_, stdout, stderr) = output(rustman(ws.path()).args(&args).arg("-vvv"));
        seen.push_str(&stdout);
        seen.push_str(&stderr);
    }
    let history = ws.path().join(".rustman/history");
    for entry in walk(&history) {
        seen.push_str(&std::fs::read_to_string(entry).unwrap_or_default());
    }
    seen.push_str(&std::fs::read_to_string(ws.path().join("report.json")).unwrap_or_default());
    assert!(
        node.server.hits().iter().any(|h| h.uri.contains(CANARY)),
        "the key was used"
    );
    assert!(!seen.contains(CANARY), "{seen}");
}

fn walk(dir: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    entries
        .flatten()
        .flat_map(|e| {
            let path = e.path();
            if path.is_dir() {
                walk(&path)
            } else {
                vec![path]
            }
        })
        .collect()
}

#[test]
fn estimate_and_a_degraded_trace() {
    // Scenario "Degrade without tracing": the result, then the note.
    let node = Node::mainnet(|method, params| match method {
        "eth_estimateGas" => Some(Ok(json!("0x5208"))),
        _ => token(method, params),
    });
    let dir = tempfile::tempdir().unwrap();
    let (code, stdout, stderr) = output(rustman(dir.path()).args([
        "evm",
        "call",
        "--to",
        USDC,
        "--abi",
        "erc20",
        "--function",
        "balanceOf",
        VITALIK,
        "--estimate",
        "--trace",
        "--rpc",
        &node.url(),
        "--no-history",
    ]));
    assert_eq!(code, Some(0), "{stdout}{stderr}");
    let lines: Vec<&str> = stdout.lines().collect();
    assert_eq!(
        lines,
        [
            "balance = 250.00 USDC (250000000)",
            "gas estimate: 21000",
            "tracing not supported by geth"
        ]
    );
}

#[test]
fn an_overloaded_function_fails_the_workspace_load() {
    // Scenario "Overloaded function needs signature". Regression: the
    // workspace loaded and `run` reported the file as errored (exit 3).
    let dir = tempfile::tempdir().unwrap();
    write(
        dir.path(),
        "rustman.toml",
        "[workspace]\nname = \"t\"\nversion = 1\n",
    );
    let file = format!(
        "version: 1\nkind: evm-call\nname: Overloaded\nrpc: \"http://127.0.0.1:9\"\nto: \"{USDC}\"\nabi: erc721\nfunction: safeTransferFrom\nargs: [\"{VITALIK}\", \"{VITALIK}\", \"1\"]\n"
    );
    write(dir.path(), "requests/ov.rman.yaml", &file);
    for args in [
        vec!["run", "requests", "--no-history"],
        vec!["evm", "call", "requests/ov.rman.yaml", "--no-history"],
    ] {
        let (code, stdout, stderr) = output(rustman(dir.path()).args(&args));
        assert_eq!(code, Some(2), "{args:?}: {stdout}{stderr}");
        assert!(
            stderr.contains("function 'safeTransferFrom' is overloaded; use a full signature"),
            "{args:?}: {stderr}"
        );
    }
}

#[test]
fn node_text_cannot_send_escape_sequences_to_the_terminal() {
    // Regression: a hostile web3_clientVersion reached the terminal as is.
    let node = Node::start(|method, params| match method {
        "eth_chainId" => Ok(json!("0x1")),
        "web3_clientVersion" => Ok(json!("Geth/v1.0\u{1b}]0;PWNED\u{7}\u{1b}[2Jevil")),
        _ => token(method, params).unwrap_or_else(|| Err((-32601, "no".into(), None))),
    });
    let dir = tempfile::tempdir().unwrap();
    let (code, stdout, stderr) = output(rustman(dir.path()).args([
        "evm",
        "call",
        "--to",
        USDC,
        "--abi",
        "erc20",
        "--function",
        "balanceOf",
        VITALIK,
        "--rpc",
        &node.url(),
        "--no-history",
    ]));
    assert_eq!(code, Some(0), "{stdout}{stderr}");
    let all = format!("{stdout}{stderr}");
    assert!(!all.contains('\u{1b}') && !all.contains('\u{7}'), "{all:?}");
    assert!(all.contains("\\u{1b}]0;PWNED"), "{all:?}");
}

#[test]
fn rpc_call_takes_typed_params_checks_them_and_shows_ether() {
    // Scenarios "Typed balance query", "Bad checksum rejected" and
    // "Balance shown in ether". Regression: there was no --param, and a bad
    // checksum reached the node.
    let node = Node::mainnet(|method, _| match method {
        "eth_getBalance" => Some(Ok(json!("0x0de0b6b3a7640000"))),
        _ => None,
    });
    let dir = tempfile::tempdir().unwrap();
    let url = node.url();
    let (code, stdout, stderr) = output(rustman(dir.path()).args([
        "rpc",
        "call",
        &url,
        "eth_getBalance",
        "--param",
        &format!("address={VITALIK}"),
        "--param",
        "block=latest",
        "--no-history",
    ]));
    assert_eq!(code, Some(0), "{stdout}{stderr}");
    assert_eq!(stdout.trim(), "1 ETH");
    assert!(stderr.contains("1000000000000000000 wei"), "{stderr}");
    assert!(stderr.contains("0x0de0b6b3a7640000"), "{stderr}");
    let sent = node.server.hits()[0].json();
    assert_eq!(sent["method"], "eth_getBalance");
    assert_eq!(sent["params"], json!([VITALIK, "latest"]));
    let bad = "0xd8dA6BF26964aF9D7eEd9e03E53415D37aA96046";
    let (code, _, stderr) = output(rustman(dir.path()).args([
        "rpc",
        "call",
        &url,
        "eth_getBalance",
        "--param",
        &format!("address={bad}"),
        "--no-history",
    ]));
    assert_eq!(code, Some(2), "{stderr}");
    assert!(
        stderr.contains("param 'address': invalid EIP-55 checksum"),
        "{stderr}"
    );
    let raw = format!("[\"{bad}\",\"latest\"]");
    let (code, _, stderr) = output(rustman(dir.path()).args([
        "rpc",
        "call",
        &url,
        "eth_getBalance",
        &raw,
        "--no-history",
    ]));
    // Regression: the same bad param exited 4 as positional JSON and 2
    // with --param.
    assert_eq!(code, Some(2), "{stderr}");
    assert!(
        stderr.contains("param 'address': invalid EIP-55 checksum"),
        "{stderr}"
    );
    assert_eq!(
        node.methods(),
        ["eth_getBalance", "eth_chainId"],
        "nothing else was sent: the chain id names the native currency"
    );
}

#[test]
fn an_unpinned_balance_uses_the_native_symbol_of_the_reported_chain() {
    // Regression: without a pin the balance was always shown in ETH.
    let balance = |method: &str, _: &Value| match method {
        "eth_getBalance" => Some(Ok(json!("0x0de0b6b3a7640000"))),
        _ => None,
    };
    let polygon = Node::chain("0x89", balance);
    let ws = workspace(&polygon.url(), None);
    let args = |url: &str| {
        vec![
            "rpc".to_owned(),
            "call".to_owned(),
            url.to_owned(),
            "eth_getBalance".to_owned(),
            "--param".to_owned(),
            format!("address={VITALIK}"),
            "--env".to_owned(),
            "dev".to_owned(),
            "--no-history".to_owned(),
        ]
    };
    let (code, stdout, stderr) = output(rustman(ws.path()).args(args(&polygon.url())));
    assert_eq!(code, Some(0), "{stdout}{stderr}");
    assert_eq!(stdout.trim(), "1 POL");
    let mainnet = Node::mainnet(balance);
    let ws = workspace(&mainnet.url(), None);
    let toml = "[workspace]\nname = \"t\"\nversion = 1\n\n[evm.chains.1]\nsymbol = \"MYL\"\n";
    write(ws.path(), "rustman.toml", toml);
    let (code, stdout, stderr) = output(rustman(ws.path()).args(args(&mainnet.url())));
    assert_eq!(code, Some(0), "{stdout}{stderr}");
    assert_eq!(stdout.trim(), "1 MYL");
}
