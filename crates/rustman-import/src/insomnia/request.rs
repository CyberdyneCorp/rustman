//! One Insomnia request (v4 resource or v5 node): method, URL, parameters,
//! headers, body, auth and scripts.

use serde_json::Value;

use super::Context;
use crate::common::{self, header, split_url, text_body};
use crate::ir::{
    ApiKeyIn, Grant, ImportedAuth, ImportedBody, ImportedOAuth2, ImportedPart, ImportedRequest,
    Location, Note, NoteKind, Pair, PartValue, Script,
};
use crate::postman::{description, string};

/// The request `v` at item path `path`.
pub(super) fn parse(v: &Value, path: &str, ctx: &mut Context) -> ImportedRequest {
    let at = ctx.location(path);
    let notes = &mut ctx.notes;
    let (url, mut query) = split_url(&string(&v["url"]).unwrap_or_default());
    query.extend(pairs(&v["parameters"], "query parameter", &at, notes));
    let headers = pairs(&v["headers"], "header", &at, notes);
    let body = body(&v["body"], &headers, &at, notes);
    ImportedRequest {
        name: String::new(),
        description: description(&v["description"]),
        method: common::method(v["method"].as_str(), &at, notes),
        url,
        query,
        headers,
        body,
        auth: auth(&v["authentication"], &at, notes),
        variables: Vec::new(),
        scripts: scripts(v, &at),
        checks: Vec::new(),
        captures: Vec::new(),
        statements: Vec::new(),
        location: at,
    }
}

/// `[{name, value, disabled}]`; disabled entries are dropped and noted.
pub(super) fn pairs(list: &Value, what: &str, at: &Location, notes: &mut Vec<Note>) -> Vec<Pair> {
    let mut out = Vec::new();
    for entry in list.as_array().into_iter().flatten() {
        let name = string(&entry["name"]).unwrap_or_default();
        if entry["disabled"] == true {
            let message = format!("disabled {what} {name} dropped");
            notes.push(Note::new(NoteKind::Dropped, message, at));
            continue;
        }
        if name.is_empty() {
            continue;
        }
        out.push(Pair::new(name, string(&entry["value"]).unwrap_or_default()));
    }
    out
}

fn body(b: &Value, headers: &[Pair], at: &Location, notes: &mut Vec<Note>) -> Option<ImportedBody> {
    let mime = string(&b["mimeType"])
        .or_else(|| header(headers, "content-type").map(str::to_owned))
        .unwrap_or_default();
    let base = mime.split(';').next().unwrap_or_default().trim();
    match base {
        "application/graphql" => graphql(&string(&b["text"]).unwrap_or_default()),
        "application/x-www-form-urlencoded" => Some(ImportedBody::Form {
            fields: pairs(&b["params"], "form field", at, notes),
        }),
        "multipart/form-data" => Some(ImportedBody::Multipart {
            parts: parts(&b["params"], at, notes),
        }),
        _ => match string(&b["fileName"]).filter(|f| !f.is_empty()) {
            Some(path) => Some(ImportedBody::File { path }),
            None => text_body(&mime, &string(&b["text"]).unwrap_or_default()),
        },
    }
}

/// Insomnia keeps a GraphQL body as JSON text `{"query", "variables"}`.
fn graphql(text: &str) -> Option<ImportedBody> {
    let doc: Value = serde_json::from_str(text).unwrap_or_default();
    let query = string(&doc["query"]).unwrap_or_else(|| text.to_owned());
    let variables = match &doc["variables"] {
        Value::Object(map) if !map.is_empty() => Some(doc["variables"].to_string()),
        Value::String(s) if !s.trim().is_empty() => Some(s.clone()),
        _ => None,
    };
    Some(ImportedBody::Graphql { query, variables })
}

