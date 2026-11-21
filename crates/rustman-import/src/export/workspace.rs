//! Workspace to IR (design D6): a collection folder or one request file,
//! with the auth, headers and variables it inherits from the folders above
//! it, as an [`ImportedCollection`]. What the IR cannot carry (assertions,
//! captures, settings, tags, other request kinds) becomes a note.

use std::path::Path;

use rustman_core::Workspace;
use rustman_core::collection::{COLLECTION_FILE, Collection, Entry, RequestEntry};
use rustman_core::format::{JsonValue, from_yaml};
use rustman_core::model::{
    ApiKeyLocation, Auth, Body, HttpRequest, JsonContent, NameValue, OAuth2Grant, Part, PartSource,
    Request, Variables,
};
use serde::Deserialize;

use crate::error::ImportError;
use crate::ir::{
    ApiKeyIn, Folder, Grant, ImportedAuth, ImportedBody, ImportedCollection, ImportedOAuth2,
    ImportedPart, ImportedRequest, Item, Location, Note, NoteKind, Pair, PartValue, Variable,
};
use crate::scripts::parse_comment_block;

/// The collection folder or request file at `rel` (relative to the
/// workspace root, `""` for the root) as IR.
pub fn collect(ws: &Workspace, rel: &Path) -> Result<ImportedCollection, ImportError> {
    let mut reader = Reader {
        root: &ws.root,
        notes: Vec::new(),
    };
    if let Some(chain) = collection_chain(&ws.tree, rel) {
        let (target, ancestors) = chain.split_last().expect("the chain holds the target");
        let mut root = reader.folder(target)?;
        inherit(&mut root, ancestors);
        if root.name.is_empty() {
            root.name.clone_from(&ws.settings.workspace.name);
        }
        return Ok(reader.finish(root));
    }
    let (ancestors, entry) = request_chain(&ws.tree, rel).ok_or_else(|| {
        ImportError::Export(format!(
            "{} is not a request file or collection folder of the workspace",
            rel.display()
        ))
    })?;
    let mut root = Folder {
        name: entry.request.name().to_owned(),
        ..Folder::default()
    };
    if let Some(request) = reader.request(entry)? {
        root.items.push(Item::Request(Box::new(request)));
    }
    inherit(&mut root, &ancestors);
    Ok(reader.finish(root))
}

/// The collections from the root down to the one at `rel`.
fn collection_chain<'a>(tree: &'a Collection, rel: &Path) -> Option<Vec<&'a Collection>> {
    if tree.path == rel {
        return Some(vec![tree]);
    }
    tree.entries.iter().find_map(|entry| match entry {
        Entry::Collection(child) if rel.starts_with(&child.path) => {
            let mut chain = collection_chain(child, rel)?;
            chain.insert(0, tree);
            Some(chain)
        }
        _ => None,
    })
}

/// The collections above the request file at `rel`, and its entry.
fn request_chain<'a>(
    tree: &'a Collection,
    rel: &Path,
) -> Option<(Vec<&'a Collection>, &'a RequestEntry)> {
    tree.entries.iter().find_map(|entry| match entry {
        Entry::Request(r) if r.path == rel => Some((vec![tree], r)),
        Entry::Collection(child) if rel.starts_with(&child.path) => {
            let (mut chain, r) = request_chain(child, rel)?;
            chain.insert(0, tree);
            Some((chain, r))
        }
        _ => None,
    })
}

/// Give `root` the auth, headers and variables of `ancestors` (outermost
/// first) that it does not set itself.
fn inherit(root: &mut Folder, ancestors: &[&Collection]) {
    for ancestor in ancestors.iter().rev() {
        let meta = &ancestor.meta;
        if root.auth.is_none() {
            root.auth = meta.auth.as_ref().map(auth);
        }
        let headers = pairs(meta.headers.as_deref());
        let new_headers: Vec<Pair> = headers
            .into_iter()
            .filter(|h| {
                !root
                    .headers
                    .iter()
                    .any(|r| r.name.eq_ignore_ascii_case(&h.name))
            })
            .collect();
        root.headers.splice(0..0, new_headers);
        let vars: Vec<Variable> = variables(meta.variables.as_ref())
            .into_iter()
            .filter(|v| !root.variables.iter().any(|r| r.name == v.name))
            .collect();
        root.variables.splice(0..0, vars);
    }
}

struct Reader<'a> {
    root: &'a Path,
    notes: Vec<Note>,
}

