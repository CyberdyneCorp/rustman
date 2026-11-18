//! The writer: IR requests and folders become request files and
//! `_collection.yaml` text through core's deterministic emitter. It refuses
//! any sensitive field that still holds a literal value (design D2).

use std::path::Path;

use rustman_core::Redactor;
use rustman_core::collection::CollectionMeta;
use rustman_core::format::Located;
use rustman_core::format::write::{
    MapBuilder, Node, auth_node, collection_to_yaml, json_node, name_values_node, render,
    request_to_yaml,
};
use rustman_core::model::{
    ApiKeyLocation, Assertion, Auth, Body, BodyAssertion, Capture, CaptureSource, Checked,
    HeaderAssertion, HttpRequest, JsonAssertion, JsonContent, LatencyAssertion, Method, NameValue,
    OAuth2, OAuth2Grant, Part, PartSource, REQUEST_VERSION, Request, StatusExpectation, VarValue,
    Variables,
};

use crate::error::ImportError;
use crate::ir::{
    ApiKeyIn, CaptureFrom, CaptureSpec, Check, Folder, Grant, ImportedAuth, ImportedBody,
    ImportedPart, ImportedRequest, JsonOp, Location, Note, NoteKind, Pair, PartValue, Variable,
};
use crate::scripts::comment_block;
use crate::secrets::{folder_has_literal, request_has_literal};

/// Where a file is written, to make file references relative.
pub struct Target<'a> {
    /// The workspace root.
    pub root: &'a Path,
    /// The file's path relative to the root, with `/` separators.
    pub path: &'a str,
    /// Redacts extracted secret values from preserved scripts.
    pub redactor: &'a Redactor,
}

impl Target<'_> {
    /// How many folders the file is below the root.
    fn depth(&self) -> usize {
        self.path.matches('/').count()
    }

    fn refuse(&self) -> ImportError {
        ImportError::LiteralSecret {
            path: self.path.to_owned(),
        }
    }
}

/// The text of the request file for `r`.
pub fn request_file(
    r: &ImportedRequest,
    target: &Target<'_>,
    notes: &mut Vec<Note>,
) -> Result<String, ImportError> {
    if request_has_literal(r) {
        return Err(target.refuse());
    }
    let comments = comment_block(&r.scripts, target.redactor);
    let yaml = match &r.body {
        Some(ImportedBody::Graphql { query, variables }) => {
            graphql_file(r, query, variables.as_deref(), &r.location, notes)
        }
        _ => request_to_yaml(&Request::Http(Box::new(http_request(r, target, notes)))),
    };
    Ok(format!("{comments}{yaml}"))
}

/// The text of `_collection.yaml` for `folder`, ordered by `order`.
pub fn collection_file(
    folder: &Folder,
    name: &str,
    order: Vec<String>,
    target: &Target<'_>,
) -> Result<String, ImportError> {
    if folder_has_literal(folder) {
        return Err(target.refuse());
    }
    let meta = CollectionMeta {
        name: Some(name.to_owned()),
        description: folder.description.clone(),
        order: (!order.is_empty()).then_some(order),
        headers: name_values(&folder.headers),
        auth: folder.auth.as_ref().map(auth),
        variables: variables(&folder.variables),
        parallel: None,
        data: None,
        secret_columns: None,
        source: None,
    };
    let comments = comment_block(&folder.scripts, target.redactor);
    Ok(format!("{comments}{}", collection_to_yaml(&meta)))
}

fn http_request(r: &ImportedRequest, target: &Target<'_>, notes: &mut Vec<Note>) -> HttpRequest {
    HttpRequest {
        version: REQUEST_VERSION,
        kind: "http".to_owned(),
        name: r.name.clone(),
        description: r.description.clone(),
        tags: None,
        method: Method::parse(&r.method).ok(),
        url: r.url.clone(),
        query: name_values(&r.query),
        headers: name_values(&r.headers),
        body: r.body.as_ref().map(|b| body(b, target, &r.location, notes)),
        auth: r.auth.as_ref().map(auth),
        settings: None,
        variables: variables(&r.variables),
        assertions: (!r.checks.is_empty()).then(|| r.checks.iter().filter_map(assertion).collect()),
        captures: (!r.captures.is_empty()).then(|| r.captures.iter().filter_map(capture).collect()),
    }
}

fn name_values(pairs: &[Pair]) -> Option<Vec<NameValue>> {
    (!pairs.is_empty()).then(|| {
        pairs
            .iter()
            .map(|p| NameValue::new(&p.name, &p.value))
            .collect()
    })
}

