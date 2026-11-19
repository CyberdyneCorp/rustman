//! `rustman rpc call`, `rustman rpc batch` and `rustman rpc discover`
//! (add-jsonrpc-client "rpc call command", "rpc batch command", "OpenRPC
//! method catalog"). The work is done by the `rustman-jsonrpc` crate
//! through the runner, so every call is resolved, checked, recorded in
//! history and redacted like any request; this module parses flags, builds
//! ad-hoc requests and prints results.

mod batch;
mod call;
mod discover;
mod print;

use std::time::Duration;

use clap::{Args, Subcommand};
use rustman_core::ConfigError;
use rustman_core::collection::RequestEntry;
use rustman_core::model::Request;
use rustman_core::run::{RequestRecord, RequestStatus, RpcRefusal, SkipReason};
use rustman_core::workspace::RequestRef;
use rustman_http::runner::{RunContext, Runner, Sent};
use rustman_jsonrpc::JsonrpcRunner;
use serde_json::{Map, Value, json};

use crate::commands::exec::{SendFlags, cancel_on_ctrl_c, history_store, runtime, secret_store};
use crate::commands::session::{Kind, Session};
use crate::commands::{EnvSelection, print_warnings};
use crate::exit::{CONFIG, CliError, INTERRUPTED, REQUEST, SAFETY};

#[derive(Debug, Args)]
#[command(after_help = "\
Examples:
  rustman rpc call http://127.0.0.1:8332 getblockcount --header \"Authorization: Basic {{btc_auth}}\"
  rustman rpc call {{rpc_url}} eth_getBalance '[\"0xabc...\", \"latest\"]'
  rustman rpc call wss://node.example eth_subscribe '[\"newHeads\"]' --subscribe eth_subscription --unsubscribe eth_unsubscribe
  rustman rpc call requests/chain/head.rman.yaml
  printf '{\"method\":\"eth_chainId\"}\\n' | rustman rpc batch {{rpc_url}} -
  rustman rpc discover {{rpc_url}}")]
pub struct RpcArgs {
    #[command(subcommand)]
    command: RpcCommand,
}

#[derive(Debug, Subcommand)]
enum RpcCommand {
    /// Send one JSON-RPC call (an endpoint with a method, or a request
    /// file) and print its result or error object
    Call(call::CallArgs),
    /// Send calls from a JSON array, a YAML list or JSON lines as one
    /// batch (100 calls per batch) and print the results in input order
    Batch(batch::BatchArgs),
    /// Fetch an endpoint's OpenRPC document with rpc.discover and cache it
    Discover(discover::DiscoverArgs),
}

pub fn run(args: &RpcArgs) -> Result<u8, CliError> {
    match &args.command {
        RpcCommand::Call(args) => call::run(args),
        RpcCommand::Batch(args) => batch::run(args),
        RpcCommand::Discover(args) => discover::run(args),
    }
}

/// Whether `target` is an endpoint (a URL, or a `{{variable}}` holding
/// one) rather than a request.
fn is_endpoint(target: &str) -> bool {
    ["http://", "https://", "ws://", "wss://", "{{"]
        .iter()
        .any(|start| target.starts_with(start))
}

/// Flags of every `rustman rpc` command.
#[derive(Debug, Args)]
struct Common {
    #[command(flatten)]
    selection: EnvSelection,
    #[command(flatten)]
    flags: SendFlags,
}

/// How calls are sent: catalog validation and subscription bounds.
#[derive(Clone, Copy, Debug, Default)]
struct Mode {
    /// `--no-validate`.
    skip_validation: bool,
    max_events: Option<u32>,
    duration: Option<Duration>,
}

/// `kind: jsonrpc` requests.
const JSONRPC: Kind = Kind {
    name: "jsonrpc",
    matches: |request| matches!(request, Request::Jsonrpc(_)),
};