fn parts(list: &Value, at: &Location, notes: &mut Vec<Note>) -> Vec<ImportedPart> {
    let mut out = Vec::new();
    for entry in list.as_array().into_iter().flatten() {
        let name = string(&entry["name"]).unwrap_or_default();
        if entry["disabled"] == true {
            let message = format!("disabled form part {name} dropped");
            notes.push(Note::new(NoteKind::Dropped, message, at));
            continue;
        }
        let value = if entry["type"] == "file" {
            PartValue::File(string(&entry["fileName"]).unwrap_or_default())
        } else {
            PartValue::Text(string(&entry["value"]).unwrap_or_default())
        };
        out.push(ImportedPart {
            name,
            value,
            content_type: None,
        });
    }
    out
}

/// The `authentication` object; `None` when absent or empty (inherit),
/// [`ImportedAuth::None`] when unsupported (never inherit a parent credential).
pub(super) fn auth(a: &Value, at: &Location, notes: &mut Vec<Note>) -> Option<ImportedAuth> {
    let kind = a["type"].as_str()?;
    if a["disabled"] == true || kind == "none" {
        return Some(ImportedAuth::None);
    }
    let get = |key: &str| string(&a[key]).unwrap_or_default();
    let auth = match kind {
        "basic" => ImportedAuth::Basic {
            username: get("username"),
            password: get("password"),
        },
        "bearer" => ImportedAuth::Bearer {
            token: get("token"),
        },
        "apikey" => ImportedAuth::ApiKey {
            name: get("key"),
            value: get("value"),
            location: if get("addTo") == "queryParams" {
                ApiKeyIn::Query
            } else {
                ApiKeyIn::Header
            },
        },
        "oauth2" => return oauth2(a, at, notes),
        other => {
            let message = format!("{other} auth unsupported");
            notes.push(Note::new(NoteKind::Unsupported, message, at));
            return Some(ImportedAuth::None);
        }
    };
    Some(auth)
}

fn oauth2(a: &Value, at: &Location, notes: &mut Vec<Note>) -> Option<ImportedAuth> {
    let get = |key: &str| string(&a[key]).unwrap_or_default();
    let grant = match get("grantType").as_str() {
        "client_credentials" => Grant::ClientCredentials,
        "" | "authorization_code" => Grant::AuthorizationCode,
        other => {
            let message = format!("oauth2 auth with grant '{other}' unsupported");
            notes.push(Note::new(NoteKind::Unsupported, message, at));
            return Some(ImportedAuth::None);
        }
    };
    let optional = |key: &str| Some(get(key)).filter(|v| !v.is_empty());
    let Some(token_url) = optional("accessTokenUrl") else {
        let message = "oauth2 auth without an access token URL unsupported";
        notes.push(Note::new(NoteKind::Unsupported, message, at));
        return Some(ImportedAuth::None);
    };
    let auth_url = match grant {
        Grant::AuthorizationCode => optional("authorizationUrl"),
        Grant::ClientCredentials => None,
    };
    Some(ImportedAuth::Oauth2(ImportedOAuth2 {
        grant,
        token_url,
        auth_url,
        client_id: get("clientId"),
        client_secret: optional("clientSecret"),
        scopes: get("scope").split_whitespace().map(str::to_owned).collect(),
    }))
}

/// Pre-request and after-response scripts: v4 `preRequestScript` /
/// `afterResponseScript`, v5 `scripts.preRequest` / `scripts.afterResponse`.
pub(super) fn scripts(v: &Value, at: &Location) -> Vec<Script> {
    let sources = [
        (
            "prerequest",
            &v["preRequestScript"],
            &v["scripts"]["preRequest"],
        ),
        (
            "after-response",
            &v["afterResponseScript"],
            &v["scripts"]["afterResponse"],
        ),
    ];
    sources
        .into_iter()
        .filter_map(|(event, v4, v5)| {
            let source = string(v4).or_else(|| string(v5))?;
            (!source.trim().is_empty()).then(|| Script {
                event: event.to_owned(),
                source,
                location: at.clone(),
            })
        })
        .collect()
}