fn variables(vars: &[Variable]) -> Option<Variables> {
    let entries: Vec<(String, VarValue)> = vars
        .iter()
        .map(|v| (v.name.clone(), VarValue::String(v.value.clone())))
        .collect();
    (!entries.is_empty()).then(|| Variables::new(entries))
}

fn body(b: &ImportedBody, target: &Target<'_>, at: &Location, notes: &mut Vec<Note>) -> Body {
    match b {
        ImportedBody::Json { text } => Body::Json(JsonContent::Text(text.clone())),
        ImportedBody::Text { text } => Body::Text(text.clone()),
        ImportedBody::Form { fields } => Body::Form(name_values(fields).unwrap_or_default()),
        ImportedBody::Multipart { parts } => {
            Body::Multipart(parts.iter().map(|p| part(p, target, at, notes)).collect())
        }
        ImportedBody::File { path } => Body::File(Located::new(file_path(path, target, at, notes))),
        // Written as `kind: graphql` instead; never reached.
        ImportedBody::Graphql { query, .. } => Body::Text(query.clone()),
    }
}

fn part(p: &ImportedPart, target: &Target<'_>, at: &Location, notes: &mut Vec<Note>) -> Part {
    let source = match &p.value {
        PartValue::Text(text) => PartSource::Text(text.clone()),
        PartValue::File(path) => PartSource::File(Located::new(file_path(path, target, at, notes))),
    };
    Part {
        name: p.name.clone(),
        source,
        content_type: p.content_type.clone(),
    }
}

/// A file reference relative to the request file when it is inside the
/// workspace; otherwise only its file name, with a note.
fn file_path(src: &str, target: &Target<'_>, at: &Location, notes: &mut Vec<Note>) -> String {
    if src.contains("{{") {
        return src.to_owned();
    }
    let path = Path::new(src);
    if path.is_absolute() {
        if let Ok(inside) = path.strip_prefix(target.root) {
            let parts: Vec<String> = inside
                .components()
                .map(|c| c.as_os_str().to_string_lossy().into_owned())
                .collect();
            return format!("{}{}", "../".repeat(target.depth()), parts.join("/"));
        }
    } else if !escapes(src, target.depth()) {
        return src.to_owned();
    }
    let name = src.rsplit(['/', '\\']).next().unwrap_or(src).to_owned();
    let message = format!("file {src} is outside the workspace; only its name {name} is kept");
    notes.push(Note::new(NoteKind::Review, message, at));
    name
}

/// Whether the relative path `src`, from a file `depth` folders below the
/// root, climbs out of the workspace or is absolute on any OS: collections
/// come from other machines, so `/Users/x` on Windows and `C:\x` on Unix
/// are absolute too.
fn escapes(src: &str, depth: usize) -> bool {
    if src.starts_with(['/', '\\']) || src.get(1..2) == Some(":") {
        return true;
    }
    let mut level = depth as isize;
    for part in src.split(['/', '\\']) {
        match part {
            ".." => level -= 1,
            "" | "." => {}
            _ => level += 1,
        }
        if level < 0 {
            return true;
        }
    }
    false
}

fn auth(a: &ImportedAuth) -> Auth {
    match a {
        ImportedAuth::None | ImportedAuth::Unsupported(_) => Auth::None,
        ImportedAuth::Basic { username, password } => Auth::Basic {
            username: username.clone(),
            password: password.clone(),
        },
        ImportedAuth::Bearer { token } => Auth::Bearer {
            token: token.clone(),
        },
        ImportedAuth::ApiKey {
            name,
            value,
            location,
        } => Auth::ApiKey {
            name: name.clone(),
            value: value.clone(),
            location: Some(match location {
                ApiKeyIn::Header => ApiKeyLocation::Header,
                ApiKeyIn::Query => ApiKeyLocation::Query,
            }),
        },
        ImportedAuth::Oauth2(o) => Auth::OAuth2(OAuth2 {
            grant: match o.grant {
                Grant::ClientCredentials => OAuth2Grant::ClientCredentials,
                Grant::AuthorizationCode => OAuth2Grant::AuthorizationCode,
            },
            token_url: o.token_url.clone(),
            auth_url: o.auth_url.clone(),
            client_id: o.client_id.clone(),
            client_secret: o.client_secret.clone(),
            scopes: (!o.scopes.is_empty()).then(|| o.scopes.clone()),
        }),
    }
}

