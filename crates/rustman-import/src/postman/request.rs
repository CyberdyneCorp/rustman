//! One Postman item: method, URL, query, headers and body.

use rustman_core::model::Method;
use serde_json::Value;

use super::{Context, auth, description, scripts, string};
use crate::ir::{
    ImportedBody, ImportedPart, ImportedRequest, Location, NoteKind, Pair, PartValue, Variable,
};

/// The request of `item`, at item path `path`.
pub(super) fn parse(item: &Value, path: &str, ctx: &mut Context) -> ImportedRequest {
    let at = ctx.location(path);
    let req = &item["request"];
    let (url, query, variables) = match req {
        Value::String(raw) => (split_url(raw).0, query_from_raw(raw), Vec::new()),
        _ => url(&req["url"], &at, ctx),
    };
    let headers = headers(&req["header"], &at, ctx);
    let body = body(&req["body"], &headers, &at, ctx);
    ImportedRequest {
        name: String::new(),
        description: description(&req["description"]).or_else(|| description(&item["description"])),
        method: method(&req["method"], &at, ctx),
        url,
        query,
        headers,
        body,
        auth: auth::parse(&req["auth"], &at, ctx),
        variables,
        scripts: scripts(&item["event"], &at),
        checks: Vec::new(),
        captures: Vec::new(),
        statements: Vec::new(),
        location: at,
    }
}

/// The method, upper-cased; `GET` when absent or not an HTTP token.
fn method(v: &Value, at: &Location, ctx: &mut Context) -> String {
    let Some(method) = string(v) else {
        return "GET".to_owned();
    };
    let upper = method.to_ascii_uppercase();
    if Method::parse(&upper).is_ok() {
        return upper;
    }
    let message = format!("invalid method '{method}' unsupported; GET used");
    ctx.note(NoteKind::Unsupported, message, at);
    "GET".to_owned()
}

/// The URL without its query string, the enabled query parameters and the
/// path variables (`/users/:id` becomes `/users/{{id}}`).
fn url(v: &Value, at: &Location, ctx: &mut Context) -> (String, Vec<Pair>, Vec<Variable>) {
    let raw = match v {
        Value::String(raw) => raw.clone(),
        _ => string(&v["raw"]).unwrap_or_else(|| url_from_parts(v)),
    };
    let (mut base, _) = split_url(&raw);
    let query = match &v["query"] {
        Value::Array(params) => pairs(params, "query parameter", at, ctx),
        _ => query_from_raw(&raw),
    };
    let mut variables = Vec::new();
    for var in v["variable"].as_array().into_iter().flatten() {
        let Some(name) = string(&var["key"]).filter(|k| !k.is_empty()) else {
            continue;
        };
        base = replace_path_variable(&base, &name);
        variables.push(Variable {
            name,
            value: string(&var["value"]).unwrap_or_default(),
            secret: false,
        });
    }
    (base, query, variables)
}

/// A URL from `protocol`, `host` and `path` when the export has no `raw`.
fn url_from_parts(v: &Value) -> String {
    let join = |part: &Value, sep: &str| match part {
        Value::Array(items) => items
            .iter()
            .filter_map(string)
            .collect::<Vec<_>>()
            .join(sep),
        other => string(other).unwrap_or_default(),
    };
    let host = join(&v["host"], ".");
    let path = join(&v["path"], "/");
    let mut url = match string(&v["protocol"]) {
        Some(protocol) => format!("{protocol}://{host}"),
        None => host,
    };
    if let Some(port) = string(&v["port"]) {
        url = format!("{url}:{port}");
    }
    if path.is_empty() {
        url
    } else {
        format!("{url}/{path}")
    }
}

/// `/:name` path segments become `/{{name}}`.
fn replace_path_variable(url: &str, name: &str) -> String {
    let segment = format!(":{name}");
    url.split('/')
        .map(|part| {
            if part == segment {
                format!("{{{{{name}}}}}")
            } else {
                part.to_owned()
            }
        })
        .collect::<Vec<_>>()
        .join("/")
}

/// The URL before `?`, and the query string after it.
fn split_url(raw: &str) -> (String, &str) {
    match raw.split_once('?') {
        Some((base, query)) => (base.to_owned(), query.split('#').next().unwrap_or_default()),
        None => (raw.to_owned(), ""),
    }
}

fn query_from_raw(raw: &str) -> Vec<Pair> {
    split_url(raw)
        .1
        .split('&')
        .filter(|p| !p.is_empty())
        .map(|p| {
            let (name, value) = p.split_once('=').unwrap_or((p, ""));
            Pair::new(name, value)
        })
        .collect()
}

