//! `rustman export curl`: a resolved request as one POSIX-shell-quoted curl
//! command (design D13). Every value goes through the [`Redactor`] unless
//! secrets are shown on purpose.

use std::borrow::Cow;

use rustman_core::Redactor;
use rustman_core::model::{ApiKeyLocation, Auth};
use rustman_core::redact::sensitive_header;
use rustman_core::secret::placeholder;
use rustman_core::vars::{
    Resolved, ResolvedBody, ResolvedPair, ResolvedPart, ResolvedPartSource, ResolvedRequest,
};

use crate::wire;

/// A curl command and what it leaves out.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Exported {
    pub command: String,
    pub warnings: Vec<String>,
}

/// `request` as a curl command. Secret values are redacted unless
/// `show_secrets`.
pub fn export_curl(request: &ResolvedRequest, show_secrets: bool) -> Exported {
    let shown = Shown {
        redactor: (!show_secrets).then(|| Redactor::for_request(request)),
    };
    let mut out = Command::default();
    out.method(request);
    out.arg(shown.text(&url(request)).into_owned());
    out.auth(request, &shown);
    for pair in &request.headers {
        if !computed_by_engine(&pair.name.text) {
            out.header(&pair.name.text, &shown.header(&pair.name.text, &pair.value));
        }
    }
    if let Some(content_type) = implied_content_type(request) {
        out.header("Content-Type", content_type);
    }
    out.body(&request.body, &shown);
    if request.settings.verify_tls == Some(false) {
        out.arg("-k");
    }
    if request.settings.follow_redirects != Some(false) {
        out.arg("-L");
    }
    out.finish()
}

/// How values are shown: redacted, or as they are with `--show-secrets`.
struct Shown {
    redactor: Option<Redactor>,
}

impl Shown {
    fn text<'a>(&self, text: &'a str) -> Cow<'a, str> {
        match &self.redactor {
            Some(redactor) => redactor.text(text),
            None => Cow::Borrowed(text),
        }
    }

    fn resolved(&self, value: &Resolved) -> String {
        match &self.redactor {
            Some(redactor) => redactor.resolved(value),
            None => value.text.clone(),
        }
    }

    /// A header value; credentials (`Authorization`, `Cookie`, ...) are
    /// replaced whole, by the placeholders of the secrets they contain, or by
    /// the header's own placeholder.
    fn header(&self, name: &str, value: &Resolved) -> String {
        let redacted = self.resolved(value);
        match sensitive_header(name) {
            Some(canonical) if self.redactor.is_some() => {
                let secrets = placeholders(&redacted);
                if secrets.is_empty() {
                    placeholder(canonical)
                } else {
                    secrets
                }
            }
            _ => redacted,
        }
    }
}

/// Every `«redacted:...»` placeholder in `text`, concatenated.
fn placeholders(text: &str) -> String {
    const START: &str = "«redacted:";
    let mut found = String::new();
    let mut rest = text;
    while let Some(start) = rest.find(START) {
        let tail = &rest[start..];
        let end = tail.find('»').map_or(tail.len(), |e| e + '»'.len_utf8());
        found.push_str(&tail[..end]);
        rest = &tail[end..];
    }
    found
}

/// The URL as sent: query appended, no fragment.
fn url(request: &ResolvedRequest) -> String {
    let mut query = request.query.clone();
    if let Some(Auth::ApiKey {
        name,
        value,
        location: Some(ApiKeyLocation::Query),
    }) = &request.auth
    {
        query.push(ResolvedPair {
            name: Resolved::literal(name.clone()),
            value: Resolved::literal(value.clone()),
        });
    }
    wire::uri(&request.url.text, &query)
        .map_or_else(|_| request.url.text.clone(), |uri| uri.to_string())
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

/// The `Content-Type` Rustman sends by default where curl's differs.
fn implied_content_type(request: &ResolvedRequest) -> Option<&'static str> {
    if has_header(request, "Content-Type") {
        return None;
    }
    match request.body {
        ResolvedBody::Json(_) | ResolvedBody::JsonText(_) => Some("application/json"),
        ResolvedBody::Text(_) => Some("text/plain; charset=utf-8"),
        ResolvedBody::File(_) => Some("application/octet-stream"),
        ResolvedBody::None | ResolvedBody::Form(_) | ResolvedBody::Multipart(_) => None,
    }
}

/// The command's words, quoted when written out.
#[derive(Default)]
struct Command {
    words: Vec<String>,
    warnings: Vec<String>,
}

impl Command {
    fn arg(&mut self, word: impl Into<String>) {
        self.words.push(word.into());
    }

