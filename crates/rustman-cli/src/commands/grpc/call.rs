//! `rustman grpc call <request | --target T --method M --data JSON>`: send
//! one call through the runner and print the response. Text output prints
//! each received message as indented JSON on stdout and the status on
//! stderr; `--json` prints each received message as one JSON line, then a
//! final status line. Exit codes follow `cli-runner`: 0 when the call
//! passed, 1 for another status or a failed assertion, 2 when it cannot be
//! sent as written (schema, method, field), 3 when no status came back or
//! the deadline expired, 130 on Ctrl+C.

use clap::{ArgGroup, Args};
use rustman_core::ConfigError;
use rustman_core::run::{RequestRecord, RequestStatus, SkipReason};
use rustman_grpc::{GrpcRunner, GrpcStatus};
use rustman_http::runner::{GRPC_CONFIG_ERROR, RunContext, Runner};
use serde_json::{Map, Value, json};

use super::Session;
use crate::commands::exec::{SendFlags, cancel_on_ctrl_c, history_store, runtime, secret_store};
use crate::commands::{EnvSelection, print_warnings};
use crate::exit::{ASSERTION, CONFIG, CliError, INTERRUPTED, REQUEST, SUCCESS};

#[derive(Debug, Args)]
#[command(group(ArgGroup::new("source").required(true).args(["request", "target"])))]
pub struct CallArgs {
    /// A grpc request file, its id or its name
    #[arg(value_name = "REQUEST")]
    request: Option<String>,
    /// The target of an ad-hoc call: host:port, grpc://host:port or
    /// grpcs://host:port
    #[arg(long, value_name = "TARGET", requires = "method")]
    target: Option<String>,
    /// The method of an ad-hoc call: package.Service/Method
    #[arg(long, value_name = "METHOD", requires = "target")]
    method: Option<String>,
    /// The request message as JSON (default: {}); a JSON array sends each
    /// entry in order (client and bidi streaming)
    #[arg(long, value_name = "JSON", requires = "target")]
    data: Option<String>,
    /// Add request metadata (repeatable): --metadata "authorization: Bearer
    /// {{token}}"; names ending in -bin take base64
    #[arg(
        long = "metadata",
        short = 'm',
        value_name = "NAME: VALUE",
        requires = "target"
    )]
    metadata: Vec<String>,
    /// The deadline of an ad-hoc call in milliseconds (default: settings
    /// timeout_ms, else 30000)
    #[arg(long, value_name = "MS", requires = "target")]
    deadline_ms: Option<u64>,
    /// Print each received message as one JSON line, then a final status
    /// line
    #[arg(long)]
    json: bool,
    #[command(flatten)]
    selection: EnvSelection,
    #[command(flatten)]
    flags: SendFlags,
}

pub fn run(args: &CallArgs) -> Result<u8, CliError> {
    let ad_hoc = args.target.is_some();
    let session = Session::open(&args.selection, ad_hoc)?;
    let entry = match &args.target {
        Some(target) => Some(
            rustman_grpc::ad_hoc_entry(target, fields(args)?)
                .map_err(|e| ConfigError::Invalid(vec![e]))?,
        ),
        None => None,
    };
    let request = match &entry {
        Some(entry) => session.ad_hoc(entry),
        None => session.find(args.request.as_deref().unwrap_or_default(), &super::GRPC)?,
    };
    let record = send(args, &session, &request)?;
    if let Some(code) = early_exit(&record) {
        return Ok(code);
    }
    print_exchange(&record, args.json);
    for failed in record.assertions.iter().filter(|a| !a.passed()) {
        eprintln!("fail: {failed}");
    }
    if let Some(error) = &record.error {
        eprintln!("error: {error}");
    }
    Ok(match record.status {
        RequestStatus::Passed => SUCCESS,
        RequestStatus::Errored => REQUEST,
        _ => ASSERTION,
    })
}

