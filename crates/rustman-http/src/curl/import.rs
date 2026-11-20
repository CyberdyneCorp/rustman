//! `rustman import curl`: a curl command line to a request file (design D13).
//!
//! The command is split with `shell-words`, then a hand-written parser maps
//! the supported options. Every other option is reported as a warning.

use rustman_core::format::Located;
use rustman_core::model::{
    Auth, Body, HttpRequest, JsonContent, Method, NameValue, Part, PartSource, REQUEST_VERSION,
    RequestSettings,
};

use super::options::{Opt, long_option, short_option};

/// What `--compressed` asks for, as curl sends it.
pub const COMPRESSED_ACCEPT_ENCODING: &str = "deflate, gzip, br, zstd";

/// A request built from a curl command, and what could not be carried over.
#[derive(Clone, Debug, PartialEq)]
pub struct Imported {
    pub request: HttpRequest,
    pub warnings: Vec<String>,
}

/// Why a curl command could not be imported.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error("curl: {0}")]
pub struct CurlError(pub String);

/// Import a curl command written as one shell string.
pub fn import_curl(command: &str) -> Result<Imported, CurlError> {
    let words = shell_words::split(command)
        .map_err(|e| CurlError(format!("cannot split the command: {e}")))?;
    import_curl_words(&words)
}

/// Import a curl command already split into words (with or without the
/// leading `curl`).
pub fn import_curl_words(words: &[String]) -> Result<Imported, CurlError> {
    let words = match words.first() {
        Some(first) if first == "curl" => &words[1..],
        _ => words,
    };
    let mut parsed = Parsed::default();
    parse(words, &mut parsed)?;
    parsed.into_imported()
}

/// The options seen, before they are turned into a request.
#[derive(Debug, Default)]
struct Parsed {
    method: Option<String>,
    url: Option<String>,
    headers: Vec<NameValue>,
    data: Vec<Data>,
    json: bool,
    parts: Vec<Part>,
    user: Option<String>,
    insecure: bool,
    location: bool,
    compressed: bool,
    get: bool,
    warnings: Vec<String>,
}

#[derive(Debug)]
enum Data {
    Text(String),
    File(String),
}

fn parse(words: &[String], parsed: &mut Parsed) -> Result<(), CurlError> {
    let mut words = words.iter().map(String::as_str);
    let mut options_done = false;
    while let Some(word) = words.next() {
        if options_done || !word.starts_with('-') || word == "-" {
            parsed.url(word);
        } else if word == "--" {
            options_done = true;
        } else if word.starts_with("--") {
            let opt = long_option(word);
            let value = take_value(opt, word, None, &mut words)?;
            parsed.apply(opt, word, value)?;
        } else {
            short_cluster(&word[1..], &mut words, parsed)?;
        }
    }
    Ok(())
}

/// `-sSL`, `-XPOST`, `-H value`: flags until one takes a value, which is the
/// rest of the word or the next word.
fn short_cluster<'a>(
    cluster: &'a str,
    words: &mut impl Iterator<Item = &'a str>,
    parsed: &mut Parsed,
) -> Result<(), CurlError> {
    for (i, flag) in cluster.char_indices() {
        let name = format!("-{flag}");
        let opt = short_option(flag);
        if opt.takes_value() {
            let rest = &cluster[i + flag.len_utf8()..];
            let attached = (!rest.is_empty()).then_some(rest);
            let value = take_value(opt, &name, attached, words)?;
            return parsed.apply(opt, &name, value);
        }
        parsed.apply(opt, &name, None)?;
    }
    Ok(())
}

fn take_value<'a>(
    opt: Opt,
    name: &str,
    attached: Option<&'a str>,
    words: &mut impl Iterator<Item = &'a str>,
) -> Result<Option<&'a str>, CurlError> {
    if !opt.takes_value() {
        return Ok(None);
    }
    attached
        .or_else(|| words.next())
        .map(Some)
        .ok_or_else(|| CurlError(format!("option {name} needs a value")))
}

impl Parsed {
    fn apply(&mut self, opt: Opt, name: &str, value: Option<&str>) -> Result<(), CurlError> {
        let value = value.unwrap_or_default();
        match opt {
            Opt::Request => self.method = Some(value.to_owned()),
            Opt::Header => self.header(value),
            Opt::Data => self.data(value),
            Opt::DataRaw => self.data.push(Data::Text(value.to_owned())),
            Opt::DataUrlencode => self.data_urlencode(value),
            Opt::Json => {
                self.json = true;
                self.data(value);
            }
            Opt::Form => self.form(value, false)?,
            Opt::FormString => self.form(value, true)?,
            Opt::User => self.user = Some(value.to_owned()),
            Opt::Cookie => self.cookie(value),
            Opt::Insecure => self.insecure = true,
            Opt::Location => self.location = true,
            Opt::Compressed => self.compressed = true,
            Opt::Get => self.get = true,
            Opt::Url => self.url(value),
            Opt::Unsupported { .. } => self
                .warnings
                .push(format!("ignored unsupported curl option {name}")),
        }
        Ok(())
    }

