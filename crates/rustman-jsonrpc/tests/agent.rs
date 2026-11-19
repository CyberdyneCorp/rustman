//! The agent `rpc_call` policy and the method guards (add-jsonrpc-client
//! tasks 7.2 and 7.3): presets, the read-only allowlist, the host
//! allowlist, refused subscriptions, and guards winning over everything,
//! including when a request is sent through the runner.

mod common;

use std::sync::Arc;

use common::{call, target};
use rustman_core::model::SubscriptionSpec;
use rustman_core::run::{RequestStatus, plan};
use rustman_core::safety::{
    Blocked, EnvironmentInfo, Governor, Limits, Origin, Permission, PolicyOptions, SafetyPolicy,
    SafetySettings,
};
use rustman_core::settings::RpcSettings;
use rustman_core::{SecretStore, Workspace};
use rustman_http::GovernedSender;
use rustman_http::runner::{RunContext, RunOptions, Runner};
use rustman_jsonrpc::agent::{
    AgentRefusal, AgentRpcPolicy, AgentRunner, NOT_READ_ONLY, UnknownPreset,
};
use rustman_jsonrpc::guard::{MethodGuard, register_guard, register_preset};
use rustman_test_support::{JsonrpcWsServer, WsFrame};
use serde_json::{Value, json};

const HOST: &str = "http://127.0.0.1:8545";

/// Refuses `eth_sendRawTransaction`, as `add-evm-toolkit` will.
struct NoRawTransactions;

impl MethodGuard for NoRawTransactions {
    fn check(&self, method: &str, _params: Option<&Value>) -> Option<String> {
        (method == "eth_sendRawTransaction")
            .then(|| "eth_sendRawTransaction is refused: transactions need --allow-tx".to_owned())
    }
}

fn settings(methods: &[&str]) -> RpcSettings {
    RpcSettings {
        agent_read_only_methods: methods.iter().map(|m| (*m).to_owned()).collect(),
        ..RpcSettings::default()
    }
}

fn allowed() -> Vec<Origin> {
    vec![Origin::of(HOST).unwrap()]
}

#[test]
fn a_method_outside_the_allowlist_is_refused() {
    // Scenario "Method outside the allowlist".
    let policy = AgentRpcPolicy::from_settings(&settings(&["getblock*"])).unwrap();
    let read = target(HOST, vec![call("getblockcount")], false);
    assert_eq!(policy.check(&read, &allowed()), Ok(()));
    let write = target(HOST, vec![call("sendtoaddress")], false);
    let refused = policy.check(&write, &allowed()).unwrap_err();
    assert_eq!(refused.to_string(), NOT_READ_ONLY);
    assert_eq!(
        refused.to_string(),
        "method not marked read-only for agents"
    );
    assert_eq!(AgentRefusal::EXIT_CODE, 4);
}

#[test]
fn an_empty_allowlist_allows_nothing() {
    let policy = AgentRpcPolicy::from_settings(&RpcSettings::default()).unwrap();
    let read = target(HOST, vec![call("getblockcount")], false);
    assert!(matches!(
        policy.check(&read, &allowed()),
        Err(AgentRefusal::NotReadOnly { .. })
    ));
}

#[test]
fn every_call_of_a_batch_must_be_read_only() {
    let policy = AgentRpcPolicy::from_settings(&settings(&["eth_get*"])).unwrap();
    let batch = target(
        HOST,
        vec![call("eth_getBalance"), call("eth_sendTransaction")],
        true,
    );
    assert_eq!(
        policy.check(&batch, &allowed()),
        Err(AgentRefusal::NotReadOnly {
            method: "eth_sendTransaction".to_owned()
        })
    );
}

#[test]
fn a_host_outside_the_allowlist_is_refused() {
    // Scenario "Host outside allowlist".
    let policy = AgentRpcPolicy::from_settings(&settings(&["*"])).unwrap();
    for endpoint in ["https://rpc.example/", "wss://rpc.example/ws", "not a url"] {
        let elsewhere = target(endpoint, vec![call("eth_chainId")], false);
        let refused = policy.check(&elsewhere, &allowed()).unwrap_err();
        assert_eq!(refused, AgentRefusal::Policy(Blocked::HostNotAllowed));
        assert_eq!(refused.to_string(), "blocked: host not allowed");
    }
    let ws = target("ws://127.0.0.1:8545/", vec![call("eth_chainId")], false);
    assert_eq!(policy.check(&ws, &allowed()), Ok(()));
}

