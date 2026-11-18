//! IR to Postman Collection v2.1 JSON (task 5.1). Placeholders are written
//! as they are; Rustman's `{{$uuid}}` becomes Postman's `{{$guid}}`.

use serde::Serialize;
use serde_json::{Value, json};

use crate::ir::{
    ApiKeyIn, Folder, Grant, ImportedAuth, ImportedBody, ImportedCollection, ImportedPart,
    ImportedRequest, Item, Note, NoteKind, Pair, PartValue, Script, Variable, VisitStrings,
};

pub const SCHEMA: &str = "https://schema.getpostman.com/json/collection/v2.1.0/collection.json";

/// The Postman events scripts can be exported to.
const EVENTS: &[&str] = &["prerequest", "test"];

#[derive(Serialize)]
struct Collection {
    info: Info,
    item: Vec<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    auth: Option<Value>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    event: Vec<Value>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    variable: Vec<Value>,
}

#[derive(Serialize)]
struct Info {
    name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    description: Option<String>,
    schema: &'static str,
}

/// The collection as Postman v2.1 JSON, with notes for what it cannot hold.
pub fn write(mut collection: ImportedCollection) -> (String, Vec<Note>) {
    collection.root.visit_strings(&mut |s| {
        if s.contains("{{$uuid}}") {
            *s = s.replace("{{$uuid}}", "{{$guid}}");
        }
    });
    let mut w = Writer {
        notes: collection.notes,
    };
    let root = &collection.root;
    w.folder_headers(root);
    let doc = Collection {
        info: Info {
            name: collection.name.clone(),
            description: root.description.clone(),
            schema: SCHEMA,
        },
        item: w.items(&root.items),
        auth: root.auth.as_ref().map(auth),
        event: w.events(&root.scripts),
        variable: variables(&root.variables),
    };
    let mut text = serde_json::to_string_pretty(&doc).expect("the collection serializes");
    text.push('\n');
    (text, w.notes)
}

struct Writer {
    notes: Vec<Note>,
}

impl Writer {
    fn items(&mut self, items: &[Item]) -> Vec<Value> {
        items
            .iter()
            .map(|item| match item {
                Item::Folder(folder) => self.folder(folder),
                Item::Request(request) => self.request(request),
            })
            .collect()
    }

    fn folder(&mut self, f: &Folder) -> Value {
        self.folder_headers(f);
        let mut v = json!({ "name": f.name });
        set(
            &mut v,
            "description",
            f.description.as_ref().map(|d| json!(d)),
        );
        v["item"] = Value::Array(self.items(&f.items));
        set(&mut v, "auth", f.auth.as_ref().map(auth));
        set(&mut v, "event", non_empty(self.events(&f.scripts)));
        set(&mut v, "variable", non_empty(variables(&f.variables)));
        v
    }

    fn folder_headers(&mut self, f: &Folder) {
        if !f.headers.is_empty() {
            let message = "folder headers not exported (Postman folders have none)";
            self.notes
                .push(Note::new(NoteKind::Unsupported, message, &f.location));
        }
    }

    fn request(&mut self, r: &ImportedRequest) -> Value {
        let mut request = json!({
            "method": r.method,
            "header": pairs(&r.headers),
        });
        set(&mut request, "body", r.body.as_ref().map(body));
        request["url"] = url(r);
        set(&mut request, "auth", r.auth.as_ref().map(auth));
        set(
            &mut request,
            "description",
            r.description.as_ref().map(|d| json!(d)),
        );
        let mut item = json!({ "name": r.name });
        set(&mut item, "event", non_empty(self.events(&r.scripts)));
        item["request"] = request;
        item
    }

    /// Postman events for the `prerequest` and `test` scripts; other
    /// sources' scripts are not Postman JavaScript and are only noted.
    fn events(&mut self, scripts: &[Script]) -> Vec<Value> {
        let mut out = Vec::new();
        for script in scripts {
            if !EVENTS.contains(&script.event.as_str()) {
                let message = format!("{} script not exported", script.event);
                self.notes
                    .push(Note::new(NoteKind::Unsupported, message, &script.location));
                continue;
            }
            let exec: Vec<&str> = script.source.lines().collect();
            out.push(json!({
                "listen": script.event,
                "script": { "type": "text/javascript", "exec": exec },
            }));
        }
        out
    }
}

/// Set `key` when `value` is present.
fn set(v: &mut Value, key: &str, value: Option<Value>) {
    if let Some(value) = value {
        v[key] = value;
    }
}

fn non_empty(values: Vec<Value>) -> Option<Value> {
    (!values.is_empty()).then_some(Value::Array(values))
}

fn pairs(list: &[Pair]) -> Vec<Value> {
    list.iter()
        .map(|p| json!({ "key": p.name, "value": p.value }))
        .collect()
}

fn variables(vars: &[Variable]) -> Vec<Value> {
    vars.iter()
        .map(|v| json!({ "key": v.name, "value": v.value, "type": "string" }))
        .collect()
}

