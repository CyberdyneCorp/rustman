//! `kind: jsonrpc` request files in a workspace (add-jsonrpc-client tasks
//! 1.1 and 1.2): registration, the file model, one fixture per load-time
//! error of the spec, and interpolation of params.

use std::fs;
use std::path::Path;

use rustman_core::model::{KindRegistry, Request};
use rustman_core::vars::{ResolvedJsonrpc, Resolver, SecretLookup, resolve_jsonrpc};
use rustman_core::{ConfigError, Workspace};
use serde_json::json;

struct Fixture {
    dir: tempfile::TempDir,
}

impl Fixture {
    fn new() -> Self {
        let fixture = Self {
            dir: tempfile::tempdir().unwrap(),
        };
        fixture.write(
            "rustman.toml",
            "[workspace]\nname = \"t\"\nversion = 1\n[variables]\nrpc_url = \"http://node.local\"\n",
        );
        fixture.write(
            "environments/prod.yaml",
            "variables:\n  wallet: Abc1\nsecrets: [api_key]\n",
        );
        fixture
    }

    fn root(&self) -> &Path {
        self.dir.path()
    }

    fn write(&self, rel: &str, text: &str) {
        let path = self.root().join(rel);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, text).unwrap();
    }

    fn load(&self) -> Result<Workspace, ConfigError> {
        Workspace::load(self.root())
    }

    fn resolve(&self, name: &str) -> ResolvedJsonrpc {
        let ws = self.load().unwrap();
        let request = &ws.find_by_name(name)[0];
        let env = ws.active_environment(Some("prod"), None).unwrap();
        let resolver = Resolver::for_request(&ws.settings, request, env, &Store);
        resolve_jsonrpc(request, &resolver).unwrap()
    }
}

struct Store;

impl SecretLookup for Store {
    fn secret(&self, env: &str, name: &str) -> Result<Option<String>, String> {
        Ok((env == "prod" && name == "api_key").then(|| "k3y-s3cr3t".to_owned()))
    }
}

/// The load error of a single file `text`, as the workspace reports it.
fn load_error(text: &str) -> String {
    let f = Fixture::new();
    f.write("rpc/r.rman.yaml", text);
    match f.load() {
        Err(ConfigError::Invalid(errors)) => errors[0].to_string().replace('\\', "/"),
        other => panic!("expected a load error, got {:?}", other.map(|_| ())),
    }
}

const HEAD: &str = "version: 1\nkind: jsonrpc\nname: r\n";

#[test]
fn jsonrpc_is_a_registered_kind() {
    assert!(KindRegistry::default().names().contains(&"jsonrpc"));
    let f = Fixture::new();
    f.write(
        "rpc/block.rman.yaml",
        &format!("{HEAD}endpoint: '{{{{rpc_url}}}}'\nmethod: eth_blockNumber\n"),
    );
    let ws = f.load().unwrap();
    let found = ws.find_by_name("r");
    assert_eq!(found[0].request().kind(), "jsonrpc");
    assert!(matches!(found[0].request(), Request::Jsonrpc(_)));
}

#[test]
fn every_field_loads() {
    let text = format!(
        "{HEAD}endpoint: wss://node.example/ws
method: eth_subscribe
params: [newHeads]
headers:
  - name: X-Client
    value: rustman
auth:
  type: bearer
  token: '{{{{api_key}}}}'
subscription:
  subscribe: eth_subscribe
  unsubscribe: eth_unsubscribe
  notification: eth_subscription
  notification_id_path: $.params.sub
  max_events: 3
  max_duration_ms: 5000
idempotent: true
settings:
  timeout_ms: 2000
  connect_timeout_ms: 500
  retry: {{attempts: 2, backoff_ms: 10}}
  validate: false
assertions:
  - json: {{path: $.events, length: 3}}
captures:
  - {{var: head, from: json, path: '$.events[0]'}}
"
    );
    let r = rustman_core::model::JsonrpcRequest::parse(Path::new("r.rman.yaml"), &text).unwrap();
    let s = r.subscription.as_ref().unwrap();
    assert_eq!(s.effective_notification_id_path(), "$.params.sub");
    assert_eq!(
        (s.effective_max_events(), s.effective_max_duration_ms()),
        (3, 5000)
    );
    assert!(r.is_idempotent());
    assert_eq!(r.retry().effective_attempts(), 2);
    assert_eq!(r.http_settings().unwrap().timeout_ms, Some(2000));
    assert_eq!(r.settings.as_ref().unwrap().validate, Some(false));
    assert_eq!(r.assertion_list().len(), 1);
    assert_eq!(r.source, text);
}

#[test]
fn missing_method() {
    let err = load_error(&format!("{HEAD}endpoint: http://n\n"));
    assert_eq!(err, "rpc/r.rman.yaml:1: jsonrpc request requires 'method'");
}

#[test]
fn scalar_params_are_rejected() {
    let err = load_error(&format!("{HEAD}endpoint: http://n\nmethod: m\nparams: 5\n"));
    assert_eq!(err, "rpc/r.rman.yaml:6: params must be a list or a map");
}

