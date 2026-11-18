//! The `http` request pane (spec: HTTP request editor tabs): method and URL,
//! Params and Headers tables with enable toggles and URL↔Params sync, the
//! body modes (none, raw JSON/XML/text, form-urlencoded, multipart), Auth
//! and Assertions. Fields the editor does not show (description, settings,
//! variables, captures, ...) are kept as loaded.

use std::fmt;
use std::path::Path;

use ratatui::Frame;
use ratatui::layout::Rect;
use rustman_core::format::Located;
use rustman_core::format::write::assertions_to_yaml;
use rustman_core::model::{
    ApiKeyLocation, Assertion, Auth, Body, HttpRequest, JsonContent, Method, NameValue, OAuth2,
    Part, PartSource, Request,
};

use crate::focus::Focus;
use crate::http_view;
use crate::keymap::chord::{Chord, Code};
use crate::kv::{KvTable, Outcome, Row};
use crate::pane::{PaneEditor, RenderCtx, RequestPane};
use crate::textarea::TextArea;
use crate::vars_view;
use crate::vim::{self, Vim};

/// The methods the method field cycles through.
pub const METHODS: [&str; 7] = ["GET", "POST", "PUT", "PATCH", "DELETE", "HEAD", "OPTIONS"];

/// The editor tabs, in order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HttpTab {
    Params,
    Headers,
    Body,
    Auth,
    Assertions,
}

impl HttpTab {
    pub const ALL: [Self; 5] = [
        Self::Params,
        Self::Headers,
        Self::Body,
        Self::Auth,
        Self::Assertions,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::Params => "Params",
            Self::Headers => "Headers",
            Self::Body => "Body",
            Self::Auth => "Auth",
            Self::Assertions => "Assertions",
        }
    }
}

/// The body modes (spec: none, raw (JSON, XML, text), form-urlencoded and
/// multipart). `File` is shown, not offered: it comes from the file only.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BodyMode {
    None,
    Json,
    Xml,
    Text,
    Form,
    Multipart,
    File,
}

impl BodyMode {
    const CYCLE: [Self; 6] = [
        Self::None,
        Self::Json,
        Self::Xml,
        Self::Text,
        Self::Form,
        Self::Multipart,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Json => "raw (JSON)",
            Self::Xml => "raw (XML)",
            Self::Text => "raw (text)",
            Self::Form => "form-urlencoded",
            Self::Multipart => "multipart",
            Self::File => "file",
        }
    }

    pub fn is_raw(self) -> bool {
        matches!(self, Self::Json | Self::Xml | Self::Text)
    }

    fn next(self) -> Self {
        let i = Self::CYCLE.iter().position(|m| *m == self).unwrap_or(0);
        Self::CYCLE[(i + 1) % Self::CYCLE.len()]
    }
}

/// The auth types the Auth tab offers. `Inherit` writes no `auth:`, so the
/// nearest collection's applies.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuthKind {
    Inherit,
    None,
    Basic,
    Bearer,
    ApiKey,
    /// Kept as loaded; edited in the file.
    OAuth2,
    /// Another type (digest, aws_sigv4, jwt), kept as loaded and edited in
    /// the file until the Auth tab has an editor for it.
    Other(&'static str),
}

impl AuthKind {
    const CYCLE: [Self; 5] = [
        Self::Inherit,
        Self::None,
        Self::Basic,
        Self::Bearer,
        Self::ApiKey,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::Inherit => "inherit",
            Self::None => "none",
            Self::Basic => "basic",
            Self::Bearer => "bearer",
            Self::ApiKey => "api_key",
            Self::OAuth2 => "oauth2",
            Self::Other(name) => name,
        }
    }

    /// Kept as loaded: the Auth tab shows it but can't edit it.
    pub fn is_kept(self) -> bool {
        matches!(self, Self::OAuth2 | Self::Other(_))
    }

    fn fields(self) -> &'static [&'static str] {
        match self {
            Self::Basic => &["username", "password"],
            Self::Bearer => &["token"],
            Self::ApiKey => &["name", "value", "in"],
            _ => &[],
        }
    }
}

/// The `http` pane.
pub struct HttpPane;

