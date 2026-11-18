//! IR to a `.http` file (task 5.2): one `###` block per request, with the
//! auth and headers it inherits written out, and the variables as `@name =
//! value` lines. Placeholders such as `{{token}}` are written as they are.

use serde_json::Value;

use crate::ir::{
    ApiKeyIn, Folder, ImportedAuth, ImportedBody, ImportedCollection, ImportedPart,
    ImportedRequest, Item, Location, Note, NoteKind, Pair, PartValue, Variable,
};

const BOUNDARY: &str = "rustman-boundary";

/// The collection as a `.http` file, with notes for what it cannot hold.
pub fn write(collection: ImportedCollection) -> (String, Vec<Note>) {
    let mut w = Writer {
        notes: collection.notes,
        variables: Vec::new(),
        blocks: Vec::new(),
    };
    w.folder(&collection.root, &Inherited::default());
    let mut out = String::new();
    for var in &w.variables {
        out.push_str(&format!("@{} = {}\n", var.name, var.value));
    }
    for block in &w.blocks {
        if !out.is_empty() {
            out.push('\n');
        }
        out.push_str(block);
    }
    (out, w.notes)
}

/// The auth and headers a folder passes down.
#[derive(Clone, Default)]
struct Inherited {
    auth: Option<ImportedAuth>,
    headers: Vec<Pair>,
}

struct Writer {
    notes: Vec<Note>,
    variables: Vec<Variable>,
    blocks: Vec<String>,
}

impl Writer {
    fn note(&mut self, message: impl Into<String>, at: &Location) {
        self.notes
            .push(Note::new(NoteKind::Unsupported, message, at));
    }

    fn folder(&mut self, f: &Folder, parent: &Inherited) {
        self.add_variables(&f.variables, &f.location);
        self.note_scripts(&f.scripts, &f.location);
        let inherited = Inherited {
            auth: f.auth.clone().or_else(|| parent.auth.clone()),
            headers: merge(&parent.headers, &f.headers),
        };
        for item in &f.items {
            match item {
                Item::Folder(child) => self.folder(child, &inherited),
                Item::Request(r) => {
                    let block = self.request(r, &inherited);
                    self.blocks.push(block);
                }
            }
        }
    }

    fn add_variables(&mut self, vars: &[Variable], at: &Location) {
        for var in vars {
            match self.variables.iter().find(|v| v.name == var.name) {
                None => self.variables.push(var.clone()),
                Some(first) if first.value != var.value => {
                    let message = format!(
                        "variable {} has another value here; .http files have one value per name, the first is kept",
                        var.name
                    );
                    self.note(message, at);
                }
                Some(_) => {}
            }
        }
    }

    fn note_scripts(&mut self, scripts: &[crate::ir::Script], at: &Location) {
        for script in scripts {
            self.note(format!("{} script not exported", script.event), at);
        }
    }

    fn request(&mut self, r: &ImportedRequest, parent: &Inherited) -> String {
        let at = &r.location;
        self.add_variables(&r.variables, at);
        self.note_scripts(&r.scripts, at);
        let mut headers = merge(&parent.headers, &r.headers);
        let mut query = r.query.clone();
        let auth = r.auth.as_ref().or(parent.auth.as_ref());
        self.auth(auth, &mut headers, &mut query, at);
        let body = r.body.as_ref().and_then(|b| self.body(b, &mut headers, at));
        let method = match &r.body {
            Some(ImportedBody::Graphql { .. }) => "POST",
            _ => r.method.as_str(),
        };
        let mut out = format!("### {}\n{method} {}\n", r.name, with_query(&r.url, &query));
        for h in &headers {
            out.push_str(&format!("{}: {}\n", h.name, h.value));
        }
        if let Some(body) = body {
            out.push('\n');
            out.push_str(&body);
            if !body.ends_with('\n') {
                out.push('\n');
            }
        }
        out
    }