#[test]
fn subscriptions_are_refused() {
    let policy = AgentRpcPolicy::from_settings(&settings(&["*"])).unwrap();
    let mut live = target("ws://127.0.0.1:8545/", vec![call("eth_subscribe")], false);
    live.subscription = Some(SubscriptionSpec {
        subscribe: "eth_subscribe".into(),
        unsubscribe: "eth_unsubscribe".into(),
        notification: "eth_subscription".into(),
        notification_id_path: None,
        max_events: None,
        max_duration_ms: None,
    });
    assert_eq!(
        policy.check(&live, &allowed()),
        Err(AgentRefusal::Subscription)
    );
}

#[test]
fn presets_expand_and_an_unknown_one_is_a_configuration_error() {
    // Scenario "Unknown preset".
    register_preset("@agent-test-read", &["agent_test_get*"]);
    let policy = AgentRpcPolicy::from_settings(&settings(&["@agent-test-read", "x"])).unwrap();
    assert_eq!(policy.globs(), ["agent_test_get*", "x"]);
    assert!(policy.is_read_only("agent_test_getThing"));
    let err = AgentRpcPolicy::from_settings(&settings(&["@solana-read"])).unwrap_err();
    assert!(
        err.to_string()
            .starts_with("unknown method preset '@solana-read' (registered: "),
        "{err}"
    );
    assert!(err.to_string().contains("@agent-test-read"), "{err}");
    assert_eq!(UnknownPreset::EXIT_CODE, 2);
}

#[test]
fn a_guard_wins_over_the_allowlist() {
    // Scenario "Guard wins over the allowlist".
    let guard: Arc<dyn rustman_jsonrpc::MethodGuard> = Arc::new(NoRawTransactions);
    register_guard(guard.clone());
    let policy = AgentRpcPolicy::from_settings(&settings(&["eth_*"])).unwrap();
    let send = target(HOST, vec![call("eth_sendRawTransaction")], false);
    let refused = policy.check(&send, &allowed()).unwrap_err();
    assert!(matches!(refused, AgentRefusal::Guard(_)));
    assert_eq!(
        refused.to_string(),
        "eth_sendRawTransaction is refused: transactions need --allow-tx"
    );
    // Before the host check too: the guard's message, not the policy's.
    let elsewhere = target(
        "https://rpc.example/",
        vec![call("eth_sendRawTransaction")],
        false,
    );
    assert!(matches!(
        policy.check(&elsewhere, &[]),
        Err(AgentRefusal::Guard(_))
    ));
    rustman_jsonrpc::unregister_guard(&guard);
}

/// A workspace holding one `kind: jsonrpc` request to `endpoint`.
fn ws_workspace(endpoint: &str) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let write = |rel: &str, text: &str| {
        let path = dir.path().join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    };
    write("rustman.toml", "[workspace]\nname = \"t\"\nversion = 1\n");
    write(
        "chain/count.rman.yaml",
        &format!(
            "version: 1\nkind: jsonrpc\nname: Count\nendpoint: '{endpoint}'\nmethod: getblockcount\n"
        ),
    );
    dir
}

#[tokio::test(flavor = "multi_thread")]
async fn websocket_calls_are_admitted_by_the_session_governor() {
    // Regression: agent calls over ws:// skipped the GovernedSender, so
    // they were neither paced nor limited for concurrency.
    let server = JsonrpcWsServer::start(|message: &Value| {
        vec![WsFrame::Json(common::result(
            message["id"].clone(),
            json!(7),
        ))]
    })
    .await;
    let url = server.url("/");
    let http = url.replacen("ws://", "http://", 1);
    let safety = SafetySettings::default();
    let options = PolicyOptions {
        target: &http,
        permission: Permission::Mutations,
        allow_hosts: &[],
        production: None,
        environment: EnvironmentInfo::default(),
    };
    let policy = SafetyPolicy::new(&safety, options).unwrap();
    let sender = Arc::new(GovernedSender::new(
        policy,
        Governor::new(Limits::from_settings(&safety)),
    ));
    let read = AgentRpcPolicy::from_settings(&settings(&["getblockcount"])).unwrap();
    let agent = AgentRunner::new(read, sender.clone());
    let dir = ws_workspace(&url);
    let ws = Workspace::load(dir.path()).unwrap();
    let store = SecretStore::with_keychain(dir.path(), None);
    let ctx = RunContext {
        root: &ws.root,
        settings: &ws.settings,
        environment: None,
        secrets: &store,
        in_workspace: true,
    };
    let run_options = RunOptions {
        jsonrpc: Some(&agent),
        ..RunOptions::default()
    };
    let runner = Runner::new(ctx, run_options).unwrap();
    let records = runner.run(&plan(&ws, &[], &[])).await.requests;
    agent.close().await;
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].status, RequestStatus::Passed, "{:?}", records[0]);
    assert_eq!(server.sessions()[0].methods(), ["getblockcount"]);
    assert_eq!(sender.governor().admitted(), 1);
}