#[test]
fn batches_hold_at_most_100_calls() {
    let calls: String = (0..101).map(|i| format!("  - method: m{i}\n")).collect();
    let err = load_error(&format!("{HEAD}endpoint: http://n\nbatch:\n{calls}"));
    assert_eq!(err, "rpc/r.rman.yaml:6: batch exceeds 100 calls");
    let hundred: String = (0..100).map(|i| format!("  - method: m{i}\n")).collect();
    let f = Fixture::new();
    f.write(
        "rpc/r.rman.yaml",
        &format!("{HEAD}endpoint: http://n\nbatch:\n{hundred}"),
    );
    assert!(f.load().is_ok());
}

#[test]
fn a_batch_call_without_method_names_its_line() {
    let err = load_error(&format!(
        "{HEAD}endpoint: http://n\nbatch:\n  - method: a\n  - params: [1]\n"
    ));
    assert_eq!(err, "rpc/r.rman.yaml:7: jsonrpc request requires 'method'");
}

#[test]
fn status_assertions_are_rejected_over_websocket() {
    let err = load_error(&format!(
        "{HEAD}endpoint: wss://n\nmethod: m\nassertions:\n  - status: 200\n"
    ));
    assert_eq!(
        err,
        "rpc/r.rman.yaml:7: status assertions are not available over WebSocket"
    );
    let err = load_error(&format!(
        "{HEAD}endpoint: ws://n\nmethod: m\nassertions:\n  - header: {{name: x, exists: true}}\n"
    ));
    assert!(err.ends_with("header assertions are not available over WebSocket"));
}

#[test]
fn result_assertions_on_notifications_are_rejected() {
    let err = load_error(&format!(
        "{HEAD}endpoint: http://n\nmethod: m\nnotification: true\nassertions:\n  - json: {{path: $.result, exists: true}}\n"
    ));
    assert_eq!(
        err,
        "rpc/r.rman.yaml:8: notifications have no result to assert on"
    );
    let err = load_error(&format!(
        "{HEAD}endpoint: http://n\nbatch:\n  - method: a\n    notification: true\n    assertions:\n      - json: {{path: $.result.x, exists: true}}\n"
    ));
    assert!(err.ends_with("notifications have no result to assert on"));
}

#[test]
fn subscriptions_over_http_are_rejected() {
    let err = load_error(&format!(
        "{HEAD}endpoint: https://n\nmethod: eth_subscribe\nsubscription: {{subscribe: eth_subscribe, unsubscribe: eth_unsubscribe, notification: eth_subscription}}\n"
    ));
    assert_eq!(
        err,
        "rpc/r.rman.yaml:6: subscriptions require a ws:// or wss:// endpoint"
    );
}

#[test]
fn method_and_batch_exclude_each_other() {
    let err = load_error(&format!(
        "{HEAD}endpoint: http://n\nmethod: m\nbatch:\n  - method: a\n"
    ));
    assert!(err.contains("a batch file sets method"), "{err}");
    let err = load_error(&format!(
        "{HEAD}endpoint: ws://n\nmethod: m\nnotification: true\nsubscription: {{subscribe: a, unsubscribe: b, notification: c}}\n"
    ));
    assert!(
        err.contains("cannot be batched or sent as notifications"),
        "{err}"
    );
}

#[test]
fn params_are_interpolated_with_secrets_recorded() {
    let f = Fixture::new();
    f.write(
        "rpc/named.rman.yaml",
        "version: 1\nkind: jsonrpc\nname: named\nendpoint: '{{rpc_url}}'\nmethod: getBalance\nparams: {address: '{{wallet}}', commitment: finalized, key: '{{api_key}}', n: 7, hex: '0x10'}\nid: req-7\n",
    );
    let resolved = f.resolve("named");
    assert_eq!(resolved.http.url.text, "http://node.local");
    let call = &resolved.calls[0];
    assert_eq!(call.method, "getBalance");
    assert_eq!(call.id, Some(json!("req-7")));
    assert_eq!(
        call.params,
        Some(
            json!({"address": "Abc1", "commitment": "finalized", "key": "k3y-s3cr3t", "n": 7, "hex": "0x10"})
        )
    );
    assert_eq!(resolved.http.secrets.len(), 1);
    let debug = format!("{resolved:?}");
    assert!(!debug.contains("k3y-s3cr3t"), "{debug}");
    assert!(debug.contains("«redacted:api_key»"), "{debug}");
}

#[test]
fn a_batch_resolves_every_call() {
    let f = Fixture::new();
    f.write(
        "rpc/batch.rman.yaml",
        "version: 1\nkind: jsonrpc\nname: batch\nendpoint: http://n\nbatch:\n  - method: eth_chainId\n  - method: getblockhash\n    params: [800000]\n    id: 9\n  - method: log\n    notification: true\n",
    );
    let resolved = f.resolve("batch");
    assert!(resolved.batch);
    let methods: Vec<&str> = resolved.calls.iter().map(|c| c.method.as_str()).collect();
    assert_eq!(methods, ["eth_chainId", "getblockhash", "log"]);
    assert_eq!(resolved.calls[1].params, Some(json!([800000])));
    assert_eq!(resolved.calls[1].id, Some(json!(9)));
    assert!(resolved.calls[2].notification);
}