    /// Write `auth` as a header or query parameter.
    fn auth(
        &mut self,
        auth: Option<&ImportedAuth>,
        headers: &mut Vec<Pair>,
        query: &mut Vec<Pair>,
        at: &Location,
    ) {
        let header = match auth {
            None | Some(ImportedAuth::None) => return,
            Some(ImportedAuth::Basic { username, password }) => {
                Pair::new("Authorization", format!("Basic {username} {password}"))
            }
            Some(ImportedAuth::Bearer { token }) => {
                Pair::new("Authorization", format!("Bearer {token}"))
            }
            Some(ImportedAuth::ApiKey {
                name,
                value,
                location: ApiKeyIn::Query,
            }) => return query.push(Pair::new(name, value)),
            Some(ImportedAuth::ApiKey { name, value, .. }) => Pair::new(name, value),
            Some(ImportedAuth::Oauth2(_)) => {
                return self.note("oauth2 auth not exported (no .http equivalent)", at);
            }
        };
        if !has(headers, &header.name) {
            headers.push(header);
        }
    }

    /// The body text, adding a `Content-Type` header when it has none.
    fn body(&mut self, b: &ImportedBody, headers: &mut Vec<Pair>, at: &Location) -> Option<String> {
        let (content_type, text) = match b {
            ImportedBody::Json { text } => ("application/json".to_owned(), text.clone()),
            ImportedBody::Text { text } => return Some(text.clone()),
            ImportedBody::Form { fields } => (
                "application/x-www-form-urlencoded".to_owned(),
                join_pairs(fields),
            ),
            ImportedBody::Multipart { parts } => (
                format!("multipart/form-data; boundary={BOUNDARY}"),
                multipart(parts),
            ),
            ImportedBody::File { path } => return Some(format!("< {path}")),
            ImportedBody::Graphql { query, variables } => (
                "application/json".to_owned(),
                self.graphql(query, variables.as_deref(), at),
            ),
        };
        if !has(headers, "content-type") {
            headers.push(Pair::new("Content-Type", content_type));
        }
        Some(text)
    }

    /// A GraphQL request as its JSON POST body.
    fn graphql(&mut self, query: &str, variables: Option<&str>, at: &Location) -> String {
        let mut doc = serde_json::json!({ "query": query });
        if let Some(text) = variables {
            match serde_json::from_str::<Value>(text) {
                Ok(vars) => doc["variables"] = vars,
                Err(_) => self.note("graphql variables are not JSON; not exported", at),
            }
        }
        serde_json::to_string_pretty(&doc).expect("JSON values serialize")
    }
}

/// `parent` headers overridden (by name, ignoring case) by `own`.
fn merge(parent: &[Pair], own: &[Pair]) -> Vec<Pair> {
    let mut out: Vec<Pair> = parent
        .iter()
        .filter(|p| !has(own, &p.name))
        .cloned()
        .collect();
    out.extend(own.iter().cloned());
    out
}

fn has(headers: &[Pair], name: &str) -> bool {
    headers.iter().any(|h| h.name.eq_ignore_ascii_case(name))
}

fn join_pairs(pairs: &[Pair]) -> String {
    pairs
        .iter()
        .map(|p| format!("{}={}", p.name, p.value))
        .collect::<Vec<_>>()
        .join("&")
}

fn with_query(url: &str, query: &[Pair]) -> String {
    if query.is_empty() {
        return url.to_owned();
    }
    let sep = if url.contains('?') { '&' } else { '?' };
    format!("{url}{sep}{}", join_pairs(query))
}

/// A `multipart/form-data` body; file parts are read with `< path`.
fn multipart(parts: &[ImportedPart]) -> String {
    let mut out = String::new();
    for p in parts {
        out.push_str(&format!("--{BOUNDARY}\n"));
        match &p.value {
            PartValue::Text(text) => {
                out.push_str(&format!(
                    "Content-Disposition: form-data; name=\"{}\"\n",
                    p.name
                ));
                push_type(&mut out, p);
                out.push_str(&format!("\n{text}\n"));
            }
            PartValue::File(path) => {
                let file = path.rsplit(['/', '\\']).next().unwrap_or(path);
                out.push_str(&format!(
                    "Content-Disposition: form-data; name=\"{}\"; filename=\"{file}\"\n",
                    p.name
                ));
                push_type(&mut out, p);
                out.push_str(&format!("\n< {path}\n"));
            }
        }
    }
    out.push_str(&format!("--{BOUNDARY}--\n"));
    out
}

fn push_type(out: &mut String, p: &ImportedPart) {
    if let Some(content_type) = &p.content_type {
        out.push_str(&format!("Content-Type: {content_type}\n"));
    }
}