/// The URL object: `raw` with the query string, the query list, and the
/// request variables (`{{id}}` path segments become Postman's `:id`).
fn url(r: &ImportedRequest) -> Value {
    let mut base = r.url.clone();
    for var in &r.variables {
        base = path_variable(&base, &var.name);
    }
    let query: Vec<String> = r
        .query
        .iter()
        .map(|p| format!("{}={}", p.name, p.value))
        .collect();
    let raw = if query.is_empty() {
        base
    } else {
        format!("{base}?{}", query.join("&"))
    };
    let mut v = json!({ "raw": raw });
    set(&mut v, "query", non_empty(pairs(&r.query)));
    let vars: Vec<Value> = r
        .variables
        .iter()
        .map(|var| json!({ "key": var.name, "value": var.value }))
        .collect();
    set(&mut v, "variable", non_empty(vars));
    v
}

/// `/{{name}}` path segments become `/:name`.
fn path_variable(url: &str, name: &str) -> String {
    let placeholder = format!("{{{{{name}}}}}");
    let (head, rest) = url.split_once("://").map_or(("", url), |(h, r)| (h, r));
    let path: Vec<String> = rest
        .split('/')
        .enumerate()
        .map(|(i, part)| {
            if i > 0 && part == placeholder {
                format!(":{name}")
            } else {
                part.to_owned()
            }
        })
        .collect();
    let path = path.join("/");
    if head.is_empty() {
        path
    } else {
        format!("{head}://{path}")
    }
}

fn body(b: &ImportedBody) -> Value {
    match b {
        ImportedBody::Json { text } => json!({
            "mode": "raw",
            "raw": text,
            "options": { "raw": { "language": "json" } },
        }),
        ImportedBody::Text { text } => json!({
            "mode": "raw",
            "raw": text,
            "options": { "raw": { "language": "text" } },
        }),
        ImportedBody::Form { fields } => {
            let fields: Vec<Value> = fields
                .iter()
                .map(|p| json!({ "key": p.name, "value": p.value, "type": "text" }))
                .collect();
            json!({ "mode": "urlencoded", "urlencoded": fields })
        }
        ImportedBody::Multipart { parts } => {
            let parts: Vec<Value> = parts.iter().map(part).collect();
            json!({ "mode": "formdata", "formdata": parts })
        }
        ImportedBody::File { path } => json!({ "mode": "file", "file": { "src": path } }),
        ImportedBody::Graphql { query, variables } => json!({
            "mode": "graphql",
            "graphql": { "query": query, "variables": variables.clone().unwrap_or_default() },
        }),
    }
}

fn part(p: &ImportedPart) -> Value {
    let mut v = match &p.value {
        PartValue::Text(text) => json!({ "key": p.name, "value": text, "type": "text" }),
        PartValue::File(path) => json!({ "key": p.name, "src": path, "type": "file" }),
    };
    set(
        &mut v,
        "contentType",
        p.content_type.as_ref().map(|c| json!(c)),
    );
    v
}

/// A Postman auth object with `[{key, value, type}]` parameters.
fn auth(a: &ImportedAuth) -> Value {
    let (kind, params): (&str, Vec<(&str, String)>) = match a {
        ImportedAuth::None => return json!({ "type": "noauth" }),
        ImportedAuth::Basic { username, password } => (
            "basic",
            vec![
                ("username", username.clone()),
                ("password", password.clone()),
            ],
        ),
        ImportedAuth::Bearer { token } => ("bearer", vec![("token", token.clone())]),
        ImportedAuth::ApiKey {
            name,
            value,
            location,
        } => {
            let at = match location {
                ApiKeyIn::Header => "header",
                ApiKeyIn::Query => "query",
            };
            let params = vec![
                ("key", name.clone()),
                ("value", value.clone()),
                ("in", at.to_owned()),
            ];
            ("apikey", params)
        }
        ImportedAuth::Oauth2(o) => {
            let grant = match o.grant {
                Grant::ClientCredentials => "client_credentials",
                Grant::AuthorizationCode => "authorization_code",
            };
            let mut params = vec![
                ("grant_type", grant.to_owned()),
                ("accessTokenUrl", o.token_url.clone()),
                ("clientId", o.client_id.clone()),
            ];
            let optional = [("authUrl", &o.auth_url), ("clientSecret", &o.client_secret)];
            params.extend(
                optional
                    .iter()
                    .filter_map(|(k, v)| Some((*k, (*v).clone()?))),
            );
            if !o.scopes.is_empty() {
                params.push(("scope", o.scopes.join(" ")));
            }
            ("oauth2", params)
        }
    };
    let params: Vec<Value> = params
        .into_iter()
        .map(|(key, value)| json!({ "key": key, "value": value, "type": "string" }))
        .collect();
    let mut v = json!({ "type": kind });
    v[kind] = Value::Array(params);
    v
}
