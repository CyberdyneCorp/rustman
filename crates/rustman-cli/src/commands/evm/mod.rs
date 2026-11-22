//! `rustman evm call|decode|logs|tx|block|ens|fork|probe|send|signer`
//! (add-evm-toolkit "EVM CLI commands", "chain-exploration",
//! "evm-signing"). The work is done by the
//! `rustman-evm` crate; this module parses flags, resolves the endpoint
//! with the environment's variables and secrets, and prints results
//! (redacted) as text or `--json`.
//!
//! Exit codes follow the ROADMAP table: 0 success, 1 a failed assertion or
//! something not found, 2 a configuration error, 3 an RPC transport error,
//! 4 a refusal such as a chain mismatch, 130 on Ctrl+C. Only `send` reaches
//! a signer, through the signing gate of `rustman-evm-signing`.

mod block;
mod call;
mod decode;
mod ens;
mod fork;
mod logs;
mod probe;
pub(crate) mod send;
mod signer;
mod tx;

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use clap::{Args, Subcommand};
use rustman_core::Redactor;
use rustman_core::collection::RequestEntry;
use rustman_core::environment::Environment;
use rustman_core::model::Request;
use rustman_core::secrets::SecretStore;
use rustman_core::settings::Settings;
use rustman_core::text::printable;
use rustman_evm::EvmError;
use rustman_evm::exec::{Connected, EvmRunner};
use rustman_evm::rpc::JsonrpcTransport;
use rustman_http::HttpEngine;
use rustman_http::runner::{RunContext, RunOptions, Runner};
use serde_json::{Map, Value, json};

use crate::commands::exec::{cancel_on_ctrl_c, overrides, parse_duration, runtime, secret_store};
use crate::commands::session::{Kind, Session};
use crate::commands::{EnvSelection, print_warnings};
use crate::exit::{CliError, SUCCESS};

#[derive(Debug, Args)]
#[command(after_help = "\
Examples:
  rustman evm call requests/token/balance.rman.yaml --env mainnet
  rustman evm call --to 0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48 --abi erc20 --function balanceOf vitalik.eth --rpc {{rpc}}
  rustman evm decode --abi erc20 0xa9059cbb000...
  rustman evm logs --address 0xA0b8... --event \"Transfer(address,address,uint256)\" --max-events 5
  rustman evm tx 0x5c50... --env mainnet --json
  rustman evm block latest
  rustman evm ens vitalik.eth
  rustman evm fork --chain mainnet --block 20000000
  rustman evm probe https://rpc-a.example https://rpc-b.example
  rustman evm send requests/transfer.rman.yaml --env local --sign
  rustman evm signer import deployer")]
pub struct EvmArgs {
    #[command(subcommand)]
    command: EvmCommand,
}

#[derive(Debug, Subcommand)]
enum EvmCommand {
    /// Run a read-only contract call (an evm-call request file, or --to,
    /// --abi and --function) and print the decoded result or revert
    Call(Box<call::CallArgs>),
    /// Decode call data, revert data or a log with an ABI, offline
    Decode(decode::DecodeArgs),
    /// Stream new heads or logs, or query logs over a block range
    Logs(logs::LogsArgs),
    /// Show a transaction and its receipt, with input and logs decoded
    Tx(tx::TxArgs),
    /// Show a block by number, tag or hash
    Block(block::BlockArgs),
    /// Resolve an ENS name to an address, or an address to its verified
    /// primary name
    Ens(ens::EnsArgs),
    /// Fork a chain with a local Anvil node and a temporary fork-<chain>
    /// environment, until Ctrl+C
    Fork(fork::ForkArgs),
    /// Probe EVM RPC providers: method support, archive state, latency,
    /// subscriptions, rate limits and cross-provider consistency
    Probe(Box<probe::ProbeArgs>),
    /// Sign and send an evm-tx request through the signing gate
    Send(Box<send::SendArgs>),
    /// Manage signers: import a key into the OS keychain
    Signer(signer::SignerArgs),
}