impl RequestPane for HttpPane {
    fn kind(&self) -> &'static str {
        "http"
    }

    fn open(&self, request: &Request) -> Option<Box<dyn PaneEditor>> {
        match request {
            Request::Http(http) => Some(Box::new(HttpEditor::new(http))),
            Request::Opaque(_) => None,
        }
    }
}

/// Editing state of an `http` request.
#[derive(Clone)]
pub struct HttpEditor {
    /// The request as loaded: fields the editor does not show come from it.
    base: HttpRequest,
    pub method: String,
    /// One line: the URL with its query string.
    pub url: TextArea,
    pub params: KvTable,
    pub headers: KvTable,
    pub body_mode: BodyMode,
    /// The JSON body was a string in the file (`content: '{"a":1}'`).
    json_as_text: bool,
    pub body: TextArea,
    /// Form fields or multipart parts (a value `@path` is a file part).
    pub form: KvTable,
    pub auth_kind: AuthKind,
    pub auth: KvTable,
    pub assertions: TextArea,
    pub tab: HttpTab,
}

impl fmt::Debug for HttpEditor {
    /// Field values (passwords, tokens) are left out.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("HttpEditor")
            .field("method", &self.method)
            .field("url", &self.url.text())
            .field("tab", &self.tab)
            .field("body_mode", &self.body_mode)
            .field("auth_kind", &self.auth_kind)
            .finish_non_exhaustive()
    }
}

impl HttpEditor {
    pub fn new(http: &HttpRequest) -> Self {
        let (base_url, url_query) = split_url(&http.url);
        let mut params = KvTable::from_pairs(&parse_query(url_query));
        params
            .rows
            .extend(KvTable::from_pairs(http.query.as_deref().unwrap_or_default()).rows);
        let headers = KvTable::from_pairs(http.headers.as_deref().unwrap_or_default());
        let (body_mode, body, form, json_as_text) = body_state(http.body.as_ref(), &headers);
        let (auth_kind, auth) = auth_state(http.auth.as_ref());
        let assertions = assertions_to_yaml(http.assertions.as_deref().unwrap_or_default());
        let mut editor = Self {
            base: http.clone(),
            method: http.effective_method().to_owned(),
            url: TextArea::from_text(base_url),
            params,
            headers,
            body_mode,
            json_as_text,
            body: TextArea::from_text(&body),
            form,
            auth_kind,
            auth,
            assertions: TextArea::from_text(assertions.trim_end()),
            tab: HttpTab::Params,
        };
        editor.url_from_params();
        editor
    }

    /// A new request (`Ctrl+T`).
    pub fn blank() -> Self {
        let text = "version: 1\nkind: http\nname: Untitled\nmethod: GET\nurl: ''\n";
        let Ok(Request::Http(http)) =
            rustman_core::model::KindRegistry::default().load(Path::new("new"), text)
        else {
            unreachable!("the blank request parses");
        };
        Self::new(&http)
    }

    /// The request as edited.
    pub fn to_http(&self) -> Result<HttpRequest, String> {
        let mut http = self.base.clone();
        http.method = Some(Method::parse(&self.method)?);
        let url = self.url.text();
        http.url = split_url(&url).0.to_owned();
        http.query = some_if_any(self.params.to_pairs());
        http.headers = some_if_any(self.headers.to_pairs());
        http.body = self.body()?;
        http.auth = self.auth();
        http.assertions = self.assertions()?;
        Ok(http)
    }

    fn body(&self) -> Result<Option<Body>, String> {
        let text = self.body.text();
        Ok(match self.body_mode {
            BodyMode::None => self.base.body.as_ref().map(|_| Body::None),
            BodyMode::Json => Some(Body::Json(self.json(text))),
            BodyMode::Xml | BodyMode::Text => Some(Body::Text(text)),
            BodyMode::Form => Some(Body::Form(self.form.to_pairs())),
            BodyMode::Multipart => Some(Body::Multipart(self.parts())),
            BodyMode::File => self.base.body.clone(),
        })
    }

