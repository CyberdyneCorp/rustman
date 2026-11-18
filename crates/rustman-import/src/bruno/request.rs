//! A parsed `.bru` file as an IR request, and the settings `collection.bru`
//! and `folder.bru` share with requests (headers, auth, variables, scripts).

use super::bru::{Block, Content};
use crate::common;
use crate::ir::{
    ApiKeyIn, CaptureSpec, Check, Grant, ImportedAuth, ImportedBody, ImportedOAuth2, ImportedPart,
    ImportedRequest, JsonOp, Location, Note, NoteKind, Pair, PartValue, Script, Variable,
};

const METHODS: &[&str] = &[
    "get", "post", "put", "delete", "patch", "options", "head", "connect", "trace",
];

/// The blocks of one `.bru` file, read from `file`.
pub(super) struct Bru<'a> {
    pub blocks: &'a [Block],
    pub file: &'a str,
    pub notes: Vec<Note>,
}

impl<'a> Bru<'a> {
    fn block(&self, name: &str) -> Option<&'a Block> {
        let blocks: &'a [Block] = self.blocks;
        blocks.iter().find(|b| b.name == name)
    }

    pub fn at(&self, line: u32) -> Location {
        Location {
            file: self.file.to_owned(),
            item: None,
            line: Some(line),
        }
    }

    fn note(&mut self, kind: NoteKind, message: String, line: u32) {
        let at = self.at(line);
        self.notes.push(Note::new(kind, message, &at));
    }

    /// `meta.name`, falling back to `fallback`.
    pub fn name(&self, fallback: &str) -> String {
        self.block("meta")
            .and_then(|m| m.get("name"))
            .filter(|n| !n.is_empty())
            .unwrap_or(fallback)
            .to_owned()
    }

    /// `meta.seq`, for ordering.
    pub fn seq(&self) -> Option<u32> {
        self.block("meta")?.get("seq")?.parse().ok()
    }

    /// The request of this file, or `None` when it has no method block.
    pub fn request(&mut self, name: String) -> Option<ImportedRequest> {
        let method_block = self
            .blocks
            .iter()
            .find(|b| METHODS.contains(&b.name.as_str()))?;
        let at = self.at(method_block.line);
        let (url, mut query) = common::split_url(method_block.get("url").unwrap_or_default());
        if let Some(params) = self.block("params:query") {
            query = self.pairs(params, "query parameter");
        }
        let mut variables = self.variables("vars:pre-request");
        let url = self.path_params(url, &mut variables);
        let body_mode = method_block.get("body").unwrap_or("none").to_owned();
        let auth_mode = method_block.get("auth").unwrap_or("inherit").to_owned();
        Some(ImportedRequest {
            name,
            description: self.docs(),
            method: method_block.name.to_ascii_uppercase(),
            url,
            query,
            headers: self.headers(),
            body: self.body(&body_mode),
            auth: self.auth(&auth_mode),
            variables,
            scripts: self.scripts(),
            checks: self.asserts(),
            captures: self.captures(),
            statements: Vec::new(),
            location: at,
        })
    }

    /// `/users/:id` with `params:path` becomes `/users/{{id}}`, and `id` a
    /// request variable.
    fn path_params(&mut self, url: String, variables: &mut Vec<Variable>) -> String {
        let Some(block) = self.block("params:path").cloned() else {
            return url;
        };
        let mut url = url;
        for entry in block.entries().filter(|e| e.enabled) {
            let segment = format!("/:{}", entry.key);
            url = url.replace(&segment, &format!("/{{{{{}}}}}", entry.key));
            variables.push(Variable {
                name: entry.key.clone(),
                value: entry.value.clone(),
                secret: false,
                path: true,
            });
        }
        url
    }

    /// The enabled entries of `block`; disabled ones are dropped and noted.
    fn pairs(&mut self, block: &Block, what: &str) -> Vec<Pair> {
        let mut out = Vec::new();
        for entry in block.entries() {
            if entry.enabled {
                out.push(Pair::new(&entry.key, &entry.value));
            } else {
                let message = format!("disabled {what} {} dropped", entry.key);
                self.note(NoteKind::Dropped, message, entry.line);
            }
        }
        out
    }

    pub fn headers(&mut self) -> Vec<Pair> {
        match self.block("headers").cloned() {
            Some(block) => self.pairs(&block, "header"),
            None => Vec::new(),
        }
    }

    pub fn variables(&mut self, name: &str) -> Vec<Variable> {
        let Some(block) = self.block(name).cloned() else {
            return Vec::new();
        };
        self.pairs(&block, "variable")
            .into_iter()
            .map(|p| Variable {
                name: p.name,
                value: p.value,
                secret: false,
                path: false,
            })
            .collect()
    }

    pub fn docs(&self) -> Option<String> {
        self.block("docs")
            .map(|b| b.text().to_owned())
            .filter(|d| !d.trim().is_empty())
    }

    pub fn scripts(&self) -> Vec<Script> {
        let events = [
            ("script:pre-request", "pre-request"),
            ("script:post-response", "post-response"),
            ("tests", "tests"),
        ];
        events
            .iter()
            .filter_map(|(block, event)| {
                let b = self.block(block)?;
                (!b.text().trim().is_empty()).then(|| Script {
                    event: (*event).to_owned(),
                    source: b.text().to_owned(),
                    location: self.at(b.line),
                })
            })
            .collect()
    }

    fn body(&mut self, mode: &str) -> Option<ImportedBody> {
        let text = |name: &str| self.block(name).map(|b| b.text().to_owned());
        match mode {
            "none" => None,
            "json" => Some(ImportedBody::Json {
                text: text("body:json")?,
            }),
            "text" | "xml" | "sparql" => Some(ImportedBody::Text {
                text: text(&format!("body:{mode}"))?,
            }),
            "graphql" => Some(ImportedBody::Graphql {
                query: text("body:graphql").unwrap_or_default(),
                variables: text("body:graphql:vars").filter(|v| !v.trim().is_empty()),
            }),
            "formUrlEncoded" => {
                let block = self.block("body:form-urlencoded")?.clone();
                Some(ImportedBody::Form {
                    fields: self.pairs(&block, "form field"),
                })
            }
            "multipartForm" => self.multipart(),
            other => {
                let line = self.blocks.first().map_or(1, |b| b.line);
                self.note(
                    NoteKind::Unsupported,
                    format!("body mode '{other}' unsupported"),
                    line,
                );
                None
            }
        }
    }

    fn multipart(&mut self) -> Option<ImportedBody> {
        let block = self.block("body:multipart-form")?.clone();
        let parts = self
            .pairs(&block, "form part")
            .into_iter()
            .map(|p| {
                let file = p
                    .value
                    .strip_prefix("@file(")
                    .and_then(|v| v.strip_suffix(')'));
                ImportedPart {
                    name: p.name,
                    value: match file {
                        Some(path) => PartValue::File(path.to_owned()),
                        None => PartValue::Text(p.value),
                    },
                    content_type: None,
                }
            })
            .collect();
        Some(ImportedBody::Multipart { parts })
    }

    /// The auth of `mode` (`inherit` and a missing mode inherit); an
    /// unsupported mode is reported and disables auth, so no parent credential
    /// is inherited.
    pub fn auth(&mut self, mode: &str) -> Option<ImportedAuth> {
        let block = self.block(&format!("auth:{mode}"));
        let line = block.map_or(1, |b| b.line);
        let get = |key: &str| {
            block
                .and_then(|b| b.get(key))
                .unwrap_or_default()
                .to_owned()
        };
        let auth = match mode {
            "inherit" => return None,
            "none" => ImportedAuth::None,
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
                location: if get("placement") == "queryparams" {
                    ApiKeyIn::Query
                } else {
                    ApiKeyIn::Header
                },
            },
            "oauth2" => return self.oauth2(&get, line),
            other => {
                self.note(
                    NoteKind::Unsupported,
                    format!("{other} auth unsupported"),
                    line,
                );
                return Some(ImportedAuth::None);
            }
        };
        Some(auth)
    }

    fn oauth2(&mut self, get: &dyn Fn(&str) -> String, line: u32) -> Option<ImportedAuth> {
        let grant = match get("grant_type").as_str() {
            "client_credentials" => Grant::ClientCredentials,
            "" | "authorization_code" => Grant::AuthorizationCode,
            other => {
                let message = format!("oauth2 auth with grant '{other}' unsupported");
                self.note(NoteKind::Unsupported, message, line);
                return Some(ImportedAuth::None);
            }
        };
        let optional = |key: &str| Some(get(key)).filter(|v| !v.is_empty());
        let Some(token_url) = optional("access_token_url") else {
            let message = "oauth2 auth without an access token URL unsupported".to_owned();
            self.note(NoteKind::Unsupported, message, line);
            return Some(ImportedAuth::None);
        };
        Some(ImportedAuth::Oauth2(ImportedOAuth2 {
            grant,
            token_url,
            auth_url: match grant {
                Grant::AuthorizationCode => optional("authorization_url"),
                Grant::ClientCredentials => None,
            },
            client_id: get("client_id"),
            client_secret: optional("client_secret"),
            scopes: get("scope").split_whitespace().map(str::to_owned).collect(),
        }))
    }

    /// `assert` entries on `res.status` and `res.body...` as checks.
    fn asserts(&mut self) -> Vec<Check> {
        let Some(block) = self.block("assert").cloned() else {
            return Vec::new();
        };
        let mut out = Vec::new();
        for entry in block.entries().filter(|e| e.enabled) {
            match assertion(&entry.key, &entry.value) {
                Some(check) => out.push(check),
                None => {
                    let message = format!("assert {}: {} unsupported", entry.key, entry.value);
                    self.note(NoteKind::Unsupported, message, entry.line);
                }
            }
        }
        out
    }

    /// `vars:post-response` entries reading `res.body...` as captures.
    fn captures(&mut self) -> Vec<CaptureSpec> {
        let Some(block) = self.block("vars:post-response").cloned() else {
            return Vec::new();
        };
        let mut out = Vec::new();
        for entry in block.entries().filter(|e| e.enabled) {
            match body_path(&entry.value) {
                Some(path) => out.push(CaptureSpec::json(entry.key.clone(), path)),
                None => {
                    let message = format!("post-response variable {} unsupported", entry.key);
                    self.note(NoteKind::Unsupported, message, entry.line);
                }
            }
        }
        out
    }
}