    fn option(&mut self, name: &str, value: impl Into<String>) {
        self.arg(name);
        self.arg(value);
    }

    fn header(&mut self, name: &str, value: &str) {
        self.option("-H", format!("{name}: {value}"));
    }

    /// `-X` unless curl's own choice (GET, or POST with a body) matches.
    fn method(&mut self, request: &ResolvedRequest) {
        let has_body = !matches!(request.body, ResolvedBody::None);
        let implied = if has_body { "POST" } else { "GET" };
        if request.method != implied {
            self.option("-X", request.method.clone());
        }
    }

    fn auth(&mut self, request: &ResolvedRequest, shown: &Shown) {
        let explicit_authorization = has_header(request, "Authorization");
        match &request.auth {
            Some(Auth::Basic { username, password }) if !explicit_authorization => {
                let user = format!("{username}:{password}");
                self.option("-u", shown.text(&user).into_owned());
            }
            Some(Auth::Bearer { token }) if !explicit_authorization => {
                let value = Resolved::literal(format!("Bearer {token}"));
                self.header("Authorization", &shown.header("Authorization", &value));
            }
            Some(Auth::ApiKey {
                name,
                value,
                location: None | Some(ApiKeyLocation::Header),
            }) if !has_header(request, name) => {
                let value = shown.header(name, &Resolved::literal(value.clone()));
                self.header(name, &value);
            }
            Some(Auth::OAuth2(_)) => self.warnings.push(
                "the OAuth2 access token is not included; send the request with rustman to acquire it"
                    .into(),
            ),
            _ => {}
        }
    }

    fn body(&mut self, body: &ResolvedBody, shown: &Shown) {
        match body {
            ResolvedBody::None => {}
            ResolvedBody::Json(value) => {
                let text = serde_json::to_string(value).expect("JSON values serialize");
                self.option("--data-raw", shown.text(&text).into_owned());
            }
            ResolvedBody::JsonText(text) | ResolvedBody::Text(text) => {
                self.option("--data-raw", shown.resolved(text));
            }
            ResolvedBody::Form(fields) => {
                self.option("--data-raw", shown.text(&wire::form(fields)).into_owned());
            }
            ResolvedBody::File(file) => self.option("--data-binary", format!("@{}", file.raw)),
            ResolvedBody::Multipart(parts) => {
                for part in parts {
                    self.part(part, shown);
                }
            }
        }
    }

    /// `--form-string name=value` for plain text; `-F` with a quoted value
    /// when there is a type, or a file.
    fn part(&mut self, part: &ResolvedPart, shown: &Shown) {
        let type_param = part
            .content_type
            .as_deref()
            .map(|ct| format!(";type={ct}"))
            .unwrap_or_default();
        match &part.source {
            ResolvedPartSource::Text(text) if type_param.is_empty() => {
                let value = shown.resolved(text);
                self.option("--form-string", format!("{}={value}", part.name));
            }
            ResolvedPartSource::Text(text) => {
                let value = quote(&shown.resolved(text));
                self.option("-F", format!("{}={value}{type_param}", part.name));
            }
            ResolvedPartSource::File(file) => {
                let path = quote_if_needed(&file.raw);
                self.option("-F", format!("{}=@{path}{type_param}", part.name));
            }
        }
    }

    fn finish(self) -> Exported {
        let mut command = String::from("curl");
        for word in &self.words {
            command.push(' ');
            command.push_str(&shell_escape::unix::escape(Cow::Borrowed(word.as_str())));
        }
        Exported {
            command,
            warnings: self.warnings,
        }
    }
}

/// A `-F` value in double quotes, with `"` and `\` escaped.
fn quote(value: &str) -> String {
    format!("\"{}\"", value.replace('\\', "\\\\").replace('"', "\\\""))
}

fn quote_if_needed(value: &str) -> String {
    if value.contains([';', '"', '\\']) {
        quote(value)
    } else {
        value.to_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn placeholders_are_collected_in_order() {
        assert_eq!(
            placeholders("Bearer «redacted:a»x«redacted:b»"),
            "«redacted:a»«redacted:b»"
        );
        assert_eq!(placeholders("plain"), "");
        assert_eq!(placeholders("«redacted:open"), "«redacted:open");
    }

    #[test]
    fn form_values_are_quoted_for_curl() {
        assert_eq!(quote(r#"a"b\c"#), r#""a\"b\\c""#);
        assert_eq!(quote_if_needed("a.png"), "a.png");
        assert_eq!(quote_if_needed("a;b"), "\"a;b\"");
    }
}
