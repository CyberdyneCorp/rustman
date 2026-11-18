//! The template input: one resolved request, redacted, with each value as
//! the snippet shows it. Templates only lay it out and quote it.

use std::path::Path;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use rustman_core::model::{ApiKeyLocation, Auth};
use rustman_core::vars::{
    PlaceholderVar, Resolved, ResolvedBody, ResolvedPart, ResolvedPartSource, ResolvedRequest,
};
use serde::Serialize;
use serde_json::Value;

use super::Lang;
use super::code::{Syntax, constant_name};
use super::url;
use crate::shown::Shown;

/// Methods every client library has a shorthand for.
const STANDARD_METHODS: [&str; 7] = ["GET", "POST", "PUT", "PATCH", "DELETE", "HEAD", "OPTIONS"];

#[derive(Serialize)]
pub(super) struct View {
    pub method: String,
    pub method_lower: String,
    pub standard_method: bool,
    pub url: String,
    pub headers: Vec<Header>,
    pub content_type: Option<String>,
    pub body: BodyView,
    /// A Digest helper the language has (the only native helper besides
    /// curl's own options, which the curl export writes).
    pub digest: Option<Credentials>,
    pub auth_comment: Option<String>,
    pub follow_redirects: bool,
    pub verify_tls: bool,
    pub constants: Vec<Constant>,
    /// httpie's request items (headers and parts), escaped but not quoted.
    pub items: Vec<String>,
}

#[derive(Serialize)]
pub(super) struct Header {
    pub name: String,
    pub value: String,
    pub is_content_type: bool,
}

#[derive(Serialize)]
pub(super) struct Credentials {
    pub username: String,
    pub password: String,
}

#[derive(Serialize)]
pub(super) struct Constant {
    pub name: String,
    pub value: String,
}

#[derive(Default, Serialize)]
pub(super) struct BodyView {
    /// `none`, `text` (JSON, text and form bodies), `file` or `multipart`.
    pub kind: &'static str,
    pub is_json: bool,
    pub text: String,
    /// A JSON body as a Python or JavaScript literal.
    pub code: String,
    pub path: String,
    pub parts: Vec<PartView>,
    pub any_file: bool,
    pub any_typed: bool,
}

#[derive(Serialize)]
pub(super) struct PartView {
    pub name: String,
    pub value: String,
    pub path: String,
    pub filename: String,
    pub content_type: Option<String>,
    pub is_file: bool,
    /// The part's `Content-Disposition` value.
    pub disposition: String,
}

/// How a view is built.
pub(super) struct Context<'a> {
    pub lang: Lang,
    pub syntax: Syntax,
    pub shown: Shown,
    pub placeholders: Option<&'a [PlaceholderVar]>,
    pub root: Option<&'a Path>,
}

