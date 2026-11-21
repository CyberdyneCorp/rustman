//! `rustman evm call <request>` or `rustman evm call --to <address> --abi
//! <abi> --function <function> [ARG]...`: run a read-only `eth_call`
//! through the runner (variables, secrets, assertions, captures, chain
//! pinning) and print the decoded result, revert, gas estimate and trace.

use std::sync::Arc;

use clap::Args;
use rustman_core::ConfigError;
use rustman_core::run::{RequestRecord, RequestStatus, SkipReason};
use rustman_core::text::printable;
use rustman_evm::adhoc::{arg_value, call_entry};
use rustman_evm::exec::{EvmRunner, exit_code_of_kind};
use rustman_http::HttpEngine;
use rustman_http::runner::{RunContext, Runner};
use serde_json::{Map, Value, json};

use super::{EVM_CALL, Endpoint};
use crate::commands::exec::{cancel_on_ctrl_c, history_store, runtime, secret_store};
use crate::commands::print_warnings;
use crate::commands::session::Session;
use crate::exit::{ASSERTION, CliError, INTERRUPTED, REQUEST, SUCCESS};

#[derive(Debug, Args)]
pub struct CallArgs {
    /// An evm-call request (file, id or name); with --to, the function's
    /// arguments instead
    #[arg(value_name = "REQUEST|ARG")]
    positionals: Vec<String>,
    /// The contract: an address or ENS name (an ad-hoc call)
    #[arg(long, value_name = "ADDRESS")]
    to: Option<String>,
    /// The ABI of an ad-hoc call: erc20, erc721, erc1155 or a path from the
    /// workspace root
    #[arg(long, value_name = "ABI", requires = "to")]
    abi: Option<String>,
    /// The function of an ad-hoc call: a name or a full signature
    #[arg(long, value_name = "FUNCTION", requires = "to")]
    function: Option<String>,
    /// The block: latest, finalized, a number or a hash (ad-hoc calls)
    #[arg(long, value_name = "BLOCK", requires = "to")]
    block: Option<String>,
    /// The caller address (ad-hoc calls)
    #[arg(long, value_name = "ADDRESS", requires = "to")]
    from: Option<String>,
    /// The value sent with the call, such as "1 ether" (ad-hoc calls)
    #[arg(long, value_name = "AMOUNT", requires = "to")]
    value: Option<String>,
    /// Also report eth_estimateGas for the same call
    #[arg(long)]
    estimate: bool,
    /// Also print the call tree (debug_traceCall, then trace_call)
    #[arg(long)]
    trace: bool,
    /// Do not record the call in history
    #[arg(long)]
    no_history: bool,
    #[command(flatten)]
    endpoint: Endpoint,
}

pub fn run(args: &CallArgs) -> Result<u8, CliError> {
    let ad_hoc = args.to.is_some();
    let session = Session::open(&args.endpoint.selection, ad_hoc && args.endpoint.ad_hoc())?;
    let entry = match ad_hoc {
        true => Some(ad_hoc_entry(args)?),
        false => None,
    };
    let target = match &entry {
        Some(entry) => session.ad_hoc(entry),
        None => {
            let [request] = args.positionals.as_slice() else {
                let message = "give one evm-call request, or --to, --abi and --function";
                return Err(ConfigError::Usage(message.to_owned()).into());
            };
            session.find(request, &EVM_CALL)?
        }
    };
    let environment = match &session.ws {
        Some(ws) => args.endpoint.selection.environment(ws)?,
        None => None,
    };
    let store = secret_store(session.root())?;
    let mut options = args.endpoint.options()?;
    options.history = history_store(args.no_history, session.ws.as_ref(), &session.settings);
    options.ad_hoc = ad_hoc;
    let mut evm = EvmRunner::new(Arc::new(HttpEngine::new()));
    evm.estimate = args.estimate;
    evm.trace = args.trace;
    options.evm = Some(&evm);
    let cancel = options.cancel.clone();
    let ctx = RunContext {
        root: session.root(),
        settings: &session.settings,
        environment,
        secrets: &store,
        in_workspace: session.ws.is_some(),
    };
    let providers = crate::commands::exec::secret_providers(session.root(), &session.settings)?;
    let runner = Runner::new(ctx, options)?.with_secret_providers(providers);
    let sent = runtime()?.block_on(async {
        cancel_on_ctrl_c(&cancel);
        runner.send(&target, 1).await
    });
    if let Err(e) = runner.finish() {
        print_warnings(&[e.to_string()]);
    }
    print_warnings(&store.take_warnings());
    print_warnings(&sent.record.warnings);
    Ok(report(&sent.record, args.endpoint.json))
}