/// Send `request` in a run of its own, with history.
fn send(
    args: &CallArgs,
    session: &Session,
    request: &rustman_core::workspace::RequestRef<'_>,
) -> Result<RequestRecord, CliError> {
    let env = session.environment(&args.selection)?;
    let store = secret_store(session.root())?;
    let grpc = GrpcRunner::default();
    let mut options = args.flags.options()?;
    let ws = session.ws.as_ref();
    options.history = history_store(args.flags.no_history, ws, &session.settings);
    options.ad_hoc = args.target.is_some();
    options.grpc = Some(&grpc);
    let cancel = options.cancel.clone();
    let ctx = RunContext {
        root: session.root(),
        settings: &session.settings,
        environment: env,
        secrets: &store,
        in_workspace: ws.is_some(),
    };
    let runner = Runner::new(ctx, options)?;
    let sent = runtime()?.block_on(async {
        cancel_on_ctrl_c(&cancel);
        runner.send(request, 1).await
    });
    if let Err(e) = runner.finish() {
        print_warnings(&[e.to_string()]);
    }
    print_warnings(&store.take_warnings());
    print_warnings(&sent.record.warnings);
    Ok(sent.record)
}

/// The fields of the ad-hoc request `args` describe.
fn fields(args: &CallArgs) -> Result<Map<String, Value>, ConfigError> {
    let mut fields = Map::new();
    fields.insert("method".into(), json!(args.method));
    let data = args.data.as_deref().unwrap_or("{}");
    let message: Value = serde_json::from_str(data)
        .map_err(|e| ConfigError::Usage(format!("--data must be JSON: {e}")))?;
    let key = if message.is_array() {
        "messages"
    } else {
        "message"
    };
    fields.insert(key.into(), message);
    if !args.metadata.is_empty() {
        fields.insert("metadata".into(), metadata(&args.metadata)?);
    }
    if let Some(ms) = args.deadline_ms {
        fields.insert("deadline_ms".into(), json!(ms));
    }
    Ok(fields)
}

/// `--metadata "name: value"` flags as request metadata.
fn metadata(flags: &[String]) -> Result<Value, ConfigError> {
    let parsed = flags
        .iter()
        .map(|m| {
            let (name, value) = m.split_once(':').ok_or_else(|| {
                ConfigError::Usage(format!("invalid metadata '{m}' (expected 'name: value')"))
            })?;
            Ok(json!({"name": name.trim(), "value": value.trim()}))
        })
        .collect::<Result<Vec<Value>, ConfigError>>()?;
    Ok(Value::Array(parsed))
}

/// The exit code of a request that produced no status to print:
/// interrupted (130), skipped (3), or stopped before a status came back
/// (2 for a request that cannot be sent as written, else 3).
fn early_exit(record: &RequestRecord) -> Option<u8> {
    if record.skip_reason == Some(SkipReason::Interrupted) {
        eprintln!("interrupted");
        return Some(INTERRUPTED);
    }
    if let Some(reason) = record.skip_reason {
        eprintln!("error: request skipped ({reason})");
        return Some(REQUEST);
    }
    let grpc = record.grpc.as_ref();
    let failure = grpc.and_then(|g| g.get("error"));
    let has_status = grpc.is_some_and(|g| g.get("status").is_some());
    if failure.is_none() && has_status {
        return None;
    }
    eprintln!("error: {}", record.error.as_deref().unwrap_or_default());
    let config = failure.and_then(|f| f["kind"].as_str()) == Some(GRPC_CONFIG_ERROR);
    Some(if config { CONFIG } else { REQUEST })
}

/// Print the received messages and the status of a call that ended with
/// one.
fn print_exchange(record: &RequestRecord, json: bool) {
    let Some(grpc) = &record.grpc else {
        return;
    };
    let messages = grpc["messages"].as_array().map_or(&[][..], Vec::as_slice);
    if json {
        for message in messages {
            println!("{message}");
        }
        let fin = json!({
            "status": grpc["status"],
            "headers": grpc["headers"],
            "trailers": grpc["trailers"],
        });
        println!("{fin}");
        return;
    }
    for message in messages {
        println!(
            "{}",
            serde_json::to_string_pretty(message).unwrap_or_default()
        );
    }
    if let Ok(status) = serde_json::from_value::<GrpcStatus>(grpc["status"].clone()) {
        eprintln!("status: {}", status.render());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn metadata_flags_are_parsed() {
        assert_eq!(
            metadata(&["authorization: Bearer {{token}}".into()]).unwrap(),
            json!([{"name": "authorization", "value": "Bearer {{token}}"}])
        );
        assert!(metadata(&["nocolon".into()]).is_err());
    }
}