fn assertion(check: &Check) -> Option<Assertion> {
    match check {
        Check::Status(code) => Some(Assertion::Status(StatusExpectation::Code(*code))),
        Check::StatusClass(digit) => Some(Assertion::Status(StatusExpectation::Class(*digit))),
        Check::HeaderExists(name) => header_assertion(name, None),
        Check::HeaderEquals { name, value } => header_assertion(name, Some(value)),
        Check::BodyContains(text) => Checked::new(BodyAssertion {
            contains: Some(text.clone()),
            not_contains: None,
            matches: None,
            equals: None,
        })
        .ok()
        .map(Assertion::Body),
        Check::LatencyBelow(ms) => Checked::new(LatencyAssertion {
            total_lt: Some(*ms),
            ttfb_lt: None,
        })
        .ok()
        .map(Assertion::Latency),
        Check::Json { path, op, value } => json_assertion(path, *op, value),
    }
}

/// `header: {name, equals: value}`, or `exists: true` without a value.
fn header_assertion(name: &str, value: Option<&String>) -> Option<Assertion> {
    Checked::new(HeaderAssertion {
        name: name.to_owned(),
        equals: value.cloned(),
        contains: None,
        matches: None,
        exists: value.is_none().then_some(true),
    })
    .ok()
    .map(Assertion::Header)
}

/// A `json: {path, <op>: value}` assertion; `value` is JSON text, or a
/// plain string when it does not parse as JSON.
fn json_assertion(path: &str, op: JsonOp, value: &str) -> Option<Assertion> {
    let value =
        serde_json::from_str(value).unwrap_or_else(|_| serde_json::Value::String(value.to_owned()));
    let mut a = JsonAssertion {
        path: path.to_owned(),
        equals: None,
        not_equals: None,
        contains: None,
        matches: None,
        gt: None,
        gte: None,
        lt: None,
        lte: None,
        exists: None,
        length: None,
        value_type: None,
    };
    match op {
        JsonOp::Equals => a.equals = Some(value),
        JsonOp::NotEquals => a.not_equals = Some(value),
        JsonOp::Gt => a.gt = Some(value),
        JsonOp::Lt => a.lt = Some(value),
        JsonOp::Contains => a.contains = Some(value),
        JsonOp::Exists => a.exists = Some(true),
        JsonOp::Length => a.length = Some(value),
        JsonOp::Type => a.value_type = value.as_str().map(str::to_owned),
    }
    Checked::new(a).ok().map(|a| Assertion::Json(Box::new(a)))
}

fn capture(c: &CaptureSpec) -> Option<Checked<Capture>> {
    let (from, path, name) = match &c.from {
        CaptureFrom::Json(path) => (CaptureSource::Json, Some(path.clone()), None),
        CaptureFrom::Header(name) => (CaptureSource::Header, None, Some(name.clone())),
        CaptureFrom::Cookie(name) => (CaptureSource::Cookie, None, Some(name.clone())),
    };
    Checked::new(Capture {
        var: c.var.clone(),
        from,
        path,
        name,
        pattern: None,
        group: None,
        required: None,
        persist: c.persist.then_some(true),
    })
    .ok()
}

/// A `kind: graphql` request file: endpoint, document and variables.
/// Variables that are not a flat JSON object are kept as a comment, since
/// only scalar request variables load in this build.
fn graphql_file(
    r: &ImportedRequest,
    query: &str,
    vars: Option<&str>,
    at: &Location,
    notes: &mut Vec<Note>,
) -> String {
    let endpoint = match name_values(&r.query) {
        Some(params) => {
            let query: Vec<String> = params
                .iter()
                .map(|p| format!("{}={}", p.name, p.value))
                .collect();
            format!("{}?{}", r.url, query.join("&"))
        }
        None => r.url.clone(),
    };
    let (variables, comment) = graphql_variables(vars);
    if comment.is_some() {
        let message = "graphql variables are not a flat JSON object; kept as a comment";
        notes.push(Note::new(NoteKind::Review, message, at));
    }
    let node = MapBuilder::new()
        .node("version", Node::plain(REQUEST_VERSION))
        .str("kind", "graphql")
        .str("name", &r.name)
        .opt_str("description", r.description.as_deref())
        .str("endpoint", &endpoint)
        .str("document", query)
        .opt("variables", variables)
        .opt(
            "headers",
            name_values(&r.headers).map(|h| name_values_node(&h)),
        )
        .opt("auth", r.auth.as_ref().map(|a| auth_node(&auth(a))))
        .build();
    let comment = comment.map_or_else(String::new, |text| {
        text.lines()
            .map(|l| format!("# graphql-variables: {l}\n"))
            .collect()
    });
    format!("{comment}{}", render(&node))
}

