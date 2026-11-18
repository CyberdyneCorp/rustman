//! `rustman export curl`: a resolved request as one POSIX-shell-quoted curl
//! command (design D13). Every value goes through the [`Redactor`] unless
//! secrets are shown on purpose.

use std::path::Path;

use rustman_core::model::{ApiKeyLocation, Auth, AwsCredentials, AwsSigV4};
use rustman_core::vars::{
    Resolved, ResolvedBody, ResolvedFile, ResolvedPart, ResolvedPartSource, ResolvedRequest,
};

use crate::shown::Shown;
use crate::snippet::{shell_word, url};

/// A curl command and what it leaves out.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Exported {
    pub command: String,
    pub warnings: Vec<String>,
}

/// `request` as a curl command. Secret values are redacted unless
/// `show_secrets`.
pub fn export_curl(request: &ResolvedRequest, show_secrets: bool) -> Exported {
    curl_command(
        request,
        &CurlOptions {
            show_secrets,
            ..CurlOptions::default()
        },
    )
}

/// What a curl command is built for: `export curl`, or a snippet.
#[derive(Default)]
pub(crate) struct CurlOptions<'a> {
    pub show_secrets: bool,
    /// Snippets: curl's own Digest and SigV4 options instead of a warning.
    pub signature_helpers: bool,
    /// Placeholder mode: `{{name}}` markers are kept unencoded.
    pub keep_markers: bool,
    /// Snippets: files named relative to this workspace root.
    pub root: Option<&'a Path>,
}

/// `request` as a curl command, built for `options`.
pub(crate) fn curl_command(request: &ResolvedRequest, options: &CurlOptions<'_>) -> Exported {
    let shown = Shown::new(request, options.show_secrets);
    let mut out = Command {
        options,
        words: Vec::new(),
        warnings: Vec::new(),
    };
    out.method(request);
    out.url(
        shown
            .text(&url::request_url(request, options.keep_markers))
            .into_owned(),
    );
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
struct Command<'o> {
    options: &'o CurlOptions<'o>,
    words: Vec<String>,
    warnings: Vec<String>,
}

impl Command<'_> {
    fn arg(&mut self, word: impl Into<String>) {
        self.words.push(word.into());
    }

    fn option(&mut self, name: &str, value: impl Into<String>) {
        self.arg(name);
        self.arg(value);
    }

    /// The URL, after `--url` when curl would read it as an option.
    fn url(&mut self, url: String) {
        if url.starts_with('-') {
            self.option("--url", url);
        } else {
            self.arg(url);
        }
    }

    /// `-H 'Name: value'`, or `-H 'Name;'` for an empty value: curl reads
    /// `Name:` with nothing after it as "do not send this header". A name
    /// that is not an HTTP token is left out (the engine refuses it too):
    /// `-H @file` would read headers from a file.
    fn header(&mut self, name: &str, value: &str) {
        if !is_token(name) {
            self.warnings.push(format!(
                "header '{name}' is not a valid header name; it is left out"
            ));
            return;
        }
        if value.is_empty() {
            self.option("-H", format!("{name};"));
        } else {
            self.option("-H", format!("{name}: {value}"));
        }
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
            Some(auth) if self.options.signature_helpers => self.signature(auth, shown),
            Some(Auth::Digest { .. }) => self.signature_warning("digest"),
            Some(Auth::AwsSigV4(_)) => self.signature_warning("aws_sigv4"),
            Some(Auth::Jwt(_)) => self.signature_warning("jwt"),
            _ => {}
        }
    }

    /// curl's `--digest` and `--aws-sigv4`. JWT has no curl option; the
    /// snippet says so itself.
    fn signature(&mut self, auth: &Auth, shown: &Shown) {
        match auth {
            Auth::Digest { username, password } => {
                self.arg("--digest");
                let password = shown.credential(password, "password");
                self.option("-u", format!("{}:{password}", shown.text(username)));
            }
            Auth::AwsSigV4(sigv4) => self.sigv4(sigv4, shown),
            _ => {}
        }
    }

    fn sigv4(&mut self, sigv4: &AwsSigV4, shown: &Shown) {
        let provider = format!("aws:amz:{}:{}", sigv4.region, sigv4.service);
        self.option("--aws-sigv4", provider);
        let (access_key, secret_key, token) = match &sigv4.credentials {
            AwsCredentials::Fields {
                access_key,
                secret_key,
                session_token,
            } => (
                shown.text(access_key).into_owned(),
                shown.credential(secret_key, "aws_secret_key"),
                session_token
                    .as_deref()
                    .map(|t| shown.credential(t, "aws_session_token")),
            ),
            AwsCredentials::Env | AwsCredentials::Profile(_) => {
                self.warnings.push(
                    "aws_sigv4 credentials from the environment or a profile are not included"
                        .into(),
                );
                let key = rustman_core::secret::placeholder("aws_access_key");
                (
                    key,
                    rustman_core::secret::placeholder("aws_secret_key"),
                    None,
                )
            }
        };
        self.option("-u", format!("{access_key}:{secret_key}"));
        if let Some(token) = token {
            self.header("x-amz-security-token", &token);
        }
    }

    /// Signature auth is computed per request, so it cannot be written as
    /// a fixed header.
    fn signature_warning(&mut self, auth: &str) {
        self.warnings.push(format!(
            "{auth} auth is not included; send the request with rustman to sign it"
        ));
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
                let form = url::form(fields, self.options.keep_markers);
                self.option("--data-raw", shown.text(&form).into_owned());
            }
            ResolvedBody::File(file) => {
                let path = self.path(file);
                self.option("--data-binary", format!("@{path}"));
            }
            ResolvedBody::Multipart(parts) => {
                for part in parts {
                    self.part(part, shown);
                }
            }
        }
    }

    /// `--form-string name=value` for plain text; `-F` with a quoted value
    /// when there is a type, or a file. `--form-string` never reads a file;
    /// `-F` does for a value starting with `@` or `<` and for a `headers=@`
    /// parameter, so its value is quoted and its type must be a plain media
    /// type. Both split at the first `=`, so a name holding one cannot be
    /// written.
    fn part(&mut self, part: &ResolvedPart, shown: &Shown) {
        let type_param = match part.content_type.as_deref() {
            Some(ct) if plain_media_type(ct) => format!(";type={ct}"),
            Some(ct) => {
                self.warnings.push(format!(
                    "the content type '{ct}' of part '{}' cannot be written for curl -F; it is left out",
                    part.name
                ));
                String::new()
            }
            None => String::new(),
        };
        if part.name.contains('=') {
            self.warnings.push(format!(
                "part '{}' has '=' in its name, which curl cannot send; it is left out",
                part.name
            ));
            return;
        }
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
                let path = quote_if_needed(&self.path(file));
                self.option("-F", format!("{}=@{path}{type_param}", part.name));
            }
        }
    }

    fn path(&self, file: &ResolvedFile) -> String {
        url::file_path(file, self.options.root)
    }

    fn finish(self) -> Exported {
        if self.words.iter().any(|w| w.contains('\0')) {
            let mut warnings = self.warnings;
            warnings.push(NUL_WARNING.replace("{tool}", "curl"));
            return Exported {
                command: nul_refusal(),
                warnings,
            };
        }
        let mut command = String::from("curl");
        for word in &self.words {
            command.push(' ');
            command.push_str(&shell_word(word));
        }
        Exported {
            command,
            warnings: self.warnings,
        }
    }
}

