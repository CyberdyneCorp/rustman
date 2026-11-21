//! Calls, traces, streams and forks against a local Anvil (tasks 3.1, 3.2,
//! 3.3 and 3.5). Each test starts its own Anvil; the fixture ERC-20
//! (tests/fixtures/contracts/TestToken.sol) is compiled with `forge build`
//! (offline, solc 0.8.26) and deployed with `forge create` from one of
//! Anvil's unlocked development accounts, so no key is involved.
//!
//! These tests run only with `RUSTMAN_ANVIL=1` (opt-in for local runs) or
//! `RUSTMAN_REQUIRE_ANVIL=1`, so a plain `cargo test` stays hermetic even
//! where Foundry is installed. Opted in but without `anvil` or `forge` on
//! `PATH` (or without solc 0.8.26 installed for Foundry) they skip; the CI
//! Anvil job installs solc and sets `RUSTMAN_REQUIRE_ANVIL=1` to make them
//! required.

mod common;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use alloy_json_abi::{Function, JsonAbi};
use alloy_primitives::Address;
use rustman_core::model::StateOverride;
use rustman_evm::abi::{AbiCache, resolve_function};
use rustman_evm::call::{CallInputs, CallOutcome, CallReport, prepare, run_call};
use rustman_evm::fork::{Anvil, AnvilOptions, TempEnvironment, fork_environment_name};
use rustman_evm::identity::chain_id;
use rustman_evm::stream::{StreamConfig, StreamKind, StreamMode, stream};
use rustman_evm::trace::{TraceResult, TraceSupport, TraceView, trace_call};
use rustman_evm::tx::TokenCache;
use rustman_evm::{Denomination, NodeClient};
use rustman_jsonrpc::SubscriptionEnd;
use serde_json::{Value, json};

use common::{REQUIRE_ANVIL, anvil_enabled, foundry_tool, transport};

/// Anvil's first and second development accounts (unlocked).
const DEPLOYER: &str = "0xf39Fd6e51aad88F6F4ce6aB8827279cffFb92266";
const OTHER: &str = "0x70997970C51812dc3A010C7d01b50e0d17dc79C8";

/// 1000 TST (6 decimals).
const SUPPLY: u64 = 1_000_000_000;

async fn start_anvil(program: &Path, extra: &[&str]) -> Anvil {
    let mut options = AnvilOptions::new(program.to_owned());
    // Anvil picks the port: nothing is reserved and released first.
    options.port = 0;
    options.extra = extra.iter().map(|a| (*a).to_owned()).collect();
    Anvil::start(&options).await.unwrap()
}

/// The solc version of the fixture (its `pragma`).
const SOLC: &str = "0.8.26";

/// A Foundry project holding the fixture token, compiled once per test
/// process with `forge build --offline`.
struct Project {
    dir: tempfile::TempDir,
    abi: Arc<JsonAbi>,
}

/// The compiled fixture, or `None` (the test skips) when `forge` or its
/// solc is missing and Anvil is not required.
fn project(forge: &Path) -> Option<&'static Project> {
    static PROJECT: OnceLock<Option<Project>> = OnceLock::new();
    PROJECT.get_or_init(|| compile(forge)).as_ref()
}

fn compile(forge: &Path) -> Option<Project> {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir(dir.path().join("src")).unwrap();
    let source = common::fixture("contracts/TestToken.sol");
    std::fs::copy(source, dir.path().join("src/TestToken.sol")).unwrap();
    let config = format!("[profile.default]\nsolc = \"{SOLC}\"\noffline = true\n");
    std::fs::write(dir.path().join("foundry.toml"), config).unwrap();
    let output = Command::new(forge)
        .args(["build", "--offline", "--root"])
        .arg(dir.path())
        .output()
        .unwrap();
    if !output.status.success() {
        let shown = String::from_utf8_lossy(&output.stderr).into_owned();
        assert!(
            std::env::var(REQUIRE_ANVIL).as_deref() != Ok("1"),
            "forge build failed ({REQUIRE_ANVIL}=1): {shown}"
        );
        eprintln!("forge build failed (solc {SOLC} missing?): skipping: {shown}");
        return None;
    }
    let artifact = dir.path().join("out/TestToken.sol/TestToken.json");
    let abi = AbiCache::new()
        .parse(&std::fs::read(artifact).unwrap())
        .unwrap();
    Some(Project { dir, abi })
}