    fn json(&self, text: String) -> JsonContent {
        if self.json_as_text {
            return JsonContent::Text(text);
        }
        serde_json::from_str(&text).map_or(JsonContent::Text(text), JsonContent::Value)
    }

    fn parts(&self) -> Vec<Part> {
        let original = match &self.base.body {
            Some(Body::Multipart(parts)) => parts.as_slice(),
            _ => &[],
        };
        self.form
            .to_pairs()
            .into_iter()
            .map(|pair| {
                let content_type = original
                    .iter()
                    .find(|p| p.name == pair.name)
                    .and_then(|p| p.content_type.clone());
                let source = match pair.value.strip_prefix('@') {
                    Some(path) => PartSource::File(Located::new(path.to_owned())),
                    None => PartSource::Text(pair.value),
                };
                Part {
                    name: pair.name,
                    source,
                    content_type,
                }
            })
            .collect()
    }

    fn auth(&self) -> Option<Auth> {
        let field = |name: &str| self.auth.value(name).to_owned();
        match self.auth_kind {
            AuthKind::Inherit => None,
            AuthKind::None => Some(Auth::None),
            AuthKind::Basic => Some(Auth::Basic {
                username: field("username"),
                password: field("password"),
            }),
            AuthKind::Bearer => Some(Auth::Bearer {
                token: field("token"),
            }),
            AuthKind::ApiKey => Some(Auth::ApiKey {
                name: field("name"),
                value: field("value"),
                location: match self.auth.value("in") {
                    "query" => Some(ApiKeyLocation::Query),
                    "header" => Some(ApiKeyLocation::Header),
                    _ => None,
                },
            }),
            AuthKind::OAuth2 | AuthKind::Other(_) => self.base.auth.clone(),
        }
    }

    fn assertions(&self) -> Result<Option<Vec<Assertion>>, String> {
        let text = self.assertions.text();
        if text.trim().is_empty() {
            return Ok(None);
        }
        rustman_core::format::from_yaml(Path::new("assertions"), &text)
            .map(Some)
            .map_err(|e| format!("Assertions: {e}"))
    }

    pub fn oauth2(&self) -> Option<&OAuth2> {
        match &self.base.auth {
            Some(Auth::OAuth2(oauth)) => Some(oauth),
            _ => None,
        }
    }

    /// The file path of a `file` body.
    pub fn body_file(&self) -> Option<&str> {
        match &self.base.body {
            Some(Body::File(path)) => Some(path.get()),
            _ => None,
        }
    }

    // ---- URL and Params sync ------------------------------------------

    /// Rewrite the URL's query from the enabled Params rows.
    fn url_from_params(&mut self) {
        let url = self.url.text();
        let base = split_url(&url).0;
        let query = join_query(&self.params.rows);
        let text = if query.is_empty() {
            base.to_owned()
        } else {
            format!("{base}?{query}")
        };
        if text != url {
            self.url = TextArea::from_text(&text);
            self.url.line_end(true);
        }
    }

    /// Rebuild the Params rows from the URL's query; disabled rows (not in
    /// the URL) are kept.
    fn params_from_url(&mut self) {
        let url = self.url.text();
        let mut rows: Vec<Row> = parse_query(split_url(&url).1)
            .iter()
            .map(|p| Row::new(&p.name, &p.value))
            .collect();
        rows.extend(self.params.rows.iter().filter(|r| !r.enabled).cloned());
        self.params.rows = rows;
        let last = self.params.rows.len().saturating_sub(1);
        self.params.selected = self.params.selected.min(last);
    }

    // ---- keys ----------------------------------------------------------

    fn method_key(&mut self, chord: Chord) -> bool {
        let i = METHODS.iter().position(|m| *m == self.method);
        let next = |by: usize| METHODS[i.map_or(0, |i| (i + by) % METHODS.len())];
        let method = match chord.code {
            _ if chord.ctrl || chord.alt => return false,
            Code::Right | Code::Enter | Code::Char(' ') => next(1),
            Code::Left => next(METHODS.len() - 1),
            Code::Char(c) => match first_after(c, i) {
                Some(method) => method,
                None => return true,
            },
            _ => return false,
        };
        self.method = method.to_owned();
        true
    }