    fn url(&mut self, url: &str) {
        if self.url.is_some() {
            self.warnings.push(format!("ignored extra URL {url}"));
        } else {
            self.url = Some(url.to_owned());
        }
    }

    fn header(&mut self, line: &str) {
        let Some((name, value)) = line.split_once(':') else {
            self.warnings
                .push(format!("ignored header without a value: {line}"));
            return;
        };
        let value = value.trim();
        if value.is_empty() {
            self.warnings
                .push(format!("ignored header without a value: {line}"));
            return;
        }
        self.headers.push(NameValue::new(name.trim(), value));
    }

    /// `-d`, `--data-binary`, `--json`: `@file` reads a file.
    fn data(&mut self, value: &str) {
        match value.strip_prefix('@') {
            Some(path) => self.data.push(Data::File(path.to_owned())),
            None => self.data.push(Data::Text(value.to_owned())),
        }
    }

    /// `--data-urlencode [name]=content`; `[name]@file` is not supported.
    fn data_urlencode(&mut self, value: &str) {
        let equals = value.find('=');
        let at = value.find('@');
        if at.is_some_and(|at| equals.is_none_or(|eq| at < eq)) {
            self.warnings.push(format!(
                "ignored --data-urlencode {value}: reading a file is not supported"
            ));
            return;
        }
        let encode = |text: &str| form_urlencoded::byte_serialize(text.as_bytes()).collect();
        let text: String = match value.split_once('=') {
            Some(("", content)) => encode(content),
            Some((name, content)) => format!("{name}={}", encode(content)),
            None => encode(value),
        };
        self.data.push(Data::Text(text));
    }

    fn form(&mut self, value: &str, literal: bool) -> Result<(), CurlError> {
        let (name, content) = value
            .split_once('=')
            .ok_or_else(|| CurlError(format!("form field without '=': {value}")))?;
        let part = if literal {
            Part {
                name: name.to_owned(),
                source: PartSource::Text(content.to_owned()),
                content_type: None,
            }
        } else {
            form_part(name, content, &mut self.warnings)
        };
        self.parts.push(part);
        Ok(())
    }

    /// `-b 'a=1; b=2'` is a `Cookie` header; `-b file` reads a cookie file.
    fn cookie(&mut self, value: &str) {
        if value.contains('=') {
            self.headers.push(NameValue::new("Cookie", value));
        } else {
            self.warnings.push(format!(
                "ignored cookie file {value}: only inline cookies are imported"
            ));
        }
    }

    fn into_imported(mut self) -> Result<Imported, CurlError> {
        let raw_url = self
            .url
            .take()
            .ok_or_else(|| CurlError("no URL in the command".into()))?;
        let url = with_scheme(raw_url);
        if self.compressed && !self.has_header("Accept-Encoding") {
            self.headers.push(NameValue::new(
                "Accept-Encoding",
                COMPRESSED_ACCEPT_ENCODING,
            ));
        }
        let query = self.query();
        let body = self.body();
        let method = self.method_name(&body);
        let method = Method::parse(&method).map_err(CurlError)?;
        let request = HttpRequest {
            version: REQUEST_VERSION,
            kind: "http".into(),
            name: request_name(method.as_str(), &url),
            description: None,
            tags: None,
            docs: None,
            method: Some(method),
            url,
            query,
            headers: (!self.headers.is_empty()).then(|| std::mem::take(&mut self.headers)),
            body,
            auth: self.user.as_deref().map(basic_auth),
            transform: None,
            settings: self.settings(),
            variables: None,
            assertions: None,
            captures: None,
        };
        Ok(Imported {
            request,
            warnings: self.warnings,
        })
    }

    /// With `-G`, the `-d` data as query parameters (curl appends it to the
    /// URL), which leaves no body and makes the method `GET`.
    fn query(&mut self) -> Option<Vec<NameValue>> {
        if !self.get || self.data.is_empty() {
            return None;
        }
        let mut pairs = Vec::new();
        for data in std::mem::take(&mut self.data) {
            match data {
                Data::Text(text) => pairs.extend(
                    form_urlencoded::parse(text.as_bytes())
                        .map(|(name, value)| NameValue::new(name, value)),
                ),
                Data::File(path) => self.warnings.push(format!(
                    "ignored data file @{path}: -G cannot read a file into the query"
                )),
            }
        }
        (!pairs.is_empty()).then_some(pairs)
    }