/// Deploy the token with `forge create` and return its address.
fn deploy(forge: &Path, project: &Project, url: &str) -> Address {
    let output = Command::new(forge)
        .args(["create", "--root"])
        .arg(project.dir.path())
        .args(["src/TestToken.sol:TestToken", "--rpc-url", url])
        .args(["--unlocked", "--from", DEPLOYER, "--broadcast", "--json"])
        .args(["--constructor-args", &SUPPLY.to_string()])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    let json = &stdout[stdout.find('{').unwrap_or_default()..];
    let created: Value = serde_json::from_str(json).unwrap();
    created["deployedTo"].as_str().unwrap().parse().unwrap()
}

struct Deployed {
    anvil: Anvil,
    abi: Arc<JsonAbi>,
    token: Address,
}

async fn deployed(extra: &[&str]) -> Option<Deployed> {
    let program = foundry_tool("anvil")?;
    let forge: PathBuf = foundry_tool("forge")?;
    let project = project(&forge)?;
    let anvil = start_anvil(&program, extra).await;
    let token = deploy(&forge, project, &anvil.url());
    let abi = project.abi.clone();
    Some(Deployed { anvil, abi, token })
}

impl Deployed {
    fn function(&self, spec: &str) -> Function {
        resolve_function(&self.abi, spec).unwrap().clone()
    }

    async fn call(&self, spec: &str, inputs: CallInputs) -> CallReport {
        let rpc = transport(&self.anvil.url());
        let function = self.function(spec);
        let inputs = CallInputs {
            to: self.token.to_checksum(None),
            ..inputs
        };
        let request = prepare(&inputs, &function, &Denomination::default(), None)
            .await
            .unwrap();
        run_call(&rpc, &request, &function, Some(&self.abi), true)
            .await
            .unwrap()
    }
}

fn args(values: Value) -> CallInputs {
    CallInputs {
        args: values.as_array().unwrap().clone(),
        ..CallInputs::default()
    }
}

#[tokio::test]
async fn calls_decode_outputs_reverts_and_estimates() {
    let Some(node) = deployed(&[]).await else {
        return;
    };
    let rpc = transport(&node.anvil.url());
    let report = node
        .call("balanceOf", args(json!([DEPLOYER])))
        .await
        .with_token(&rpc, 31_337, node.token, &TokenCache::new())
        .await;
    let rendered = report.render();
    assert!(
        rendered.starts_with("0 = 1000.00 TST (1000000000)\ngas estimate: "),
        "{rendered}"
    );
    let too_much = CallInputs {
        from: Some(OTHER.to_owned()),
        ..args(json!([DEPLOYER, "5"]))
    };
    let report = node.call("transfer", too_much).await;
    assert_eq!(
        report.revert().unwrap().to_string(),
        "reverted: InsufficientBalance(available=0, required=5)"
    );
    let report = node.call("ratio", args(json!(["1", "0"]))).await;
    assert_eq!(
        report.revert().unwrap().to_string(),
        "reverted: Panic(0x12) division or modulo by zero"
    );
    let report = node.call("check", args(json!(["200"]))).await;
    assert_eq!(
        report.revert().unwrap().to_string(),
        "reverted: value too large"
    );
}

#[tokio::test]
async fn state_overrides_and_historical_blocks_apply() {
    let Some(node) = deployed(&[]).await else {
        return;
    };
    // totalSupply is storage slot 0.
    let token = node.token.to_checksum(None);
    let overrides: BTreeMap<String, StateOverride> = serde_json::from_value(json!({
        (token): {"storage": {"0x0": "0x2a"}},
        OTHER: {"balance": "100 ether"}
    }))
    .unwrap();
    let inputs = CallInputs {
        state_overrides: Some(overrides),
        ..CallInputs::default()
    };
    let report = node.call("totalSupply", inputs).await;
    assert_eq!(report.to_json()["outputs"], json!({"0": "42"}));
    let report = node.call("totalSupply", CallInputs::default()).await;
    assert_eq!(
        report.to_json()["outputs"],
        json!({"0": SUPPLY.to_string()})
    );
    // At block 0 the token does not exist yet: no output to decode.
    let earliest = CallInputs {
        block: Some("0".into()),
        ..CallInputs::default()
    };
    let report = node.call("totalSupply", earliest).await;
    assert!(
        matches!(&report.outcome, CallOutcome::Returned { data, values: Err(_) } if data.is_empty()),
        "{report:?}"
    );
}