impl Context<'_> {
    pub fn view(&self, request: &ResolvedRequest) -> View {
        let content_type = self.content_type(request);
        let url = self.text(&url::request_url(request, self.keep_markers()));
        View {
            method: request.method.clone(),
            method_lower: request.method.to_ascii_lowercase(),
            standard_method: STANDARD_METHODS.contains(&request.method.as_str()),
            url,
            headers: self.headers(request, content_type.as_deref()),
            content_type,
            body: self.body(&request.body),
            digest: self.digest(request.auth.as_ref()),
            auth_comment: auth_comment(request.auth.as_ref(), self.lang),
            follow_redirects: request.settings.follow_redirects != Some(false),
            verify_tls: request.settings.verify_tls != Some(false),
            constants: self.constants(),
            items: Vec::new(),
        }
    }

    fn keep_markers(&self) -> bool {
        self.placeholders.is_some()
    }

    fn text(&self, text: &str) -> String {
        self.shown.text(text).into_owned()
    }

    fn constants(&self) -> Vec<Constant> {
        if self.syntax == Syntax::Shell {
            return Vec::new();
        }
        let vars = self.placeholders.unwrap_or_default();
        vars.iter()
            .map(|v| Constant {
                name: constant_name(&v.name),
                value: self.text(&v.value),
            })
            .collect()
    }

    /// The request's headers, then those its auth and body imply.
    fn headers(&self, request: &ResolvedRequest, content_type: Option<&str>) -> Vec<Header> {
        let mut headers: Vec<Header> = request
            .headers
            .iter()
            .filter(|pair| !computed_by_engine(&pair.name.text))
            .map(|pair| self.header(&pair.name.text, &pair.value))
            .collect();
        if let Some((name, value)) = auth_header(request) {
            headers.push(self.header(&name, &Resolved::literal(value)));
        }
        if let Some(content_type) = content_type.filter(|_| !has_header(request, "Content-Type")) {
            headers.push(self.header("Content-Type", &Resolved::literal(content_type)));
        }
        headers
    }

    fn header(&self, name: &str, value: &Resolved) -> Header {
        Header {
            name: name.to_owned(),
            value: self.shown.header_with_scheme(name, value),
            is_content_type: name.eq_ignore_ascii_case("Content-Type"),
        }
    }

    /// The `Content-Type` sent: the request's own, or the one its body
    /// implies (multipart bodies get theirs from the client library).
    fn content_type(&self, request: &ResolvedRequest) -> Option<String> {
        let own = request
            .headers
            .iter()
            .find(|h| h.name.text.eq_ignore_ascii_case("Content-Type"));
        if let Some(own) = own {
            return Some(self.shown.resolved(&own.value));
        }
        let implied = match request.body {
            ResolvedBody::Json(_) | ResolvedBody::JsonText(_) => "application/json",
            ResolvedBody::Text(_) => "text/plain; charset=utf-8",
            ResolvedBody::File(_) => "application/octet-stream",
            ResolvedBody::Form(_) => "application/x-www-form-urlencoded",
            ResolvedBody::None | ResolvedBody::Multipart(_) => return None,
        };
        Some(implied.to_owned())
    }

    fn body(&self, body: &ResolvedBody) -> BodyView {
        let text = |text: String| BodyView {
            kind: "text",
            text,
            ..BodyView::default()
        };
        match body {
            ResolvedBody::None => BodyView {
                kind: "none",
                ..BodyView::default()
            },
            ResolvedBody::Json(value) => self.json_body(value),
            ResolvedBody::JsonText(value) => BodyView {
                is_json: true,
                ..text(self.shown.resolved(value))
            },
            ResolvedBody::Text(value) => text(self.shown.resolved(value)),
            ResolvedBody::Form(fields) => text(self.text(&url::form(fields, self.keep_markers()))),
            ResolvedBody::File(file) => BodyView {
                kind: "file",
                path: url::file_path(file, self.root),
                ..BodyView::default()
            },
            ResolvedBody::Multipart(parts) => self.multipart(parts),
        }
    }

    fn json_body(&self, value: &Value) -> BodyView {
        let redacted = self.redact_json(value);
        let syntax = match self.syntax {
            Syntax::Python => Syntax::Python,
            _ => Syntax::Js,
        };
        BodyView {
            kind: "text",
            is_json: true,
            text: serde_json::to_string(&redacted).expect("JSON values serialize"),
            code: syntax.json(&redacted, self.keep_markers()),
            ..BodyView::default()
        }
    }

    /// `value` with every string (keys included) redacted.
    fn redact_json(&self, value: &Value) -> Value {
        match value {
            Value::String(s) => Value::String(self.text(s)),
            Value::Array(items) => {
                Value::Array(items.iter().map(|v| self.redact_json(v)).collect())
            }
            Value::Object(map) => Value::Object(
                map.iter()
                    .map(|(k, v)| (self.text(k), self.redact_json(v)))
                    .collect(),
            ),
            other => other.clone(),
        }
    }

    fn multipart(&self, parts: &[ResolvedPart]) -> BodyView {
        let parts: Vec<PartView> = parts.iter().map(|p| self.part(p)).collect();
        BodyView {
            kind: "multipart",
            any_file: parts.iter().any(|p| p.is_file),
            any_typed: parts.iter().any(|p| p.is_file || p.content_type.is_some()),
            parts,
            ..BodyView::default()
        }
    }

    fn part(&self, part: &ResolvedPart) -> PartView {
        let (value, path, filename) = match &part.source {
            ResolvedPartSource::Text(text) => (self.shown.resolved(text), String::new(), None),
            ResolvedPartSource::File(file) => (
                String::new(),
                url::file_path(file, self.root),
                Some(url::file_name(file)),
            ),
        };
        let is_file = filename.is_some();
        let content_type = part
            .content_type
            .clone()
            .or_else(|| is_file.then(|| "application/octet-stream".to_owned()));
        PartView {
            disposition: disposition(&part.name, filename.as_deref()),
            name: part.name.clone(),
            value,
            path,
            filename: filename.unwrap_or_default(),
            content_type,
            is_file,
        }
    }

    fn digest(&self, auth: Option<&Auth>) -> Option<Credentials> {
        match auth {
            Some(Auth::Digest { username, password }) if self.lang.has_digest_helper() => {
                Some(Credentials {
                    username: self.text(username),
                    password: self.shown.credential(password, "password"),
                })
            }
            _ => None,
        }
    }
}

