//! `rustman rpc discover <endpoint>`: call `rpc.discover`, cache the
//! OpenRPC document in `.rustman/cache/openrpc/` and print how many
//! methods it lists. Later calls to that endpoint are checked against it.
//! Exit codes: 0 when cached, 1 when the endpoint answered with an error
//! (or not with an OpenRPC document), 3 when nothing came back.

use clap::Args;
use rustman_core::run::RequestRecord;
use rustman_jsonrpc::catalog::write_cache;
use serde_json::{Map, Value, json};

use super::{Common, Mode, Session, ad_hoc_entry, early_exit, headers};
use crate::exit::{ASSERTION, CliError, SUCCESS};

/// What a node without `rpc.discover` answers.
const METHOD_NOT_FOUND: i64 = -32601;

#[derive(Debug, Args)]
pub struct DiscoverArgs {
    /// The endpoint URL (or a {{variable}} holding one)
    #[arg(value_name = "ENDPOINT")]
    endpoint: String,
    /// Add a header (repeatable): --header "Authorization: Bearer {{token}}"
    #[arg(long = "header", short = 'H', value_name = "NAME: VALUE")]
    headers: Vec<String>,
    #[command(flatten)]
    common: Common,
}

pub fn run(args: &DiscoverArgs) -> Result<u8, CliError> {
    let session = Session::open(&args.common.selection, false)?;
    let mut fields = Map::new();
    fields.insert("method".into(), json!("rpc.discover"));
    fields.insert("settings".into(), json!({"validate": false}));
    if !args.headers.is_empty() {
        fields.insert("headers".into(), headers(&args.headers)?);
    }
    let entry = ad_hoc_entry(&args.endpoint, fields)?;
    let target = session.ad_hoc(&entry);
    let sent = session.send(&args.common, Mode::default(), true, &[target])?;
    let sent = &sent[0];
    if let Some(code) = early_exit(&sent.record) {
        return Ok(code);
    }
    let document = match answer(&sent.record) {
        Ok(document) => document,
        Err(message) => {
            eprintln!("error: {message}");
            return Ok(ASSERTION);
        }
    };
    let resolved = sent.resolved().map(|r| r.url.text.as_str());
    let endpoint = resolved.unwrap_or(&args.endpoint);
    let (path, catalog) = match write_cache(session.root(), endpoint, document, &sent.redactor) {
        Ok(cached) => cached,
        Err(e) => {
            eprintln!("error: {e}");
            return Ok(ASSERTION);
        }
    };
    let shown = path
        .strip_prefix(session.root())
        .unwrap_or(&path)
        .to_string_lossy()
        .replace('\\', "/");
    let count = catalog.methods.len();
    let noun = if count == 1 { "method" } else { "methods" };
    println!("{count} {noun} cached in {shown}");
    Ok(SUCCESS)
}

/// The OpenRPC document the call returned, or why there is none.
fn answer(record: &RequestRecord) -> Result<Value, String> {
    let response = record
        .jsonrpc
        .as_ref()
        .and_then(|j| j.calls.first())
        .and_then(|c| c.response.as_ref())
        .ok_or_else(|| "no response to rpc.discover".to_owned())?;
    if let Some(error) = response.get("error") {
        if error.get("code").and_then(Value::as_i64) == Some(METHOD_NOT_FOUND) {
            return Err("endpoint does not support rpc.discover; provide an OpenRPC file".into());
        }
        return Err(record.error.clone().unwrap_or_else(|| error.to_string()));
    }
    response
        .get("result")
        .cloned()
        .ok_or_else(|| "no result from rpc.discover".to_owned())
}