/// `{key, value, disabled}` entries; disabled ones are dropped and noted.
fn pairs(entries: &[Value], what: &str, at: &Location, ctx: &mut Context) -> Vec<Pair> {
    let mut out = Vec::new();
    for entry in entries {
        let name = string(&entry["key"]).unwrap_or_default();
        if entry["disabled"] == true {
            ctx.note(
                NoteKind::Dropped,
                format!("disabled {what} {name} dropped"),
                at,
            );
            continue;
        }
        if name.is_empty() && entry["value"].is_null() {
            continue;
        }
        out.push(Pair::new(name, string(&entry["value"]).unwrap_or_default()));
    }
    out
}

/// Headers: a list of entries, or (in old exports) `Name: value` lines.
fn headers(v: &Value, at: &Location, ctx: &mut Context) -> Vec<Pair> {
    match v {
        Value::Array(entries) => pairs(entries, "header", at, ctx),
        Value::String(text) => text
            .lines()
            .filter_map(|line| line.split_once(':'))
            .map(|(name, value)| Pair::new(name.trim(), value.trim()))
            .collect(),
        _ => Vec::new(),
    }
}

fn body(b: &Value, headers: &[Pair], at: &Location, ctx: &mut Context) -> Option<ImportedBody> {
    if b["disabled"] == true {
        return None;
    }
    let mode = b["mode"].as_str()?;
    match mode {
        "raw" => raw_body(b, headers),
        "urlencoded" => {
            let fields = pairs(b["urlencoded"].as_array()?, "form field", at, ctx);
            Some(ImportedBody::Form { fields })
        }
        "formdata" => Some(ImportedBody::Multipart {
            parts: parts(b["formdata"].as_array()?, at, ctx),
        }),
        "file" => file_body(b, at, ctx),
        "graphql" => Some(ImportedBody::Graphql {
            query: string(&b["graphql"]["query"]).unwrap_or_default(),
            variables: string(&b["graphql"]["variables"]).filter(|v| !v.trim().is_empty()),
        }),
        other => {
            ctx.note(
                NoteKind::Unsupported,
                format!("body mode '{other}' unsupported"),
                at,
            );
            None
        }
    }
}

/// `raw` is JSON when its language says so, or (without one) when the
/// request's `Content-Type` does.
fn raw_body(b: &Value, headers: &[Pair]) -> Option<ImportedBody> {
    let text = string(&b["raw"]).filter(|t| !t.is_empty())?;
    let language = b["options"]["raw"]["language"].as_str();
    let json_type = headers
        .iter()
        .any(|h| h.name.eq_ignore_ascii_case("content-type") && h.value.contains("json"));
    let is_json = language == Some("json") || (language.is_none() && json_type);
    Some(if is_json {
        ImportedBody::Json { text }
    } else {
        ImportedBody::Text { text }
    })
}

fn file_body(b: &Value, at: &Location, ctx: &mut Context) -> Option<ImportedBody> {
    match first_src(&b["file"]["src"]) {
        Some(path) => Some(ImportedBody::File { path }),
        None => {
            ctx.note(NoteKind::Dropped, "file body without a file dropped", at);
            None
        }
    }
}

fn parts(entries: &[Value], at: &Location, ctx: &mut Context) -> Vec<ImportedPart> {
    let mut out = Vec::new();
    for entry in entries {
        let name = string(&entry["key"]).unwrap_or_default();
        if entry["disabled"] == true {
            ctx.note(
                NoteKind::Dropped,
                format!("disabled form part {name} dropped"),
                at,
            );
            continue;
        }
        let value = if entry["type"] == "file" {
            if entry["src"].as_array().is_some_and(|s| s.len() > 1) {
                let message = format!("form part {name} has several files; only the first is kept");
                ctx.note(NoteKind::Unsupported, message, at);
            }
            match first_src(&entry["src"]) {
                Some(path) => PartValue::File(path),
                None => {
                    ctx.note(
                        NoteKind::Dropped,
                        format!("form part {name} has no file; dropped"),
                        at,
                    );
                    continue;
                }
            }
        } else {
            PartValue::Text(string(&entry["value"]).unwrap_or_default())
        };
        out.push(ImportedPart {
            name,
            value,
            content_type: string(&entry["contentType"]).filter(|c| !c.is_empty()),
        });
    }
    out
}

/// A file path given as a string or as a list of paths.
fn first_src(src: &Value) -> Option<String> {
    match src {
        Value::Array(items) => items.first().and_then(string),
        other => string(other),
    }
    .filter(|s| !s.is_empty())
}