fn computed_by_engine(header: &str) -> bool {
    ["Content-Length", "Host"]
        .iter()
        .any(|h| h.eq_ignore_ascii_case(header))
}

fn has_header(request: &ResolvedRequest, name: &str) -> bool {
    request
        .headers
        .iter()
        .any(|h| h.name.text.eq_ignore_ascii_case(name))
}

/// The header Basic, Bearer and API-key auth send, unless the request sets
/// it itself.
fn auth_header(request: &ResolvedRequest) -> Option<(String, String)> {
    let (name, value) = match request.auth.as_ref()? {
        Auth::Basic { username, password } => {
            let encoded = STANDARD.encode(format!("{username}:{password}"));
            ("Authorization".to_owned(), format!("Basic {encoded}"))
        }
        Auth::Bearer { token } => ("Authorization".to_owned(), format!("Bearer {token}")),
        Auth::ApiKey {
            name,
            value,
            location: None | Some(ApiKeyLocation::Header),
        } => (name.clone(), value.clone()),
        _ => return None,
    };
    (!has_header(request, &name)).then_some((name, value))
}

/// The comment a snippet carries for auth computed per request that the
/// language has no helper for.
pub(super) fn auth_comment(auth: Option<&Auth>, lang: Lang) -> Option<String> {
    let kind = super::signature_kind(auth?)?;
    if lang.has_helper(kind) {
        return None;
    }
    let how = match kind {
        "digest" => "answer the server's challenge with an HTTP Digest helper",
        "aws_sigv4" => "sign with the AWS SDK signer",
        _ => "sign a JWT and send it as a bearer token",
    };
    Some(format!("{kind} auth: {how}"))
}

/// The `Content-Disposition` of a part, as Rustman sends it.
fn disposition(name: &str, filename: Option<&str>) -> String {
    let escape = |v: &str| {
        v.replace('"', "%22")
            .replace('\r', "%0D")
            .replace('\n', "%0A")
    };
    let mut value = format!("form-data; name=\"{}\"", escape(name));
    if let Some(filename) = filename {
        value.push_str(&format!("; filename=\"{}\"", escape(filename)));
    }
    value
}

#[cfg(test)]
mod tests {
    use super::*;
    use rustman_core::vars::ResolvedPair;

    #[test]
    fn dispositions_escape_quotes() {
        assert_eq!(
            disposition("a\"b", Some("x.png")),
            "form-data; name=\"a%22b\"; filename=\"x.png\""
        );
    }

    #[test]
    fn auth_headers_yield_to_the_request_s_own() {
        let mut request = ResolvedRequest {
            method: "GET".into(),
            url: Resolved::literal("http://x"),
            query: Vec::new(),
            headers: Vec::new(),
            body: ResolvedBody::None,
            auth: Some(Auth::Bearer { token: "t".into() }),
            settings: Default::default(),
            tls: Default::default(),
            secrets: Vec::new(),
        };
        assert_eq!(
            auth_header(&request),
            Some(("Authorization".into(), "Bearer t".into()))
        );
        request.headers.push(ResolvedPair {
            name: Resolved::literal("authorization"),
            value: Resolved::literal("x"),
        });
        assert_eq!(auth_header(&request), None);
    }
}