    fn url_key(&mut self, chord: Chord) -> bool {
        match chord.code {
            // One line: Enter does nothing, Up/Down stay put.
            Code::Enter | Code::Up | Code::Down if !chord.ctrl && !chord.alt => true,
            _ if vim::type_key(&mut self.url, chord) => {
                self.params_from_url();
                true
            }
            _ => false,
        }
    }

    fn tabs_key(&mut self, chord: Chord) -> bool {
        let i = HttpTab::ALL
            .iter()
            .position(|t| *t == self.tab)
            .unwrap_or(0);
        let n = HttpTab::ALL.len();
        match chord.code {
            _ if chord.ctrl || chord.alt => return false,
            Code::Right | Code::Char('l') => self.tab = HttpTab::ALL[(i + 1) % n],
            Code::Left | Code::Char('h') => self.tab = HttpTab::ALL[(i + n - 1) % n],
            Code::Char(c @ '1'..='5') => {
                self.tab = HttpTab::ALL[usize::from(c as u8 - b'1')];
            }
            _ => return false,
        }
        true
    }

    fn content_key(&mut self, chord: Chord, vim: Option<&mut Vim>) -> bool {
        let outcome = match self.tab {
            HttpTab::Params => self.params.key(chord),
            HttpTab::Headers => self.headers.key(chord),
            HttpTab::Body => return self.body_key(chord, vim),
            HttpTab::Auth => self.auth_key(chord),
            HttpTab::Assertions => return text_key(&mut self.assertions, chord, vim),
        };
        if self.tab == HttpTab::Params && outcome == Outcome::Changed {
            self.url_from_params();
        }
        outcome != Outcome::Ignored
    }

    fn body_key(&mut self, chord: Chord, vim: Option<&mut Vim>) -> bool {
        match self.body_mode {
            mode if mode.is_raw() => text_key(&mut self.body, chord, vim),
            BodyMode::Form | BodyMode::Multipart => self.form.key(chord) != Outcome::Ignored,
            _ => false,
        }
    }

    fn auth_key(&mut self, chord: Chord) -> Outcome {
        if self.auth_kind.fields().is_empty() {
            Outcome::Ignored
        } else {
            self.auth.key(chord)
        }
    }

    fn set_body_mode(&mut self, mode: BodyMode) {
        self.body_mode = mode;
        let content_type = match mode {
            BodyMode::Xml => "application/xml",
            BodyMode::Json => "application/json",
            _ => return,
        };
        let has = self
            .headers
            .rows
            .iter()
            .any(|r| r.name.eq_ignore_ascii_case("content-type"));
        if !has && mode == BodyMode::Xml {
            self.headers.push("Content-Type", content_type);
        }
    }

    fn set_auth_kind(&mut self, kind: AuthKind) {
        let old = std::mem::take(&mut self.auth);
        let rows = kind
            .fields()
            .iter()
            .map(|name| Row::new(*name, old.value(name)))
            .collect();
        self.auth = KvTable::fixed(rows);
        self.auth_kind = kind;
    }

    fn content_cursor_var(&self) -> Option<String> {
        match self.tab {
            HttpTab::Params => table_var(&self.params),
            HttpTab::Headers => table_var(&self.headers),
            HttpTab::Body if self.body_mode.is_raw() => area_var(&self.body),
            HttpTab::Body => table_var(&self.form),
            HttpTab::Auth => table_var(&self.auth),
            HttpTab::Assertions => area_var(&self.assertions),
        }
    }
}

impl PaneEditor for HttpEditor {
    fn kind(&self) -> &str {
        "http"
    }