pub fn run(args: &EvmArgs) -> Result<u8, CliError> {
    match &args.command {
        EvmCommand::Call(args) => call::run(args),
        EvmCommand::Decode(args) => decode::run(args),
        EvmCommand::Logs(args) => logs::run(args),
        EvmCommand::Tx(args) => tx::run(args),
        EvmCommand::Block(args) => block::run(args),
        EvmCommand::Ens(args) => ens::run(args),
        EvmCommand::Fork(args) => fork::run(args),
        EvmCommand::Probe(args) => probe::run(args),
        EvmCommand::Send(args) => send::run(args),
        EvmCommand::Signer(args) => signer::run(args),
    }
}

/// The endpoint flags of every command that calls a node.
#[derive(Clone, Debug, Args)]
struct Endpoint {
    #[command(flatten)]
    selection: EnvSelection,
    /// The node: a URL or a {{variable}} (default: the environment's {{rpc}})
    #[arg(long, value_name = "URL")]
    rpc: Option<String>,
    /// Set a variable, above every other source (repeatable): --var k=v
    #[arg(long = "var", value_name = "K=V")]
    vars: Vec<String>,
    /// Total timeout per node call: 500ms, 5s, 2m, or milliseconds
    #[arg(long, value_name = "DURATION", value_parser = parse_duration)]
    timeout: Option<Duration>,
    /// Do not verify TLS certificates
    #[arg(long, short = 'k')]
    insecure: bool,
    /// Print one JSON object instead of text
    #[arg(long)]
    json: bool,
}

/// The default endpoint: the environment's `rpc` variable.
const DEFAULT_RPC: &str = "{{rpc}}";

impl Endpoint {
    fn rpc(&self) -> &str {
        self.rpc.as_deref().unwrap_or(DEFAULT_RPC)
    }

    fn options(&self) -> Result<RunOptions<'static>, CliError> {
        Ok(RunOptions {
            overrides: overrides(&self.vars)?,
            timeout: self.timeout,
            insecure: self.insecure,
            ..RunOptions::default()
        })
    }

    /// Outside a workspace only a literal URL works.
    fn ad_hoc(&self) -> bool {
        !self.rpc().contains("{{")
    }
}

/// A node, identified, and what printing needs.
struct Node<'a> {
    rpc: JsonrpcTransport,
    /// `[evm] ens_rpc`, when set, or why it could not be resolved (reported
    /// only by the commands that read ENS).
    ens_rpc: Option<Result<JsonrpcTransport, String>>,
    connected: Connected,
    /// Every secret of the session: output is redacted with it.
    redactor: Redactor,
    root: &'a Path,
    json: bool,
    /// Cancelled on Ctrl+C.
    cancel: tokio_util::sync::CancellationToken,
}

impl Node<'_> {
    /// Print `value` as JSON (with the endpoint identity) or `text`,
    /// redacted.
    fn print(&self, text: &str, mut value: Value) {
        if self.json {
            if let Value::Object(map) = &mut value {
                map.insert("endpoint".into(), self.connected.identity.to_json());
            }
            let shown = serde_json::to_string_pretty(&value).unwrap_or_default();
            println!("{}", self.redactor.text(&shown));
        } else {
            println!("{}", shown(&self.redactor, text));
        }
    }

    /// Print a note on stderr, redacted.
    fn note(&self, note: &str) {
        eprintln!("{}", shown(&self.redactor, note));
    }
}

/// What a command needs to run against a node.
struct Prepared {
    session: Session,
    store: SecretStore,
    entry: RequestEntry,
    environment: Option<Environment>,
}

/// The environment, secrets and an ad-hoc request carrying the endpoint.
fn prepare(flags: &Endpoint) -> Result<Prepared, CliError> {
    let session = Session::open(&flags.selection, flags.ad_hoc())?;
    let environment = match &session.ws {
        Some(ws) => flags.selection.environment(ws)?.cloned(),
        None => None,
    };
    let store = secret_store(session.root())?;
    let mut fields = Map::new();
    fields.insert("method".into(), json!("eth_chainId"));
    let entry = rustman_jsonrpc::ad_hoc_entry(flags.rpc(), fields)
        .map_err(|e| rustman_core::ConfigError::Invalid(vec![e]))?;
    Ok(Prepared {
        session,
        store,
        entry,
        environment,
    })
}

