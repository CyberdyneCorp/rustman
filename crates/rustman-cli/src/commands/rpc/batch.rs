//! `rustman rpc batch <endpoint> <file|->`: read calls from a JSON array,
//! a YAML list or JSON lines (`{method, params}` each), send them as
//! batches of at most 100 calls and print one line per call, in input
//! order: the result, or `{"error": {...}}`. Exit codes: 0 when every
//! call returned a result, 1 when one returned an error, 2 for invalid
//! input or a catalog refusal, 3 when a batch got no usable answer.

use std::io::Read;
use std::path::{Path, PathBuf};

use clap::Args;
use rustman_core::ConfigError;
use rustman_core::model::MAX_BATCH_CALLS;
use rustman_core::run::RequestStatus;
use serde_json::{Map, Value, json};

use super::print::{answer, pretty, write_lines};
use super::{Common, Mode, Session, ad_hoc_entry, early_exit, headers};
use crate::exit::{ASSERTION, CliError, SUCCESS};

#[derive(Debug, Args)]
pub struct BatchArgs {
    /// The endpoint URL (or a {{variable}} holding one)
    #[arg(value_name = "ENDPOINT")]
    endpoint: String,
    /// The calls: a JSON array, a YAML list or JSON lines; - reads stdin
    #[arg(value_name = "FILE")]
    file: String,
    /// Add a header (repeatable): --header "Authorization: Bearer {{token}}"
    #[arg(long = "header", short = 'H', value_name = "NAME: VALUE")]
    headers: Vec<String>,
    /// Send even when the calls do not match the endpoint's OpenRPC catalog
    #[arg(long)]
    no_validate: bool,
    /// Print every result as one JSON array
    #[arg(long)]
    json: bool,
    #[command(flatten)]
    common: Common,
}

pub fn run(args: &BatchArgs) -> Result<u8, CliError> {
    let text = read_input(&args.file)?;
    let calls = parse_calls(&text)?;
    let session = Session::open(&args.common.selection, true)?;
    let entries = calls
        .chunks(MAX_BATCH_CALLS)
        .map(|chunk| ad_hoc_entry(&args.endpoint, batch_fields(chunk, &args.headers)?))
        .collect::<Result<Vec<_>, ConfigError>>()?;
    let mode = Mode {
        skip_validation: args.no_validate,
        ..Mode::default()
    };
    let targets: Vec<_> = entries.iter().map(|e| session.ad_hoc(e)).collect();
    let sent = session.send(&args.common, mode, true, &targets)?;
    let mut answers = Vec::with_capacity(calls.len());
    let mut failed = false;
    for one in &sent {
        if let Some(code) = early_exit(&one.record) {
            return Ok(code);
        }
        failed |= one.record.status != RequestStatus::Passed;
        let jsonrpc = one.record.jsonrpc.iter().flat_map(|j| &j.calls);
        answers.extend(jsonrpc.filter_map(answer));
    }
    if args.json {
        write_lines(&[pretty(&Value::Array(answers))])?;
    } else {
        let lines: Vec<String> = answers.iter().map(Value::to_string).collect();
        write_lines(&lines)?;
    }
    Ok(if failed { ASSERTION } else { SUCCESS })
}

fn read_input(file: &str) -> Result<String, ConfigError> {
    if file == "-" {
        let mut text = String::new();
        std::io::stdin()
            .read_to_string(&mut text)
            .map_err(|e| io_error(Path::new("<stdin>"), e))?;
        return Ok(text);
    }
    let path = PathBuf::from(file);
    std::fs::read_to_string(&path).map_err(|e| io_error(&path, e))
}

/// The calls of `text`: a JSON array, JSON lines or a YAML list.
fn parse_calls(text: &str) -> Result<Vec<Value>, ConfigError> {
    let items = if text.trim_start().starts_with('[') {
        serde_json::from_str::<Vec<Value>>(text)
            .map_err(|e| usage(format!("invalid JSON array: {e}")))?
    } else if let Some(lines) = json_lines(text) {
        lines
    } else {
        rustman_core::format::from_yaml::<Vec<Value>>(Path::new("<calls>"), text).map_err(|e| {
            usage(format!(
                "calls must be a JSON array, a YAML list or JSON lines: {e}"
            ))
        })?
    };
    if items.is_empty() {
        return Err(usage("no calls to send".to_owned()));
    }
    items
        .into_iter()
        .enumerate()
        .map(|(index, item)| call(index, item))
        .collect()
}

/// Every non-blank line parsed as a JSON object, or `None` when one is not.
fn json_lines(text: &str) -> Option<Vec<Value>> {
    text.lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| {
            serde_json::from_str::<Value>(line)
                .ok()
                .filter(Value::is_object)
        })
        .collect()
}

/// Call number `index` as a batch entry: `method`, and optionally
/// `params` (a list or a map) and `notification`.
fn call(index: usize, item: Value) -> Result<Value, ConfigError> {
    let at = index + 1;
    let Value::Object(mut fields) = item else {
        return Err(usage(format!(
            "call {at}: expected an object with 'method'"
        )));
    };
    if !fields.get("method").is_some_and(Value::is_string) {
        return Err(usage(format!("call {at}: 'method' must be a string")));
    }
    if fields
        .get("params")
        .is_some_and(|p| !(p.is_array() || p.is_object()))
    {
        return Err(usage(format!("call {at}: params must be a list or a map")));
    }
    fields.retain(|key, _| matches!(key.as_str(), "method" | "params" | "notification"));
    Ok(Value::Object(fields))
}

fn batch_fields(
    chunk: &[Value],
    header_flags: &[String],
) -> Result<Map<String, Value>, ConfigError> {
    let mut fields = Map::new();
    fields.insert("batch".into(), json!(chunk));
    if !header_flags.is_empty() {
        fields.insert("headers".into(), headers(header_flags)?);
    }
    Ok(fields)
}

fn io_error(path: &Path, source: std::io::Error) -> ConfigError {
    ConfigError::Io {
        path: path.to_path_buf(),
        source,
    }
}

fn usage(message: String) -> ConfigError {
    ConfigError::Usage(message)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn calls_come_from_json_arrays_lines_or_yaml() {
        let array = parse_calls(r#"[{"method":"a"},{"method":"b","params":[1]}]"#).unwrap();
        assert_eq!(array.len(), 2);
        let lines =
            parse_calls("{\"method\":\"a\"}\n\n{\"method\":\"b\",\"params\":{\"x\":1}}\r\n")
                .unwrap();
        assert_eq!(lines[1]["params"]["x"], 1);
        let yaml = parse_calls("- method: a\n- method: b\n  params: [0x10]\n").unwrap();
        assert_eq!(yaml.len(), 2);
    }

    #[test]
    fn invalid_calls_are_usage_errors() {
        let err = parse_calls(r#"[{"params":[]}]"#).unwrap_err();
        assert_eq!(err.to_string(), "call 1: 'method' must be a string");
        let err = parse_calls(r#"{"method":"a","params":5}"#).unwrap_err();
        assert_eq!(err.to_string(), "call 1: params must be a list or a map");
        assert!(parse_calls("[]").is_err());
    }
}