/// The warning for a request a shell command cannot carry: shells cut an
/// argument at its first NUL byte, so the command would send something else.
pub(crate) const NUL_WARNING: &str =
    "snippet: {tool} cannot be given a NUL byte in a shell argument; send the request with rustman";

/// The command written instead: a comment, so nothing different is sent.
pub(crate) fn nul_refusal() -> String {
    "# this request contains a NUL byte, which a shell argument cannot carry; \
     send it with rustman or put the body in a file"
        .to_owned()
}

/// A `-F` value in double quotes, with `"` and `\` escaped.
fn quote(value: &str) -> String {
    format!("\"{}\"", value.replace('\\', "\\\\").replace('"', "\\\""))
}

fn quote_if_needed(value: &str) -> String {
    if value.contains([';', ',', '"', '\\']) {
        quote(value)
    } else {
        value.to_owned()
    }
}

/// Whether `name` is an RFC 9110 token (a valid header name).
fn is_token(name: &str) -> bool {
    !name.is_empty()
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"!#$%&'*+-.^_`|~".contains(&b))
}

/// Whether `content_type` can follow `;type=` in a curl `-F` value: curl
/// cuts the type at `,` and reads `;headers=@file` (or `filename=`,
/// `encoder=`) after it as its own parameters, quoted or not, so only plain
/// `type/subtype` with token parameters (`; charset=utf-8`) is written.
fn plain_media_type(content_type: &str) -> bool {
    let mut parts = content_type.split(';');
    let essence = parts.next().unwrap_or_default();
    let valid_essence = essence
        .split_once('/')
        .is_some_and(|(t, s)| is_token(t) && is_token(s));
    valid_essence
        && parts.all(|param| {
            param
                .trim_start()
                .split_once('=')
                .is_some_and(|(name, value)| {
                    is_token(name)
                        && is_token(value)
                        && !["type", "filename", "headers", "encoder"]
                            .iter()
                            .any(|k| name.eq_ignore_ascii_case(k))
                })
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn form_values_are_quoted_for_curl() {
        assert_eq!(quote(r#"a"b\c"#), r#""a\"b\\c""#);
        assert_eq!(quote_if_needed("a.png"), "a.png");
        assert_eq!(quote_if_needed("a;b"), "\"a;b\"");
        assert_eq!(quote_if_needed("a,b"), "\"a,b\"");
    }

    #[test]
    fn only_plain_media_types_follow_type() {
        for ok in ["image/png", "text/plain; charset=utf-8", "a/b;x=y"] {
            assert!(plain_media_type(ok), "{ok}");
        }
        for bad in [
            "text/plain;headers=@/etc/passwd",
            "text/plain; filename=x",
            "text/plain,x/y",
            "text/plain; charset=\"x\"",
            "@/etc/passwd",
            "text",
        ] {
            assert!(!plain_media_type(bad), "{bad}");
        }
    }
}