    fn method_name(&self, body: &Option<Body>) -> String {
        match (&self.method, body) {
            (Some(method), _) => method.clone(),
            (None, Some(_)) => "POST".into(),
            (None, None) => "GET".into(),
        }
    }

    fn settings(&self) -> Option<RequestSettings> {
        let settings = RequestSettings {
            verify_tls: self.insecure.then_some(false),
            follow_redirects: self.location.then_some(true),
            ..RequestSettings::default()
        };
        (settings != RequestSettings::default()).then_some(settings)
    }

    fn has_header(&self, name: &str) -> bool {
        self.headers
            .iter()
            .any(|h| h.name.eq_ignore_ascii_case(name))
    }

    fn content_type(&self) -> Option<String> {
        self.headers
            .iter()
            .find(|h| h.name.eq_ignore_ascii_case("Content-Type"))
            .map(|h| h.value.to_ascii_lowercase())
    }

    /// The body, with the `Content-Type` header dropped when the body type
    /// implies it, or added when curl's default differs from Rustman's.
    fn body(&mut self) -> Option<Body> {
        if !self.parts.is_empty() {
            self.ignore_data_with_parts();
            self.drop_content_type(|ct| ct.starts_with("multipart/form-data"));
            return Some(Body::Multipart(std::mem::take(&mut self.parts)));
        }
        if self.data.is_empty() {
            return None;
        }
        if self.json {
            self.json_defaults();
        }
        if let Some(path) = self.single_file() {
            self.drop_content_type(|ct| ct == "application/octet-stream");
            if self.content_type().is_none() {
                self.headers.push(NameValue::new(
                    "Content-Type",
                    "application/x-www-form-urlencoded",
                ));
            }
            return Some(Body::File(Located::new(path)));
        }
        Some(self.text_body())
    }

    fn ignore_data_with_parts(&mut self) {
        if !self.data.is_empty() {
            self.warnings
                .push("ignored -d data: it cannot be combined with -F".into());
            self.data.clear();
        }
    }

    /// `--json` sets `Content-Type` and `Accept` unless given.
    fn json_defaults(&mut self) {
        if self.content_type().is_none() {
            self.headers
                .push(NameValue::new("Content-Type", "application/json"));
        }
        if !self.has_header("Accept") {
            self.headers
                .push(NameValue::new("Accept", "application/json"));
        }
    }

    fn single_file(&mut self) -> Option<String> {
        match self.data.as_slice() {
            [Data::File(path)] => Some(path.clone()),
            _ => None,
        }
    }

    fn text_body(&mut self) -> Body {
        let mut pieces = Vec::new();
        for data in std::mem::take(&mut self.data) {
            match data {
                Data::Text(text) => pieces.push(text),
                Data::File(path) => self.warnings.push(format!(
                    "ignored data file @{path}: it cannot be combined with other data"
                )),
            }
        }
        let text = pieces.join("&");
        let content_type = self.content_type();
        let body = text_body_for(&text, content_type.as_deref());
        match &body {
            Body::Json(_) => self.drop_content_type(|ct| ct == "application/json"),
            Body::Form(_) => {
                self.drop_content_type(|ct| ct == "application/x-www-form-urlencoded");
            }
            _ if content_type.is_none() => self.headers.push(NameValue::new(
                "Content-Type",
                "application/x-www-form-urlencoded",
            )),
            _ => self.drop_content_type(|ct| ct == "text/plain; charset=utf-8"),
        }
        body
    }

    fn drop_content_type(&mut self, implied: impl Fn(&str) -> bool) {
        self.headers.retain(|h| {
            !(h.name.eq_ignore_ascii_case("Content-Type") && implied(&h.value.to_ascii_lowercase()))
        });
    }
}

/// JSON when the content type says so and the text parses; form fields when
/// it is (or defaults to) form-encoded and looks like `a=1&b=2`; else text.
fn text_body_for(text: &str, content_type: Option<&str>) -> Body {
    let content_type = content_type.unwrap_or("application/x-www-form-urlencoded");
    if content_type.contains("json") {
        match serde_json::from_str::<serde_json::Value>(text) {
            Ok(serde_json::Value::String(_)) => {
                return Body::Json(JsonContent::Text(text.to_owned()));
            }
            Ok(value) => return Body::Json(JsonContent::Value(value)),
            Err(_) => {}
        }
    }
    if content_type == "application/x-www-form-urlencoded" && looks_like_form(text) {
        let fields = form_urlencoded::parse(text.as_bytes())
            .map(|(name, value)| NameValue::new(name, value))
            .collect();
        return Body::Form(fields);
    }
    Body::Text(text.to_owned())
}

fn looks_like_form(text: &str) -> bool {
    !text.is_empty()
        && text
            .split('&')
            .all(|pair| pair.contains('=') && !pair.starts_with('='))
}