/// `res.body.a.b` (or `res.body`) as the JSONPath `$.a.b` (or `$`).
fn body_path(expr: &str) -> Option<String> {
    let rest = expr.trim().strip_prefix("res.body")?;
    let valid = rest.is_empty() || rest.starts_with('.') || rest.starts_with('[');
    valid.then(|| format!("${rest}"))
}

/// The check for the Bruno assertion `key: op operand`.
pub(super) fn assertion(key: &str, value: &str) -> Option<Check> {
    let (op, operand) = value
        .trim()
        .split_once(char::is_whitespace)
        .map_or((value.trim(), ""), |(op, rest)| (op, rest.trim()));
    if key == "res.status" {
        return if op == "eq" {
            operand.parse().ok().map(Check::Status)
        } else {
            None
        };
    }
    let path = body_path(key)?;
    let op = match op {
        "eq" => JsonOp::Equals,
        "neq" => JsonOp::NotEquals,
        "gt" => JsonOp::Gt,
        "lt" => JsonOp::Lt,
        "contains" => JsonOp::Contains,
        "isDefined" => JsonOp::Exists,
        _ => return None,
    };
    Some(Check::Json {
        path,
        op,
        value: operand.to_owned(),
    })
}

/// The variables of an environment `.bru`: `vars` values and the names of
/// `vars:secret` (whose values Bruno never exports).
pub(super) fn environment_variables(bru: &mut Bru<'_>) -> Vec<Variable> {
    let mut out = bru.variables("vars");
    let secrets = bru.block("vars:secret").map(|b| b.content.clone());
    if let Some(Content::List(names)) = secrets {
        for name in names.into_iter().filter(|n| !n.starts_with('~')) {
            // A name listed in `vars` too keeps its value but becomes secret.
            match out.iter_mut().find(|v| v.name == name) {
                Some(listed) => listed.secret = true,
                None => out.push(Variable {
                    name,
                    value: String::new(),
                    secret: true,
                    path: false,
                }),
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_and_body_assertions_map() {
        assert_eq!(assertion("res.status", "eq 200"), Some(Check::Status(200)));
        assert_eq!(
            assertion("res.body.data.id", "isDefined"),
            Some(Check::Json {
                path: "$.data.id".into(),
                op: JsonOp::Exists,
                value: String::new()
            })
        );
        for (op, expected) in [
            ("eq", JsonOp::Equals),
            ("neq", JsonOp::NotEquals),
            ("gt", JsonOp::Gt),
            ("lt", JsonOp::Lt),
            ("contains", JsonOp::Contains),
        ] {
            let check = assertion("res.body.items[0]", &format!("{op} 3"));
            assert_eq!(
                check,
                Some(Check::Json {
                    path: "$.items[0]".into(),
                    op: expected,
                    value: "3".into()
                })
            );
        }
    }

    #[test]
    fn other_assertions_are_not_mapped() {
        assert_eq!(assertion("res.status", "gt 199"), None);
        assert_eq!(assertion("res.headers.etag", "isDefined"), None);
        assert_eq!(assertion("res.body.a", "matches ^x"), None);
        assert_eq!(assertion("res.bodyx", "eq 1"), None);
    }
}