    fn tabs(&self) -> Vec<&'static str> {
        HttpTab::ALL.iter().map(|t| t.label()).collect()
    }

    fn tab(&self) -> usize {
        HttpTab::ALL
            .iter()
            .position(|t| *t == self.tab)
            .unwrap_or(0)
    }

    fn set_tab(&mut self, index: usize) {
        if let Some(tab) = HttpTab::ALL.get(index) {
            self.tab = *tab;
        }
    }

    fn method(&self) -> Option<&str> {
        Some(&self.method)
    }

    fn url(&self) -> Option<&TextArea> {
        Some(&self.url)
    }

    fn key(&mut self, focus: Focus, chord: Chord, vim: Option<&mut Vim>) -> bool {
        match focus {
            Focus::Method => self.method_key(chord),
            Focus::Url => self.url_key(chord),
            Focus::EditorTabs => self.tabs_key(chord),
            Focus::EditorBody => self.content_key(chord, vim),
            _ => false,
        }
    }

    fn cycle_mode(&mut self) -> bool {
        match self.tab {
            HttpTab::Body if self.body_mode != BodyMode::File => {
                self.set_body_mode(self.body_mode.next());
                true
            }
            HttpTab::Auth if !self.auth_kind.is_kept() => {
                let i = AuthKind::CYCLE.iter().position(|k| *k == self.auth_kind);
                let next = AuthKind::CYCLE[i.map_or(0, |i| (i + 1) % AuthKind::CYCLE.len())];
                self.set_auth_kind(next);
                true
            }
            _ => false,
        }
    }

    fn request(&self) -> Result<Request, String> {
        self.to_http().map(|http| Request::Http(Box::new(http)))
    }

    fn render(&self, frame: &mut Frame, rect: Rect, ctx: &RenderCtx) {
        http_view::content(frame, rect, self, ctx);
    }

    fn variable_at_cursor(&self, focus: Focus) -> Option<String> {
        match focus {
            Focus::Url => area_var(&self.url),
            Focus::EditorBody => self.content_cursor_var(),
            _ => None,
        }
    }

    fn external_text(&self) -> Option<(String, &'static str)> {
        let extension = match self.body_mode {
            BodyMode::Json => "json",
            BodyMode::Xml => "xml",
            BodyMode::Text => "txt",
            _ => return None,
        };
        Some((self.body.text(), extension))
    }

    fn set_external_text(&mut self, text: &str) {
        let text = text.strip_suffix('\n').unwrap_or(text);
        let text = text.strip_suffix('\r').unwrap_or(text);
        self.body = TextArea::from_text(&text.replace("\r\n", "\n"));
    }

    fn response_tabs(&self) -> &'static [&'static str] {
        &["Body", "Headers", "Timing", "Assertions"]
    }
}

/// Typing in a text area: modal with vim keys, plain otherwise.
fn text_key(area: &mut TextArea, chord: Chord, vim: Option<&mut Vim>) -> bool {
    match vim {
        Some(vim) => vim.key(area, chord),
        None => vim::type_key(area, chord),
    }
}

fn area_var(area: &TextArea) -> Option<String> {
    let cursor = area.cursor();
    vars_view::at(area.lines().get(cursor.row)?, cursor.col)
}

/// The variable in the selected row's value (or name), or under the cursor
/// of the cell being edited.
fn table_var(table: &KvTable) -> Option<String> {
    if let Some(edit) = &table.edit {
        return area_var(&edit.text);
    }
    let row = table.rows.get(table.selected)?;
    let in_value = vars_view::spans(&row.value).into_iter().next();
    let in_name = || vars_view::spans(&row.name).into_iter().next();
    in_value.or_else(in_name).map(|span| span.name)
}

/// The method after the one at `current` that starts with `c`.
fn first_after(c: char, current: Option<usize>) -> Option<&'static str> {
    let c = c.to_ascii_uppercase();
    let start = current.map_or(0, |i| i + 1);
    (0..METHODS.len())
        .map(|k| METHODS[(start + k) % METHODS.len()])
        .find(|m| m.starts_with(c))
}

fn some_if_any(pairs: Vec<NameValue>) -> Option<Vec<NameValue>> {
    (!pairs.is_empty()).then_some(pairs)
}

/// The URL before `?`, and the query after it (without a `#fragment`).
pub fn split_url(url: &str) -> (&str, &str) {
    match url.split_once('?') {
        Some((base, query)) => (base, query.split('#').next().unwrap_or("")),
        None => (url, ""),
    }
}