/// Resolve the endpoint of `flags`, identify the node (checking the
/// environment's `chain_id`) and run `work` with it. Errors are printed;
/// the result is the exit code.
fn with_node(
    flags: &Endpoint,
    work: impl AsyncFnOnce(&Node<'_>) -> Result<u8, EvmError>,
) -> Result<u8, CliError> {
    let prepared = prepare(flags)?;
    let settings = &prepared.session.settings;
    let ctx = RunContext {
        root: prepared.session.root(),
        settings,
        environment: prepared.environment.as_ref(),
        secrets: &prepared.store,
        in_workspace: prepared.session.ws.is_some(),
    };
    let options = flags.options()?;
    let cancel = options.cancel.clone();
    let providers = crate::commands::exec::secret_providers(prepared.session.root(), settings)?;
    let runner = Runner::new(ctx, options)?.with_secret_providers(providers);
    let target = prepared.session.ad_hoc(&prepared.entry);
    let evm = EvmRunner::new(Arc::new(HttpEngine::new()));
    let code = runtime()?.block_on(async {
        cancel_on_ctrl_c(&cancel);
        let node = connect(&runner, &target, &evm, flags, &prepared).await;
        let node = node.map(|node| Node {
            cancel: cancel.clone(),
            ..node
        });
        let redactor = runner.known_secrets();
        let result = match node {
            Ok(node) => work(&node).await,
            Err(error) => Err(error),
        };
        finish(result, &redactor)
    });
    print_warnings(&prepared.store.take_warnings());
    Ok(code)
}

async fn connect<'a>(
    runner: &Runner<'_>,
    target: &rustman_core::workspace::RequestRef<'_>,
    evm: &'a EvmRunner,
    flags: &Endpoint,
    prepared: &'a Prepared,
) -> Result<Node<'a>, EvmError> {
    let settings = &prepared.session.settings;
    let (endpoint, options) = runner
        .resolve_evm_endpoint(target, flags.rpc())
        .await
        .map_err(EvmError::Config)?;
    let rpc = evm.transport(endpoint, &options);
    let environment = prepared.environment.as_ref();
    let connected = evm.connect(&rpc, &settings.evm, environment).await?;
    let ens_rpc = match connected.settings.ens_rpc.as_deref() {
        Some(url) => Some(
            runner
                .resolve_evm_endpoint(target, url)
                .await
                .map(|(endpoint, options)| evm.transport(endpoint, &options))
                .map_err(|e| format!("[evm] ens_rpc: {e}")),
        ),
        None => None,
    };
    Ok(Node {
        rpc,
        ens_rpc,
        connected,
        redactor: runner.known_secrets(),
        root: prepared.session.root(),
        json: flags.json,
        cancel: tokio_util::sync::CancellationToken::new(),
    })
}

/// `text` as printed: redacted, and with control characters escaped, as
/// node answers (client versions, decoded strings, revert reasons) may
/// hold terminal escape sequences.
fn shown(redactor: &Redactor, text: &str) -> String {
    printable(&redactor.text(text)).into_owned()
}

/// Print an error (redacted) and pick the exit code.
fn finish(result: Result<u8, EvmError>, redactor: &Redactor) -> u8 {
    match result {
        Ok(code) => code,
        Err(error) => {
            eprintln!("error: {}", shown(redactor, &error.to_string()));
            error.exit_code()
        }
    }
}

/// `evm-call` requests.
const EVM_CALL: Kind = Kind {
    name: "evm-call",
    matches: |request| matches!(request, Request::Evm(evm) if matches!(**evm, rustman_core::model::EvmRequest::Call(_))),
};

/// The workspace settings of a command that works offline.
fn offline_settings(selection: &EnvSelection) -> Result<(Settings, std::path::PathBuf), CliError> {
    let session = Session::open(selection, true)?;
    let root = session.root().to_path_buf();
    Ok((session.settings, root))
}

/// Exit with success after printing.
const DONE: Result<u8, EvmError> = Ok(SUCCESS);
