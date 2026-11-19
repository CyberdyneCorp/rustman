//! `rustman rpc call <endpoint|request-file> [method] [params-json]`: send
//! one call (or a request file) and print its result or error object.
//! Exit codes: 0 for a result, 1 for a JSON-RPC error or a failed
//! assertion, 2 when the catalog refuses the call, 3 when nothing usable
//! came back, 130 on Ctrl+C.

use std::time::Duration;

use clap::Args;
use rustman_core::ConfigError;
use rustman_core::run::RequestStatus;
use serde_json::{Map, Value, json};

use super::print::{Format, print_call};
use super::{Common, Mode, Session, ad_hoc_entry, early_exit, headers, is_endpoint, params};
use crate::commands::exec::parse_duration;
use crate::exit::{ASSERTION, CliError, SUCCESS};

#[derive(Debug, Args)]
pub struct CallArgs {
    /// An endpoint URL (http, https, ws, wss, or a {{variable}}), or a
    /// jsonrpc request file, its id or its name
    #[arg(value_name = "ENDPOINT|REQUEST")]
    target: String,
    /// The method to call (with an endpoint)
    #[arg(value_name = "METHOD")]
    method: Option<String>,
    /// The params, as a JSON array or object
    #[arg(value_name = "PARAMS_JSON")]
    params: Option<String>,
    /// Add a header (repeatable): --header "Authorization: Basic {{auth}}"
    #[arg(long = "header", short = 'H', value_name = "NAME: VALUE")]
    headers: Vec<String>,
    /// Send this id instead of 1 (an integer, or else a string)
    #[arg(long, value_name = "ID", conflicts_with = "notify")]
    id: Option<String>,
    /// Send a notification: no id, no response expected
    #[arg(long)]
    notify: bool,
    /// Subscribe: METHOD is the subscribe method and this the notification
    /// method (needs --unsubscribe and a ws:// or wss:// endpoint)
    #[arg(long, value_name = "NOTIFICATION", requires = "unsubscribe")]
    subscribe: Option<String>,
    /// The method that ends a subscription
    #[arg(long, value_name = "METHOD", requires = "subscribe")]
    unsubscribe: Option<String>,
    /// Subscriptions stop after N events (default: the request's
    /// max_events, else 10)
    #[arg(long, value_name = "N", value_parser = clap::value_parser!(u32).range(1..))]
    max_events: Option<u32>,
    /// Subscriptions stop after this long (default: the request's
    /// max_duration_ms, else 30s)
    #[arg(long, value_name = "DURATION", value_parser = parse_duration)]
    duration: Option<Duration>,
    /// Send even when the call does not match the endpoint's OpenRPC catalog
    #[arg(long)]
    no_validate: bool,
    /// Print the response envelopes as received (redacted)
    #[arg(long, conflicts_with = "json")]
    raw: bool,
    /// Print the whole exchange as one JSON object
    #[arg(long)]
    json: bool,
    #[command(flatten)]
    common: Common,
}

pub fn run(args: &CallArgs) -> Result<u8, CliError> {
    let ad_hoc = is_endpoint(&args.target);
    let session = Session::open(&args.common.selection, ad_hoc)?;
    let mode = Mode {
        skip_validation: args.no_validate,
        max_events: args.max_events,
        duration: args.duration,
    };
    let sent = if ad_hoc {
        let entry = ad_hoc_entry(&args.target, fields(args)?)?;
        let target = session.ad_hoc(&entry);
        session.send(&args.common, mode, true, &[target])?
    } else {
        if args.method.is_some() || args.has_call_flags() {
            let message = "a request file already defines its call: drop METHOD, PARAMS_JSON and the call flags";
            return Err(ConfigError::Usage(message.to_owned()).into());
        }
        let target = session.find(&args.target)?;
        session.send(&args.common, mode, false, &[target])?
    };
    let record = &sent[0].record;
    if let Some(code) = early_exit(record) {
        return Ok(code);
    }
    print_call(record, format(args))?;
    for failed in record.assertions.iter().filter(|a| !a.passed()) {
        eprintln!("fail: {failed}");
    }
    if let Some(error) = &record.error {
        eprintln!("error: {error}");
    }
    Ok(match record.status {
        RequestStatus::Passed => SUCCESS,
        _ => ASSERTION,
    })
}

impl CallArgs {
    fn has_call_flags(&self) -> bool {
        self.params.is_some()
            || !self.headers.is_empty()
            || self.id.is_some()
            || self.notify
            || self.subscribe.is_some()
    }
}

fn format(args: &CallArgs) -> Format {
    if args.json {
        Format::Json
    } else if args.raw {
        Format::Raw
    } else {
        Format::Pretty
    }
}

/// The fields of the ad-hoc request `args` describe.
fn fields(args: &CallArgs) -> Result<Map<String, Value>, ConfigError> {
    let method = args
        .method
        .as_deref()
        .ok_or_else(|| ConfigError::Usage("METHOD is required with an endpoint".to_owned()))?;
    let mut fields = Map::new();
    fields.insert("method".into(), json!(method));
    if let Some(text) = &args.params {
        fields.insert("params".into(), params(text)?);
    }
    if let Some(id) = &args.id {
        let id = id.parse::<i64>().map_or_else(|_| json!(id), |n| json!(n));
        fields.insert("id".into(), id);
    }
    if args.notify {
        fields.insert("notification".into(), json!(true));
    }
    if !args.headers.is_empty() {
        fields.insert("headers".into(), headers(&args.headers)?);
    }
    if let (Some(notification), Some(unsubscribe)) = (&args.subscribe, &args.unsubscribe) {
        let subscription = json!({
            "subscribe": method,
            "unsubscribe": unsubscribe,
            "notification": notification,
        });
        fields.insert("subscription".into(), subscription);
    }
    Ok(fields)
}