impl Reader<'_> {
    fn finish(self, root: Folder) -> ImportedCollection {
        ImportedCollection {
            name: root.name.clone(),
            root,
            environments: Vec::new(),
            notes: self.notes,
        }
    }

    fn text(&self, rel: &Path) -> Result<String, ImportError> {
        let path = self.root.join(rel);
        std::fs::read_to_string(&path).map_err(|e| ImportError::io(&path, e))
    }

    fn folder(&mut self, c: &Collection) -> Result<Folder, ImportError> {
        let meta_path = c.path.join(COLLECTION_FILE);
        let scripts = if self.root.join(&meta_path).is_file() {
            parse_comment_block(&self.text(&meta_path)?, &location(&meta_path))
        } else {
            Vec::new()
        };
        let mut items = Vec::new();
        for entry in &c.entries {
            match entry {
                Entry::Collection(child) => {
                    items.push(Item::Folder(Box::new(self.folder(child)?)));
                }
                Entry::Request(r) => {
                    if let Some(request) = self.request(r)? {
                        items.push(Item::Request(Box::new(request)));
                    }
                }
            }
        }
        let meta = &c.meta;
        Ok(Folder {
            name: c.name(),
            description: meta.description.clone(),
            auth: meta.auth.as_ref().map(auth),
            headers: pairs(meta.headers.as_deref()),
            variables: variables(meta.variables.as_ref()),
            scripts,
            items,
            location: location(&c.path),
        })
    }

    /// The request of `entry`; `None` (with a note) for a kind the IR
    /// cannot carry.
    fn request(&mut self, entry: &RequestEntry) -> Result<Option<ImportedRequest>, ImportError> {
        let at = location(&entry.path);
        let text = self.text(&entry.path)?;
        let mut request = match &entry.request {
            Request::Http(r) => self.http(r, &at),
            Request::Graphql(_) => self.graphql(&text, &entry.path)?,
            Request::Jsonrpc(_)
            | Request::Mcp(_)
            | Request::Grpc(_)
            | Request::Ws(_)
            | Request::Sse(_)
            | Request::Messaging(_)
            | Request::Opaque(_) => {
                let kind = entry.request.kind();
                let message = format!("request kind '{kind}' not exported");
                self.notes
                    .push(Note::new(NoteKind::Unsupported, message, &at));
                return Ok(None);
            }
        };
        request.scripts = parse_comment_block(&text, &at);
        request.location = at;
        Ok(Some(request))
    }

    fn http(&mut self, r: &HttpRequest, at: &Location) -> ImportedRequest {
        self.note_lost(r, at);
        ImportedRequest {
            name: r.name.clone(),
            description: r.description.clone(),
            method: r.effective_method().to_owned(),
            url: r.url.clone(),
            query: pairs(r.query.as_deref()),
            headers: pairs(r.headers.as_deref()),
            body: r.body.as_ref().and_then(body),
            auth: r.auth.as_ref().map(auth),
            variables: variables(r.variables.as_ref()),
            ..ImportedRequest::default()
        }
    }

    /// Note the request features the IR (and so every target) lacks.
    fn note_lost(&mut self, r: &HttpRequest, at: &Location) {
        let assertions = r.assertions.as_ref().map_or(0, Vec::len);
        let captures = r.captures.as_ref().map_or(0, Vec::len);
        let mut lost = Vec::new();
        if assertions > 0 {
            lost.push(format!("{assertions} assertions"));
        }
        if captures > 0 {
            lost.push(format!("{captures} captures"));
        }
        if r.settings.is_some() {
            lost.push("settings".to_owned());
        }
        if r.tags.as_ref().is_some_and(|t| !t.is_empty()) {
            lost.push("tags".to_owned());
        }
        if !lost.is_empty() {
            let message = format!("{} not exported (no equivalent)", lost.join(", "));
            self.notes
                .push(Note::new(NoteKind::Unsupported, message, at));
        }
    }

    fn graphql(&mut self, text: &str, rel: &Path) -> Result<ImportedRequest, ImportError> {
        let file: GraphqlFile = from_yaml(rel, text)
            .map_err(|e| ImportError::Export(format!("{}: {e}", rel.display())))?;
        let variables = file
            .variables
            .map(|v| serde_json::to_string(&v.0).expect("JSON values serialize"))
            .or_else(|| commented_variables(text));
        Ok(ImportedRequest {
            name: file.name,
            description: file.description,
            method: "POST".to_owned(),
            url: file.endpoint,
            headers: pairs(file.headers.as_deref()),
            body: Some(ImportedBody::Graphql {
                query: file.document,
                variables,
            }),
            auth: file.auth.as_ref().map(auth),
            ..ImportedRequest::default()
        })
    }
}