#[tokio::test]
async fn debug_trace_call_gives_a_nested_tree() {
    let Some(node) = deployed(&[]).await else {
        return;
    };
    let rpc = transport(&node.anvil.url());
    let function = node.function("balanceViaSelf");
    let inputs = CallInputs {
        to: node.token.to_checksum(None),
        ..args(json!([DEPLOYER]))
    };
    let request = prepare(&inputs, &function, &Denomination::default(), None)
        .await
        .unwrap();
    let anvil = NodeClient::parse("anvil/v1.0.0");
    let TraceResult::Tree(root) = trace_call(&rpc, &request, &anvil, &TraceSupport::new()).await
    else {
        panic!("Anvil supports debug_traceCall");
    };
    assert_eq!(root.kind, "CALL");
    assert_eq!(root.calls.len(), 1);
    assert_eq!(root.calls[0].kind, "STATICCALL");
    let abi = Arc::clone(&node.abi);
    let abis = move |_| vec![Arc::clone(&abi)];
    let shown = TraceView { abis: &abis }.render(&root);
    assert!(
        shown.contains(&format!("balanceViaSelf(who={DEPLOYER})")),
        "{shown}"
    );
    assert!(
        shown.contains(&format!("  STATICCALL {}", node.token)),
        "{shown}"
    );
}

#[tokio::test]
async fn new_heads_stream_over_websocket() {
    let Some(program) = foundry_tool("anvil") else {
        return;
    };
    let anvil = start_anvil(&program, &["--block-time", "1"]).await;
    let rpc = transport(&anvil.ws_url());
    let config = StreamConfig {
        max_events: Some(2),
        max_duration: Some(Duration::from_secs(30)),
        ..StreamConfig::default()
    };
    assert_eq!(
        StreamMode::of(&anvil.ws_url(), &config),
        StreamMode::Subscription
    );
    let (tx, mut rx) = rustman_evm::stream::item_channel();
    let summary = stream(&rpc, &StreamKind::Heads, &config, &tx)
        .await
        .unwrap();
    assert_eq!(summary.ended, SubscriptionEnd::MaxEvents);
    assert_eq!(summary.events, 2);
    let first = rx.try_recv().unwrap();
    let rustman_evm::stream::StreamItem::Head(head) = first else {
        panic!("not a head");
    };
    assert!(head["number"].is_string(), "{head}");
}

#[tokio::test]
async fn a_fork_starts_registers_its_environment_and_stops() {
    let Some(program) = foundry_tool("anvil") else {
        return;
    };
    let upstream = start_anvil(&program, &[]).await;
    let mut options = AnvilOptions::new(program.clone());
    options.port = 0;
    options.fork_url = Some(upstream.url());
    options.fork_block = Some(0);
    let fork = Anvil::start(&options).await.unwrap();
    let id = chain_id(&transport(&fork.url())).await.unwrap();
    assert_eq!(id, 31_337);
    let root = tempfile::tempdir().unwrap();
    let name = fork_environment_name("anvil", id);
    let env = TempEnvironment::write(root.path(), &name, &fork.url(), id, fork.pid()).unwrap();
    assert_eq!(env.name(), "fork-anvil");
    let text = std::fs::read_to_string(env.path()).unwrap();
    assert!(text.contains(&format!("rpc: {}", fork.url())), "{text}");
    let address = fork.url().trim_start_matches("http://").to_owned();
    fork.stop().await;
    let path = env.path().to_owned();
    drop(env);
    assert!(!path.exists());
    assert!(
        tokio::net::TcpStream::connect(&address).await.is_err(),
        "the fork is stopped"
    );
}

#[test]
fn anvil_tests_are_opt_in() {
    assert!(!anvil_enabled(None, None));
    assert!(!anvil_enabled(Some("0"), Some("")));
    assert!(anvil_enabled(Some("1"), None));
    assert!(anvil_enabled(None, Some("1")));
}