/// `a=1&b` as pairs, kept as typed (no percent-decoding: values may hold
/// `{{variables}}`).
pub fn parse_query(query: &str) -> Vec<NameValue> {
    query
        .split('&')
        .filter(|part| !part.is_empty())
        .map(|part| match part.split_once('=') {
            Some((name, value)) => NameValue::new(name, value),
            None => NameValue::new(part, ""),
        })
        .collect()
}

/// The enabled, named rows as a query string.
pub fn join_query(rows: &[Row]) -> String {
    rows.iter()
        .filter(|r| r.enabled && !r.name.is_empty())
        .map(|r| {
            if r.value.is_empty() {
                r.name.clone()
            } else {
                format!("{}={}", r.name, r.value)
            }
        })
        .collect::<Vec<_>>()
        .join("&")
}

/// The body mode and editor contents for a loaded body.
fn body_state(body: Option<&Body>, headers: &KvTable) -> (BodyMode, String, KvTable, bool) {
    let xml = headers.rows.iter().any(|r| {
        r.name.eq_ignore_ascii_case("content-type") && r.value.to_ascii_lowercase().contains("xml")
    });
    match body {
        None | Some(Body::None) => (BodyMode::None, String::new(), KvTable::default(), false),
        Some(Body::Json(JsonContent::Value(value))) => {
            let text = serde_json::to_string_pretty(value).unwrap_or_default();
            (BodyMode::Json, text, KvTable::default(), false)
        }
        Some(Body::Json(JsonContent::Text(text))) => {
            (BodyMode::Json, text.clone(), KvTable::default(), true)
        }
        Some(Body::Text(text)) => {
            let mode = if xml { BodyMode::Xml } else { BodyMode::Text };
            (mode, text.clone(), KvTable::default(), false)
        }
        Some(Body::Form(fields)) => (
            BodyMode::Form,
            String::new(),
            KvTable::from_pairs(fields),
            false,
        ),
        Some(Body::Multipart(parts)) => {
            let pairs: Vec<NameValue> = parts.iter().map(part_pair).collect();
            (
                BodyMode::Multipart,
                String::new(),
                KvTable::from_pairs(&pairs),
                false,
            )
        }
        Some(Body::File(_)) => (BodyMode::File, String::new(), KvTable::default(), false),
    }
}

fn part_pair(part: &Part) -> NameValue {
    match &part.source {
        PartSource::Text(value) => NameValue::new(&part.name, value),
        PartSource::File(path) => NameValue::new(&part.name, format!("@{}", path.get())),
    }
}