/// The variables as a YAML node when they are a JSON object of scalars,
/// else their text for a comment.
fn graphql_variables(vars: Option<&str>) -> (Option<Node>, Option<String>) {
    let Some(text) = vars else {
        return (None, None);
    };
    match serde_json::from_str::<serde_json::Value>(text) {
        Ok(serde_json::Value::Object(map))
            if map
                .values()
                .all(|v| !v.is_object() && !v.is_array() && !v.is_null()) =>
        {
            (Some(json_node(&serde_json::Value::Object(map))), None)
        }
        _ => (None, Some(text.to_owned())),
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn absolute_paths_from_any_os_escape() {
        for src in [
            "/Users/someone/Desktop/invoice.pdf",
            "C:\\Users\\x\\a.pdf",
            "c:/x/a.pdf",
            "\\\\server\\share\\a.pdf",
            "\\x\\a.pdf",
        ] {
            assert!(super::escapes(src, 3), "{src}");
        }
        assert!(!super::escapes("files/a.pdf", 0));
        assert!(!super::escapes("../a.pdf", 1));
        assert!(super::escapes("../../a.pdf", 1));
    }

    use super::*;

    fn target<'a>(root: &'a Path, path: &'a str, redactor: &'a Redactor) -> Target<'a> {
        Target {
            root,
            path,
            redactor,
        }
    }

    #[test]
    fn literal_secrets_are_refused() {
        let redactor = Redactor::new();
        let root = Path::new("/ws");
        let r = ImportedRequest {
            name: "a".into(),
            method: "GET".into(),
            url: "https://x".into(),
            headers: vec![Pair::new("Authorization", "Bearer abc")],
            ..ImportedRequest::default()
        };
        let err = request_file(
            &r,
            &target(root, "c/a.rman.yaml", &redactor),
            &mut Vec::new(),
        )
        .unwrap_err();
        assert_eq!(
            err.to_string(),
            "import: refusing to write c/a.rman.yaml: a secret field has a literal value"
        );
        let folder = Folder {
            auth: Some(ImportedAuth::Bearer {
                token: "abc".into(),
            }),
            ..Folder::default()
        };
        assert!(
            collection_file(
                &folder,
                "c",
                vec![],
                &target(root, "c/_collection.yaml", &redactor)
            )
            .is_err()
        );
    }

    #[test]
    fn file_references_stay_inside_the_workspace() {
        let redactor = Redactor::new();
        let root = std::env::temp_dir().join("ws");
        let t = target(&root, "billing/users/a.rman.yaml", &redactor);
        let mut notes = Vec::new();
        let at = Location::default();
        let inside = root.join("files").join("a.png");
        assert_eq!(
            file_path(&inside.to_string_lossy(), &t, &at, &mut notes),
            "../../files/a.png"
        );
        assert_eq!(file_path("data/b.csv", &t, &at, &mut notes), "data/b.csv");
        assert!(notes.is_empty());
        assert_eq!(
            file_path("../../../../x/c.bin", &t, &at, &mut notes),
            "c.bin"
        );
        assert_eq!(
            file_path("C:\\Users\\me\\d.png", &t, &at, &mut notes),
            "d.png"
        );
        assert_eq!(notes.len(), 2);
        assert_eq!(
            file_path("{{dir}}/e.png", &t, &at, &mut notes),
            "{{dir}}/e.png"
        );
    }

    #[test]
    fn graphql_requests_become_graphql_files() {
        let r = ImportedRequest {
            name: "Me".into(),
            url: "{{base}}/graphql".into(),
            headers: vec![Pair::new("X-Team", "core")],
            body: Some(ImportedBody::Graphql {
                query: "query Me($id: ID) {\n  user(id: $id) { name }\n}".into(),
                variables: Some("{\"id\": \"{{user_id}}\", \"n\": 2}".into()),
            }),
            ..ImportedRequest::default()
        };
        let redactor = Redactor::new();
        let text = request_file(
            &r,
            &target(Path::new("/ws"), "a.rman.yaml", &redactor),
            &mut Vec::new(),
        )
        .unwrap();
        assert_eq!(
            text,
            "version: 1\nkind: graphql\nname: Me\nendpoint: '{{base}}/graphql'\n\
             document: |-\n  query Me($id: ID) {\n    user(id: $id) { name }\n  }\n\
             variables:\n  id: '{{user_id}}'\n  n: 2\n\
             headers:\n  - name: X-Team\n    value: core\n"
        );
        let request = rustman_core::model::KindRegistry::default()
            .load(Path::new("a.rman.yaml"), &text)
            .unwrap();
        assert_eq!(request.kind(), "graphql");
    }

    #[test]
    fn nested_graphql_variables_are_kept_as_a_comment() {
        let (node, comment) = graphql_variables(Some("{\"input\": {\"a\": 1}}"));
        assert!(node.is_none());
        assert_eq!(comment.as_deref(), Some("{\"input\": {\"a\": 1}}"));
    }
}