/// The request-file fields of an ad-hoc call.
fn ad_hoc_entry(args: &CallArgs) -> Result<rustman_core::collection::RequestEntry, CliError> {
    let (Some(to), Some(abi), Some(function)) = (&args.to, &args.abi, &args.function) else {
        let message = "an ad-hoc call needs --to, --abi and --function";
        return Err(ConfigError::Usage(message.to_owned()).into());
    };
    let mut fields = Map::new();
    fields.insert("to".into(), json!(to));
    fields.insert("abi".into(), json!(abi));
    fields.insert("function".into(), json!(function));
    let values: Vec<Value> = args.positionals.iter().map(|a| arg_value(a)).collect();
    fields.insert("args".into(), Value::Array(values));
    let optional = [
        ("block", &args.block),
        ("from", &args.from),
        ("value", &args.value),
    ];
    for (key, value) in optional {
        if let Some(value) = value {
            fields.insert(key.into(), json!(value));
        }
    }
    call_entry(args.endpoint.rpc(), fields).map_err(|e| ConfigError::Invalid(vec![e]).into())
}

/// Print the outcome of `record` and pick the exit code.
fn report(record: &RequestRecord, json: bool) -> u8 {
    if record.skip_reason == Some(SkipReason::Interrupted) {
        eprintln!("interrupted");
        return INTERRUPTED;
    }
    if let Some(reason) = record.skip_reason {
        eprintln!("error: request skipped ({reason})");
        return REQUEST;
    }
    let evm = record.evm.clone().unwrap_or(Value::Null);
    if record.status == RequestStatus::Errored {
        let error = record.error.as_deref().unwrap_or_default();
        eprintln!("error: {}", printable(error));
        let kind = evm["error"]["kind"].as_str().unwrap_or("rpc");
        return exit_code_of_kind(kind);
    }
    if json {
        print_json(record, evm);
    } else {
        print_text(record, &evm);
    }
    match record.status {
        RequestStatus::Passed => SUCCESS,
        _ => ASSERTION,
    }
}

fn print_text(record: &RequestRecord, evm: &Value) {
    if let Some(status) = evm["status"].as_str() {
        eprintln!("{}", printable(status));
    }
    if let Some(note) = evm["abi"].as_str() {
        eprintln!("{}", printable(note));
    }
    println!(
        "{}",
        printable(evm["rendered"].as_str().unwrap_or_default())
    );
    if let Some(tree) = evm["trace"]["tree"].as_str() {
        println!("{}", printable(tree));
    }
    if let Some(note) = evm["trace"]["note"].as_str() {
        println!("{}", printable(note));
    }
    for failed in record.assertions.iter().filter(|a| !a.passed()) {
        eprintln!("fail: {}", printable(&failed.to_string()));
    }
    if let Some(error) = &record.error {
        eprintln!("error: {}", printable(error));
    }
}

fn print_json(record: &RequestRecord, mut evm: Value) {
    if let Value::Object(map) = &mut evm {
        map.remove("rendered");
        map.insert(
            "passed".into(),
            json!(record.status == RequestStatus::Passed),
        );
        map.insert("assertions".into(), json!(record.assertions));
        map.insert("captures".into(), json!(record.captures));
    }
    println!("{}", serde_json::to_string_pretty(&evm).unwrap_or_default());
}