/// GraphQL variables kept as `# graphql-variables:` comment lines (when
/// they are not a flat object).
fn commented_variables(text: &str) -> Option<String> {
    let lines: Vec<&str> = text
        .lines()
        .filter_map(|l| {
            l.trim_end_matches('\r')
                .strip_prefix("# graphql-variables: ")
        })
        .collect();
    (!lines.is_empty()).then(|| lines.join("\n"))
}

/// The fields of a `kind: graphql` request file the IR carries.
#[derive(Deserialize)]
struct GraphqlFile {
    name: String,
    description: Option<String>,
    endpoint: String,
    document: String,
    variables: Option<JsonValue>,
    headers: Option<Vec<NameValue>>,
    auth: Option<Auth>,
}

fn location(rel: &Path) -> Location {
    let parts: Vec<String> = rel
        .components()
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
        .collect();
    Location {
        file: parts.join("/"),
        item: None,
        line: None,
    }
}

fn pairs(list: Option<&[NameValue]>) -> Vec<Pair> {
    list.unwrap_or_default()
        .iter()
        .map(|nv| Pair::new(&nv.name, &nv.value))
        .collect()
}

fn variables(vars: Option<&Variables>) -> Vec<Variable> {
    vars.map(|v| {
        v.iter()
            .map(|(name, value)| Variable {
                name: name.to_owned(),
                value: value.to_string(),
                secret: false,
                path: false,
            })
            .collect()
    })
    .unwrap_or_default()
}

fn body(b: &Body) -> Option<ImportedBody> {
    Some(match b {
        Body::None => return None,
        Body::Json(JsonContent::Text(text)) => ImportedBody::Json { text: text.clone() },
        Body::Json(JsonContent::Value(value)) => ImportedBody::Json {
            text: serde_json::to_string_pretty(value).expect("JSON values serialize"),
        },
        Body::Text(text) => ImportedBody::Text { text: text.clone() },
        Body::Form(fields) => ImportedBody::Form {
            fields: pairs(Some(fields)),
        },
        Body::Multipart(parts) => ImportedBody::Multipart {
            parts: parts.iter().map(part).collect(),
        },
        Body::File(path) => ImportedBody::File {
            path: path.get().clone(),
        },
    })
}

fn part(p: &Part) -> ImportedPart {
    ImportedPart {
        name: p.name.clone(),
        value: match &p.source {
            PartSource::Text(text) => PartValue::Text(text.clone()),
            PartSource::File(path) => PartValue::File(path.get().clone()),
        },
        content_type: p.content_type.clone(),
    }
}

pub(crate) fn auth(a: &Auth) -> ImportedAuth {
    match a {
        Auth::None => ImportedAuth::None,
        Auth::Basic { username, password } => ImportedAuth::Basic {
            username: username.clone(),
            password: password.clone(),
        },
        Auth::Bearer { token } => ImportedAuth::Bearer {
            token: token.clone(),
        },
        Auth::ApiKey {
            name,
            value,
            location,
        } => ImportedAuth::ApiKey {
            name: name.clone(),
            value: value.clone(),
            location: match location {
                Some(ApiKeyLocation::Query) => ApiKeyIn::Query,
                _ => ApiKeyIn::Header,
            },
        },
        Auth::OAuth2(o) => ImportedAuth::Oauth2(ImportedOAuth2 {
            grant: match o.grant {
                OAuth2Grant::ClientCredentials => Grant::ClientCredentials,
                OAuth2Grant::AuthorizationCode => Grant::AuthorizationCode,
            },
            token_url: o.token_url.clone(),
            auth_url: o.auth_url.clone(),
            client_id: o.client_id.clone(),
            client_secret: o.client_secret.clone(),
            scopes: o.scopes.clone().unwrap_or_default(),
        }),
        Auth::Digest { .. } => ImportedAuth::Unsupported("digest".to_owned()),
        Auth::AwsSigV4(_) => ImportedAuth::Unsupported("aws_sigv4".to_owned()),
        Auth::Plugin(_) => ImportedAuth::Unsupported("plugin".to_owned()),
        Auth::Jwt(_) => ImportedAuth::Unsupported("jwt".to_owned()),
    }
}
