//! One call into a plugin's extension point, and its result as named
//! output lines (`header.X-Signature`, `value`, `body`, ...) that a case's
//! `expect` table is checked against.

use std::collections::BTreeMap;

use rustman_core::Redactor;

use crate::harness::cases::{
    Call, CheckCall, ConvertCall, ExecuteCall, GenerateCall, RenderCall, SignCall,
    TransformRequestCall, TransformResponseCall,
};
use crate::plugin::{AuthRequest, Exchange, Header, Message, Param, Plugin, Verdict};

/// An output line: its name and value.
pub type Line = (String, String);

/// Make `call`, handing the plugin its inputs redacted of the secrets it
/// may not read, as the runtime does. `Err` is the call's error message.
pub fn make(plugin: &Plugin, call: &Call) -> Result<Vec<Line>, String> {
    let redactor = plugin.input_redactor();
    match call {
        Call::Sign(call) => sign(plugin, call, &redactor),
        Call::Check(call) => check(plugin, call, &redactor),
        Call::Generate(call) => generate(plugin, call, &redactor),
        Call::TransformRequest(call) => transform_request(plugin, call, &redactor),
        Call::TransformResponse(call) => transform_response(plugin, call, &redactor),
        Call::Convert(call) => convert(plugin, call, &redactor),
        Call::Execute(call) => execute(plugin, call, &redactor),
        Call::Render(call) => render(plugin, call, &redactor),
    }
}

fn sign(plugin: &Plugin, call: &SignCall, redactor: &Redactor) -> Result<Vec<Line>, String> {
    let request = AuthRequest {
        scheme: call.scheme.clone(),
        method: call.method.clone(),
        url: text(redactor, &call.url),
        headers: headers(&call.headers, redactor),
        body: bytes(redactor, &call.body),
        params: call
            .params
            .iter()
            .map(|(name, value)| Param {
                name: name.clone(),
                value: text(redactor, value),
            })
            .collect(),
    };
    let output = plugin.sign(&request).map_err(|e| e.to_string())?;
    let mut lines = header_lines(&output.set_headers);
    for param in output.add_query {
        lines.push((format!("query.{}", param.name), param.value));
    }
    if let Some(url) = output.url {
        lines.push(("url".to_owned(), url));
    }
    Ok(lines)
}

fn check(plugin: &Plugin, call: &CheckCall, redactor: &Redactor) -> Result<Vec<Line>, String> {
    let exchange = Exchange {
        method: call.method.clone(),
        url: text(redactor, &call.url),
        request_headers: headers(&call.request_headers, redactor),
        request_body: bytes(redactor, &call.request_body),
        status: call.status,
        response_headers: headers(&call.headers, redactor),
        response_body: bytes(redactor, &call.body),
        duration_ms: call.duration_ms,
    };
    let args = text(redactor, &call.args);
    let verdict = plugin
        .check(&call.function, &exchange, &args)
        .map_err(|e| e.to_string())?;
    let line = |verdict: &str| ("verdict".to_owned(), verdict.to_owned());
    Ok(match verdict {
        Verdict::Pass => vec![line("pass")],
        Verdict::Fail(message) => vec![line("fail"), ("message".to_owned(), message)],
        Verdict::Error(message) => vec![line("error"), ("message".to_owned(), message)],
    })
}

fn generate(
    plugin: &Plugin,
    call: &GenerateCall,
    redactor: &Redactor,
) -> Result<Vec<Line>, String> {
    let args: Vec<String> = call.args.iter().map(|a| text(redactor, a)).collect();
    let value = plugin
        .generate(&call.name, &args)
        .map_err(|e| e.to_string())?;
    Ok(vec![("value".to_owned(), value)])
}

fn transform_request(
    plugin: &Plugin,
    call: &TransformRequestCall,
    redactor: &Redactor,
) -> Result<Vec<Line>, String> {
    let message = message(&call.headers, &call.body, redactor);
    let url = text(redactor, &call.url);
    let output = plugin
        .transform_request(&call.name, &url, &message)
        .map_err(|e| e.to_string())?;
    Ok(message_lines(output))
}

fn transform_response(
    plugin: &Plugin,
    call: &TransformResponseCall,
    redactor: &Redactor,
) -> Result<Vec<Line>, String> {
    let message = message(&call.headers, &call.body, redactor);
    let output = plugin
        .transform_response(&call.name, call.status, &message)
        .map_err(|e| e.to_string())?;
    Ok(message_lines(output))
}

fn convert(plugin: &Plugin, call: &ConvertCall, redactor: &Redactor) -> Result<Vec<Line>, String> {
    let source = bytes(redactor, &call.source);
    let output = plugin
        .convert(&call.format, &call.source_path, &source)
        .map_err(|e| e.to_string())?;
    let files = |kind: &str, files: Vec<crate::plugin::ImportedFile>| -> Vec<Line> {
        files
            .into_iter()
            .map(|f| (format!("{kind}.{}", f.path), f.content))
            .collect()
    };
    let mut lines = files("request", output.requests);
    lines.extend(files("environment", output.environments));
    Ok(lines)
}

fn execute(plugin: &Plugin, call: &ExecuteCall, redactor: &Redactor) -> Result<Vec<Line>, String> {
    let request = text(redactor, &call.request);
    let exchange = plugin
        .execute(&call.kind, &request)
        .map_err(|e| e.to_string())?;
    let mut lines = vec![("summary".to_owned(), exchange.summary)];
    if let Some(status) = exchange.status {
        lines.push(("status".to_owned(), status.to_string()));
    }
    lines.extend(header_lines(&exchange.headers));
    lines.push(("body".to_owned(), lossy(&exchange.body)));
    Ok(lines)
}

fn render(plugin: &Plugin, call: &RenderCall, redactor: &Redactor) -> Result<Vec<Line>, String> {
    let report = text(redactor, &call.report);
    let output = plugin
        .render(&call.format, &report)
        .map_err(|e| e.to_string())?;
    Ok(vec![("output".to_owned(), lossy(&output))])
}

fn text(redactor: &Redactor, value: &str) -> String {
    redactor.text(value).into_owned()
}

fn bytes(redactor: &Redactor, value: &str) -> Vec<u8> {
    redactor.text(value).into_owned().into_bytes()
}

fn headers(map: &BTreeMap<String, String>, redactor: &Redactor) -> Vec<Header> {
    map.iter()
        .map(|(name, value)| Header {
            name: name.clone(),
            value: text(redactor, value),
        })
        .collect()
}

fn message(headers_map: &BTreeMap<String, String>, body: &str, redactor: &Redactor) -> Message {
    Message {
        headers: headers(headers_map, redactor),
        body: bytes(redactor, body),
    }
}

fn header_lines(headers: &[Header]) -> Vec<Line> {
    headers
        .iter()
        .map(|h| (format!("header.{}", h.name), h.value.clone()))
        .collect()
}

fn message_lines(message: Message) -> Vec<Line> {
    let mut lines = header_lines(&message.headers);
    lines.push(("body".to_owned(), lossy(&message.body)));
    lines
}

fn lossy(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}