fn basic_auth(user: &str) -> Auth {
    let (username, password) = user.split_once(':').unwrap_or((user, ""));
    Auth::Basic {
        username: username.to_owned(),
        password: password.to_owned(),
    }
}

/// The URL with `http://` added when it has no scheme, as curl does. A URL
/// that starts with a `{{variable}}` is kept: the variable (such as
/// `{{base_url}}`) brings the scheme when it is resolved.
fn with_scheme(url: String) -> String {
    if url.contains("://") || url.starts_with("{{") {
        url
    } else {
        format!("http://{url}")
    }
}

/// `POST /path`: the method and the URL path. For `{{base_url}}/path`, the
/// path after the variable.
fn request_name(method: &str, url: &str) -> String {
    let path = match url
        .strip_prefix("{{")
        .and_then(|rest| rest.split_once("}}"))
    {
        Some((_, rest)) => rest.split(['?', '#']).next().unwrap_or_default().to_owned(),
        None => url::Url::parse(url)
            .map(|u| u.path().to_owned())
            .unwrap_or_default(),
    };
    let path = if path.starts_with('/') {
        path
    } else {
        format!("/{path}")
    };
    format!("{method} {path}")
}

/// `-F name=value`, `name=@file`, `name=<file`, with `;type=` and quoting.
fn form_part(name: &str, content: &str, warnings: &mut Vec<String>) -> Part {
    let (kind, rest) = match content.chars().next() {
        Some(c @ ('@' | '<')) => (Some(c), &content[1..]),
        _ => (None, content),
    };
    let (value, params) = form_value(rest);
    let mut content_type = None;
    for param in params {
        match param.split_once('=') {
            Some((key, ct)) if key.trim() == "type" => content_type = Some(ct.trim().to_owned()),
            _ => warnings.push(format!(
                "ignored form parameter '{}' of field '{name}'",
                param.trim()
            )),
        }
    }
    if kind == Some('<') {
        warnings.push(format!(
            "form field '{name}' reads its value from a file; it is imported as a file part"
        ));
    }
    let source = match kind {
        Some(_) => PartSource::File(Located::new(value)),
        None => PartSource::Text(value),
    };
    Part {
        name: name.to_owned(),
        source,
        content_type,
    }
}

/// The value (quoted with `"` and `\` escapes, or up to the first `;`) and
/// the `;key=value` parameters after it.
fn form_value(text: &str) -> (String, Vec<&str>) {
    let (value, rest) = match text.strip_prefix('"') {
        Some(quoted) => unquote(quoted),
        None => {
            let end = text.find(';').unwrap_or(text.len());
            (text[..end].to_owned(), &text[end..])
        }
    };
    let params = rest.split(';').filter(|p| !p.trim().is_empty()).collect();
    (value, params)
}

/// The text up to the closing quote, unescaped, and what follows it.
fn unquote(quoted: &str) -> (String, &str) {
    let mut value = String::new();
    let mut chars = quoted.char_indices();
    while let Some((i, c)) = chars.next() {
        match c {
            '"' => return (value, &quoted[i + 1..]),
            '\\' => value.extend(chars.next().map(|(_, c)| c)),
            c => value.push(c),
        }
    }
    (value, "")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn form_values_unquote_and_split_parameters() {
        assert_eq!(form_value("a;type=x"), ("a".into(), vec!["type=x"]));
        assert_eq!(
            form_value(r#""a;b \"c\" \\";type=x"#),
            (r#"a;b "c" \"#.into(), vec!["type=x"])
        );
        assert_eq!(form_value("plain"), ("plain".into(), vec![]));
        assert_eq!(unquote("open"), ("open".into(), ""));
    }

    #[test]
    fn form_detection_needs_name_value_pairs() {
        assert!(looks_like_form("a=1&b="));
        assert!(!looks_like_form("hello"));
        assert!(!looks_like_form("=x"));
        assert!(!looks_like_form(""));
    }

    #[test]
    fn names_use_the_url_path() {
        assert_eq!(request_name("GET", "https://x.io/a/b?q=1"), "GET /a/b");
        assert_eq!(request_name("GET", "not a url"), "GET /");
        assert_eq!(request_name("GET", "{{base_url}}/a/b?q=1"), "GET /a/b");
        assert_eq!(request_name("GET", "{{base_url}}"), "GET /");
    }

    #[test]
    fn a_templated_url_keeps_its_variable_as_the_scheme() {
        assert_eq!(with_scheme("{{base_url}}/x".into()), "{{base_url}}/x");
        assert_eq!(with_scheme("x.io/a".into()), "http://x.io/a");
        assert_eq!(with_scheme("https://x.io".into()), "https://x.io");
    }
}