impl Session {
    /// Send every request of `targets` in one run (shared captures and
    /// WebSocket connections), in order.
    fn send(
        &self,
        common: &Common,
        mode: Mode,
        ad_hoc: bool,
        targets: &[RequestRef<'_>],
    ) -> Result<Vec<Sent>, CliError> {
        let env = match &self.ws {
            Some(ws) => common.selection.environment(ws)?,
            None => None,
        };
        let store = secret_store(self.root())?;
        let jsonrpc = JsonrpcRunner {
            max_events: mode.max_events,
            max_duration: mode.duration,
            skip_validation: mode.skip_validation,
            ..JsonrpcRunner::default()
        };
        let mut options = common.flags.options()?;
        options.history = history_store(common.flags.no_history, self.ws.as_ref(), &self.settings);
        options.ad_hoc = ad_hoc;
        options.jsonrpc = Some(&jsonrpc);
        let cancel = options.cancel.clone();
        let ctx = RunContext {
            root: self.root(),
            settings: &self.settings,
            environment: env,
            secrets: &store,
            in_workspace: self.ws.is_some(),
        };
        let runner = Runner::new(ctx, options)?;
        let sent = runtime()?.block_on(async {
            cancel_on_ctrl_c(&cancel);
            let mut sent = Vec::with_capacity(targets.len());
            for target in targets {
                sent.push(runner.send(target, 1).await);
            }
            jsonrpc.close().await;
            sent
        });
        if let Err(e) = runner.finish() {
            print_warnings(&[e.to_string()]);
        }
        print_warnings(&store.take_warnings());
        for one in &sent {
            print_warnings(&one.record.warnings);
        }
        Ok(sent)
    }
}

/// The exit code of a request that produced nothing to print: interrupted
/// (130), refused by the catalog (2) or a guard (4), or errored (3). `None`
/// when there is a result to print.
fn early_exit(record: &RequestRecord) -> Option<u8> {
    if record.skip_reason == Some(SkipReason::Interrupted) {
        eprintln!("interrupted");
        return Some(INTERRUPTED);
    }
    if let Some(reason) = record.skip_reason {
        eprintln!("error: request skipped ({reason})");
        return Some(REQUEST);
    }
    let refusal = record.jsonrpc.as_ref().and_then(|j| j.refusal);
    let error = record.error.as_deref().unwrap_or_default();
    match refusal {
        Some(RpcRefusal::Validation) => return refuse(error, CONFIG),
        Some(RpcRefusal::Guard) => return refuse(error, SAFETY),
        None => {}
    }
    if record.status == RequestStatus::Errored {
        eprintln!("error: {error}");
        return Some(REQUEST);
    }
    None
}

fn refuse(message: &str, code: u8) -> Option<u8> {
    for violation in message.split("; ") {
        eprintln!("error: {violation}");
    }
    Some(code)
}

/// An ad-hoc `kind: jsonrpc` request from the fields of `doc`.
fn ad_hoc_entry(endpoint: &str, fields: Map<String, Value>) -> Result<RequestEntry, ConfigError> {
    rustman_jsonrpc::ad_hoc_entry(endpoint, fields).map_err(|e| ConfigError::Invalid(vec![e]))
}

/// `--header "Name: value"` flags as request headers.
fn headers(flags: &[String]) -> Result<Value, ConfigError> {
    let parsed = flags
        .iter()
        .map(|h| {
            let (name, value) = h.split_once(':').ok_or_else(|| {
                ConfigError::Usage(format!("invalid header '{h}' (expected 'Name: value')"))
            })?;
            Ok(json!({"name": name.trim(), "value": value.trim()}))
        })
        .collect::<Result<Vec<Value>, ConfigError>>()?;
    Ok(Value::Array(parsed))
}

/// `params` must be a JSON array or object.
fn params(text: &str) -> Result<Value, ConfigError> {
    let value: Value = serde_json::from_str(text)
        .map_err(|e| ConfigError::Usage(format!("params must be JSON: {e}")))?;
    if !(value.is_array() || value.is_object()) {
        let message = "params must be a JSON array or object".to_owned();
        return Err(ConfigError::Usage(message));
    }
    Ok(value)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn endpoints_and_requests_are_told_apart() {
        assert!(is_endpoint("http://127.0.0.1:8332"));
        assert!(is_endpoint("wss://node.example"));
        assert!(is_endpoint("{{rpc_url}}"));
        assert!(!is_endpoint("requests/head.rman.yaml"));
        assert!(!is_endpoint("Head"));
    }

    #[test]
    fn params_and_headers_are_checked() {
        assert_eq!(params("[1, \"a\"]").unwrap(), json!([1, "a"]));
        assert!(
            params("5")
                .unwrap_err()
                .to_string()
                .contains("array or object")
        );
        assert!(
            params("[")
                .unwrap_err()
                .to_string()
                .contains("must be JSON")
        );
        assert_eq!(
            headers(&["Authorization: Basic {{auth}}".into()]).unwrap(),
            json!([{"name": "Authorization", "value": "Basic {{auth}}"}])
        );
        assert!(headers(&["nocolon".into()]).is_err());
    }

    /// Refuses `cli_guarded_send`, as a capability's guard would.
    struct CliGuard;

    impl rustman_jsonrpc::MethodGuard for CliGuard {
        fn check(&self, method: &str, _params: Option<&Value>) -> Option<String> {
            (method == "cli_guarded_send").then(|| "cli_guarded_send is guarded".to_owned())
        }
    }

    #[derive(clap::Parser)]
    struct Cli {
        #[command(flatten)]
        rpc: RpcArgs,
    }

    #[test]
    fn a_guard_refuses_a_call_with_exit_4_and_sends_nothing() {
        // Scenario "Registered guard refuses" (task 7.3, CLI face).
        use clap::Parser;
        use rustman_test_support::{JsonrpcHttpServer, RpcReply};
        let guard: std::sync::Arc<dyn rustman_jsonrpc::MethodGuard> = std::sync::Arc::new(CliGuard);
        rustman_jsonrpc::register_guard(guard.clone());
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let server = runtime.block_on(JsonrpcHttpServer::start(|_, _| {
            RpcReply::json(&json!({"jsonrpc": "2.0", "id": 1, "result": "0x1"}))
        }));
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("rustman.toml"),
            "[workspace]\nname = \"t\"\nversion = 1\n",
        )
        .unwrap();
        let url = server.url("/");
        let workspace = dir.path().to_string_lossy().into_owned();
        let parse = |method: &str| {
            let args = ["rustman", "call", &url, method, "[\"0x00\"]"];
            let flags = ["--no-history", "--workspace", &workspace];
            Cli::try_parse_from(args.into_iter().chain(flags)).unwrap()
        };
        let code = run(&parse("cli_guarded_send").rpc);
        rustman_jsonrpc::unregister_guard(&guard);
        assert_eq!(code.unwrap(), SAFETY);
        assert!(server.hits().is_empty(), "nothing is sent");
    }

    #[test]
    fn ad_hoc_requests_load_like_files() {
        let mut fields = Map::new();
        fields.insert("method".into(), json!("getblockcount"));
        let entry = ad_hoc_entry("http://127.0.0.1:8332", fields).unwrap();
        let Request::Jsonrpc(request) = &entry.request else {
            panic!("not jsonrpc");
        };
        assert_eq!(request.method.as_deref(), Some("getblockcount"));
    }
}