fn auth_state(auth: Option<&Auth>) -> (AuthKind, KvTable) {
    let (kind, values): (AuthKind, Vec<&str>) = match auth {
        None => (AuthKind::Inherit, vec![]),
        Some(Auth::None) => (AuthKind::None, vec![]),
        Some(Auth::Basic { username, password }) => (AuthKind::Basic, vec![username, password]),
        Some(Auth::Bearer { token }) => (AuthKind::Bearer, vec![token]),
        Some(Auth::ApiKey {
            name,
            value,
            location,
        }) => (
            AuthKind::ApiKey,
            vec![name, value, location.map_or("", |l| l.as_str())],
        ),
        Some(Auth::OAuth2(_)) => (AuthKind::OAuth2, vec![]),
        Some(Auth::Digest { .. }) => (AuthKind::Other("digest"), vec![]),
        Some(Auth::AwsSigV4(_)) => (AuthKind::Other("aws_sigv4"), vec![]),
        Some(Auth::Jwt(_)) => (AuthKind::Other("jwt"), vec![]),
    };
    let rows = kind
        .fields()
        .iter()
        .zip(values)
        .map(|(name, value)| Row::new(*name, value))
        .collect();
    (kind, KvTable::fixed(rows))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::keymap::chord::parse_chord;
    use rustman_core::format::write::request_to_yaml;
    use rustman_core::model::KindRegistry;

    fn load(text: &str) -> HttpRequest {
        let full = format!("version: 1\nkind: http\nname: R\n{text}");
        match KindRegistry::default().load(Path::new("r"), &full).unwrap() {
            Request::Http(http) => *http,
            Request::Opaque(_) => unreachable!(),
        }
    }

    fn press(editor: &mut HttpEditor, focus: Focus, keys: &str) {
        for key in keys.split(' ') {
            editor.key(focus, parse_chord(key).unwrap(), None);
        }
    }

    fn yaml(editor: &HttpEditor) -> String {
        request_to_yaml(&editor.request().unwrap())
    }

    #[test]
    fn signature_auth_types_are_kept_as_loaded() {
        let request = load(
            "method: GET\nurl: https://x.test\nauth: {type: digest, username: u, password: '{{pw}}'}\n",
        );
        let mut editor = HttpEditor::new(&request);
        assert_eq!(editor.auth_kind, AuthKind::Other("digest"));
        assert!(editor.auth_kind.is_kept());
        // Cycling the auth type is disabled, so the loaded auth survives a save.
        editor.tab = HttpTab::Auth;
        editor.cycle_mode();
        assert_eq!(editor.auth_kind, AuthKind::Other("digest"));
        let saved = yaml(&editor);
        assert!(saved.contains("type: digest"), "{saved}");
        assert!(saved.contains("{{pw}}"), "{saved}");
    }

    #[test]
    fn adding_a_params_row_updates_the_url() {
        // Scenario "Query string and Params stay in sync".
        let mut editor = HttpEditor::new(&load("url: https://api.test/users\n"));
        press(&mut editor, Focus::EditorBody, "a p a g e Tab 2 Enter");
        assert_eq!(editor.url.text(), "https://api.test/users?page=2");
        let http = editor.to_http().unwrap();
        assert_eq!(http.url, "https://api.test/users");
        assert_eq!(http.query.unwrap(), [NameValue::new("page", "2")]);
    }

    #[test]
    fn typing_a_query_in_the_url_fills_params_and_disabled_rows_leave_the_url() {
        let mut editor = HttpEditor::new(&load("url: http://x/u\n"));
        editor.url.line_end(true);
        press(&mut editor, Focus::Url, "? a = 1 & b");
        let rows: Vec<_> = editor
            .params
            .rows
            .iter()
            .map(|r| (&*r.name, &*r.value))
            .collect();
        assert_eq!(rows, [("a", "1"), ("b", "")]);
        press(&mut editor, Focus::EditorBody, "Space");
        assert_eq!(editor.url.text(), "http://x/u?b");
        press(&mut editor, Focus::Url, "Backspace");
        assert_eq!(editor.params.rows.len(), 1, "the disabled `a` is kept");
        assert!(!editor.params.rows[0].enabled);
        let http = editor.to_http().unwrap();
        assert_eq!(
            http.query.unwrap(),
            [NameValue::new("a", "1").enabled(false)]
        );
    }

    #[test]
    fn a_loaded_file_round_trips_unchanged() {
        let text = "method: POST\nurl: '{{base}}/u?x=1'\nquery:\n- name: y\n  value: '2'\n\
            headers:\n- name: X-Debug\n  value: '1'\n  enabled: false\n\
            body:\n  type: json\n  content:\n    a: 1\nauth:\n  type: bearer\n  token: '{{token}}'\n\
            assertions:\n- status: 200\n";
        let http = load(text);
        let editor = HttpEditor::new(&http);
        assert_eq!(editor.url.text(), "{{base}}/u?x=1&y=2");
        let again = HttpEditor::new(&editor.to_http().unwrap());
        assert_eq!(yaml(&again), yaml(&editor), "normalised once, then stable");
        assert!(!editor.headers.rows[0].enabled);
        assert_eq!(editor.body_mode, BodyMode::Json);
        assert_eq!(editor.auth_kind, AuthKind::Bearer);
        assert_eq!(editor.assertions.text(), "- status: 200");
    }

    #[test]
    fn method_cycles_and_jumps_by_letter() {
        let mut editor = HttpEditor::new(&load("url: x\n"));
        press(&mut editor, Focus::Method, "Right");
        assert_eq!(editor.method, "POST");
        press(&mut editor, Focus::Method, "p");
        assert_eq!(editor.method, "PUT");
        press(&mut editor, Focus::Method, "p p d Left");
        assert_eq!(editor.method, "PATCH");
    }

    #[test]
    fn body_modes_cycle_and_xml_sets_a_content_type() {
        let mut editor = HttpEditor::new(&load("url: x\n"));
        editor.tab = HttpTab::Body;
        assert!(editor.cycle_mode());
        assert_eq!(editor.body_mode, BodyMode::Json);
        press(&mut editor, Focus::EditorBody, "{ }");
        assert!(editor.cycle_mode());
        assert_eq!(editor.body_mode, BodyMode::Xml);
        assert_eq!(editor.headers.value("Content-Type"), "application/xml");
        assert_eq!(editor.external_text(), Some(("{}".into(), "xml")));
        editor.cycle_mode();
        editor.cycle_mode();
        assert_eq!(editor.body_mode, BodyMode::Form);
        press(&mut editor, Focus::EditorBody, "a k Tab v Enter");
        editor.cycle_mode();
        assert_eq!(editor.body_mode, BodyMode::Multipart);
        press(
            &mut editor,
            Focus::EditorBody,
            "a f Tab @ d a t a . b i n Enter",
        );
        let Some(Body::Multipart(parts)) = editor.to_http().unwrap().body else {
            panic!("multipart");
        };
        assert_eq!(parts[0].source, PartSource::Text("v".into()));
        assert_eq!(
            parts[1].source,
            PartSource::File(Located::new("data.bin".into()))
        );
        editor.cycle_mode();
        assert_eq!(editor.body_mode, BodyMode::None);
        assert_eq!(
            editor.to_http().unwrap().body,
            None,
            "no body before, none now"
        );
    }

    #[test]
    fn auth_types_cycle_and_keep_matching_fields() {
        let mut editor = HttpEditor::new(&load(
            "url: x\nauth:\n  type: basic\n  username: u\n  password: p\n",
        ));
        editor.tab = HttpTab::Auth;
        press(&mut editor, Focus::EditorBody, "Down Enter 2 Enter");
        assert_eq!(editor.auth.value("password"), "p2");
        editor.cycle_mode();
        assert_eq!(editor.auth_kind, AuthKind::Bearer);
        editor.cycle_mode();
        press(
            &mut editor,
            Focus::EditorBody,
            "Enter K Enter Down Down Enter q u e r y Enter",
        );
        let Some(Auth::ApiKey { name, location, .. }) = editor.to_http().unwrap().auth else {
            panic!("api key");
        };
        assert_eq!(
            (name.as_str(), location),
            ("K", Some(ApiKeyLocation::Query))
        );
        editor.cycle_mode();
        assert_eq!(
            editor.to_http().unwrap().auth,
            None,
            "inherit writes no auth"
        );
        assert!(!format!("{editor:?}").contains("p2"), "Debug hides values");
    }

    #[test]
    fn invalid_assertions_are_reported() {
        let mut editor = HttpEditor::new(&load("url: x\n"));
        editor.tab = HttpTab::Assertions;
        press(
            &mut editor,
            Focus::EditorBody,
            "- Space s t a t u s : Space x",
        );
        let err = editor.request().unwrap_err();
        assert!(err.starts_with("Assertions:"), "{err}");
    }

    #[test]
    fn the_variable_under_the_cursor() {
        let mut editor = HttpEditor::new(&load(
            "url: '{{base}}/u'\nheaders:\n- name: A\n  value: Bearer {{token}}\n",
        ));
        editor.url.line_start();
        assert_eq!(
            editor.variable_at_cursor(Focus::Url).as_deref(),
            Some("base")
        );
        editor.tab = HttpTab::Headers;
        assert_eq!(
            editor.variable_at_cursor(Focus::EditorBody).as_deref(),
            Some("token")
        );
        assert_eq!(editor.variable_at_cursor(Focus::Method), None);
    }

    #[test]
    fn editor_tabs_switch_with_arrows_and_digits() {
        let mut editor = HttpEditor::blank();
        press(&mut editor, Focus::EditorTabs, "Left");
        assert_eq!(editor.tab, HttpTab::Assertions);
        press(&mut editor, Focus::EditorTabs, "3");
        assert_eq!(editor.tab, HttpTab::Body);
        assert_eq!(PaneEditor::tab(&editor), 2);
    }
}
