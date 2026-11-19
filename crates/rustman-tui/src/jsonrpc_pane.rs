//! The `jsonrpc` request pane (add-jsonrpc-client "TUI JSON-RPC pane"):
//! the endpoint on the URL row, and the tabs Call (the method, completed
//! from the endpoint's OpenRPC catalog, and the params as JSON), Batch,
//! Headers, Auth and Assertions (each that part of the file as YAML).
//!
//! An unedited request keeps its file text. Once edited, the file is
//! rebuilt from the loaded document with the edited parts in place, in
//! schema order, and parsed again, so every load-time check applies.

use std::path::Path;
use std::sync::Arc;

use ratatui::Frame;
use ratatui::layout::Rect;
use rustman_core::format::from_yaml;
use rustman_core::format::write::{Node, json_node, render};
use rustman_core::model::{JsonrpcRequest, Request};
use rustman_jsonrpc::Catalog;
use serde_json::{Map, Value};

use crate::focus::Focus;
use crate::jsonrpc_view;
use crate::keymap::chord::{Chord, Code};
use crate::pane::{PaneEditor, RenderCtx, RequestPane};
use crate::textarea::TextArea;
use crate::vars_view;
use crate::vim::{self, Vim};

/// The editor tabs, in order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RpcTab {
    Call,
    Batch,
    Headers,
    Auth,
    Assertions,
}

impl RpcTab {
    pub const ALL: [Self; 5] = [
        Self::Call,
        Self::Batch,
        Self::Headers,
        Self::Auth,
        Self::Assertions,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::Call => "Call",
            Self::Batch => "Batch",
            Self::Headers => "Headers",
            Self::Auth => "Auth",
            Self::Assertions => "Assertions",
        }
    }

    /// The file member a YAML tab edits.
    fn key(self) -> Option<&'static str> {
        match self {
            Self::Call => None,
            Self::Batch => Some("batch"),
            Self::Headers => Some("headers"),
            Self::Auth => Some("auth"),
            Self::Assertions => Some("assertions"),
        }
    }
}

/// Which field of the Call tab has the cursor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CallField {
    Method,
    Params,
}

/// Where the endpoint's OpenRPC catalog stands.
#[derive(Debug, Clone)]
pub enum CatalogState {
    /// Not looked for yet: the next update looks.
    Unloaded,
    /// Looked for; `None` when the endpoint has no catalog.
    Ready(Option<Arc<Catalog>>),
    /// The catalog file is unreadable or invalid (redacted reason).
    Failed(String),
}

/// One method the popup offers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MethodItem {
    pub name: String,
    /// `address, block?`: the param names, optional ones with `?`.
    pub params: String,
}

/// The method completion popup.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MethodPopup {
    pub items: Vec<MethodItem>,
    pub selected: usize,
}

/// The `jsonrpc` pane.
pub struct JsonrpcPane;

impl RequestPane for JsonrpcPane {
    fn kind(&self) -> &'static str {
        "jsonrpc"
    }

    fn open(&self, request: &Request) -> Option<Box<dyn PaneEditor>> {
        match request {
            Request::Jsonrpc(jsonrpc) => Some(Box::new(JsonrpcEditor::new(jsonrpc))),
            _ => None,
        }
    }
}

/// The text of every editable part, to tell an edit from the loaded file.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Texts([String; 7]);

/// Editing state of a `jsonrpc` request.
#[derive(Clone)]
pub struct JsonrpcEditor {
    base: JsonrpcRequest,
    /// The loaded file as a document, for the members the pane leaves alone.
    doc: Map<String, Value>,
    loaded: Texts,
    pub endpoint: TextArea,
    pub method: TextArea,
    pub params: TextArea,
    pub batch: TextArea,
    pub headers: TextArea,
    pub auth: TextArea,
    pub assertions: TextArea,
    pub tab: RpcTab,
    pub field: CallField,
    pub catalog: CatalogState,
    pub popup: Option<MethodPopup>,
}

impl std::fmt::Debug for JsonrpcEditor {
    /// Params, headers and auth (which may hold tokens) are left out.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("JsonrpcEditor")
            .field("endpoint", &self.endpoint.text())
            .field("method", &self.method.text())
            .field("tab", &self.tab)
            .finish_non_exhaustive()
    }
}

impl JsonrpcEditor {
    pub fn new(jsonrpc: &JsonrpcRequest) -> Self {
        let doc = match from_yaml::<Value>(Path::new("request"), &jsonrpc.source) {
            Ok(Value::Object(map)) => map,
            _ => Map::new(),
        };
        let params = doc
            .get("params")
            .and_then(|p| serde_json::to_string_pretty(p).ok())
            .unwrap_or_default();
        let yaml = |key: &str| doc.get(key).map(yaml_text).unwrap_or_default();
        let mut editor = Self {
            base: jsonrpc.clone(),
            loaded: Texts(Default::default()),
            endpoint: TextArea::from_text(&jsonrpc.endpoint),
            method: TextArea::from_text(jsonrpc.method.as_deref().unwrap_or_default()),
            params: TextArea::from_text(&params),
            batch: TextArea::from_text(&yaml("batch")),
            headers: TextArea::from_text(&yaml("headers")),
            auth: TextArea::from_text(&yaml("auth")),
            assertions: TextArea::from_text(&yaml("assertions")),
            doc,
            tab: RpcTab::Call,
            field: CallField::Method,
            catalog: CatalogState::Unloaded,
            popup: None,
        };
        editor.loaded = editor.texts();
        editor
    }

    /// The request file as loaded.
    pub fn base(&self) -> &JsonrpcRequest {
        &self.base
    }

    /// The subscription's subscribe method, when the file has one.
    pub fn subscribe_method(&self) -> Option<&str> {
        self.base
            .subscription
            .as_ref()
            .map(|s| s.subscribe.as_str())
    }

    /// The loaded catalog, if any.
    pub fn loaded_catalog(&self) -> Option<&Catalog> {
        match &self.catalog {
            CatalogState::Ready(Some(catalog)) => Some(catalog),
            _ => None,
        }
    }

    fn texts(&self) -> Texts {
        Texts(
            [
                &self.endpoint,
                &self.method,
                &self.params,
                &self.batch,
                &self.headers,
                &self.auth,
                &self.assertions,
            ]
            .map(TextArea::text),
        )
    }

    /// Why the params do not parse; `None` when they do (or are empty).
    pub fn params_error(&self) -> Option<String> {
        params_value(&self.params.text()).err()
    }

    /// The request as edited.
    pub fn to_jsonrpc(&self) -> Result<JsonrpcRequest, String> {
        if self.texts() == self.loaded {
            return Ok(self.base.clone());
        }
        let mut doc = self.doc.clone();
        put(
            &mut doc,
            "endpoint",
            Some(Value::String(self.endpoint.text())),
        );
        let method = self.method.text();
        put(
            &mut doc,
            "method",
            (!method.trim().is_empty()).then(|| Value::String(method.trim().to_owned())),
        );
        put(&mut doc, "params", params_value(&self.params.text())?);
        for tab in &RpcTab::ALL[1..] {
            let key = tab.key().unwrap_or_default();
            put(
                &mut doc,
                key,
                yaml_value(tab.label(), &self.area(*tab).text())?,
            );
        }
        let text = render(&ordered(&doc));
        JsonrpcRequest::parse(Path::new("request"), &text).map_err(|e| e.to_string())
    }

    fn area(&self, tab: RpcTab) -> &TextArea {
        match tab {
            RpcTab::Call => &self.params,
            RpcTab::Batch => &self.batch,
            RpcTab::Headers => &self.headers,
            RpcTab::Auth => &self.auth,
            RpcTab::Assertions => &self.assertions,
        }
    }

    fn area_mut(&mut self, tab: RpcTab) -> &mut TextArea {
        match tab {
            RpcTab::Call => &mut self.params,
            RpcTab::Batch => &mut self.batch,
            RpcTab::Headers => &mut self.headers,
            RpcTab::Auth => &mut self.auth,
            RpcTab::Assertions => &mut self.assertions,
        }
    }

    // ---- completion ---------------------------------------------------

    /// Open (or refresh) the popup with the catalog methods starting with
    /// the method typed so far; false when there is none to offer.
    pub fn open_completion(&mut self) -> bool {
        self.tab = RpcTab::Call;
        self.field = CallField::Method;
        self.popup = self
            .candidates()
            .map(|items| MethodPopup { items, selected: 0 });
        self.popup.is_some()
    }

    fn candidates(&self) -> Option<Vec<MethodItem>> {
        let catalog = self.loaded_catalog()?;
        let typed = self.method.text();
        let items: Vec<MethodItem> = catalog
            .completions(typed.trim())
            .into_iter()
            .map(|m| MethodItem {
                name: m.name.clone(),
                params: m
                    .params
                    .iter()
                    .map(|p| format!("{}{}", p.name, if p.required { "" } else { "?" }))
                    .collect::<Vec<_>>()
                    .join(", "),
            })
            .collect();
        (!items.is_empty()).then_some(items)
    }

    fn popup_key(&mut self, chord: Chord) -> bool {
        let Some(popup) = &mut self.popup else {
            return false;
        };
        let n = popup.items.len();
        match chord.code {
            Code::Up if !chord.ctrl => popup.selected = (popup.selected + n - 1) % n,
            Code::Down if !chord.ctrl => popup.selected = (popup.selected + 1) % n,
            Code::Enter | Code::Tab => self.accept(),
            Code::Esc => self.popup = None,
            _ => return self.method_key(chord),
        }
        true
    }

    /// Put the selected method in the method field.
    fn accept(&mut self) {
        let Some(popup) = self.popup.take() else {
            return;
        };
        if let Some(item) = popup.items.get(popup.selected) {
            self.method = TextArea::from_text(&item.name);
            self.method.line_end(true);
        }
    }

    // ---- keys -----------------------------------------------------------

    fn endpoint_key(&mut self, chord: Chord) -> bool {
        let changed = match chord.code {
            Code::Enter | Code::Up | Code::Down if !chord.ctrl && !chord.alt => return true,
            _ => vim::type_key(&mut self.endpoint, chord),
        };
        if changed {
            self.catalog = CatalogState::Unloaded;
            self.popup = None;
        }
        changed
    }

    fn tabs_key(&mut self, chord: Chord) -> bool {
        let i = RpcTab::ALL.iter().position(|t| *t == self.tab).unwrap_or(0);
        let n = RpcTab::ALL.len();
        match chord.code {
            _ if chord.ctrl || chord.alt => return false,
            Code::Right | Code::Char('l') => self.tab = RpcTab::ALL[(i + 1) % n],
            Code::Left | Code::Char('h') => self.tab = RpcTab::ALL[(i + n - 1) % n],
            Code::Char(c @ '1'..='5') => self.tab = RpcTab::ALL[usize::from(c as u8 - b'1')],
            _ => return false,
        }
        true
    }

    fn content_key(&mut self, chord: Chord, vim: Option<&mut Vim>) -> bool {
        match (self.tab, self.field) {
            (RpcTab::Call, _) if self.base.batch.is_some() => false,
            (RpcTab::Call, CallField::Method) if self.popup.is_some() => self.popup_key(chord),
            (RpcTab::Call, CallField::Method) => self.method_key(chord),
            (RpcTab::Call, CallField::Params) => self.params_key(chord, vim),
            (tab, _) => text_key(self.area_mut(tab), chord, vim),
        }
    }

    /// The one-line method field: `Enter` / `Down` move to the params;
    /// typing narrows the completion popup.
    fn method_key(&mut self, chord: Chord) -> bool {
        if !chord.ctrl && !chord.alt && matches!(chord.code, Code::Enter | Code::Down) {
            self.popup = None;
            self.field = CallField::Params;
            return true;
        }
        if matches!(chord.code, Code::Up) {
            return false;
        }
        let typed = vim::type_key(&mut self.method, chord);
        if typed {
            self.popup = self
                .candidates()
                .map(|items| MethodPopup { items, selected: 0 });
        }
        typed
    }

    /// `Up` on the first params row goes back to the method.
    fn params_key(&mut self, chord: Chord, vim: Option<&mut Vim>) -> bool {
        let first_row = self.params.cursor().row == 0;
        if first_row && chord.code == Code::Up && !chord.ctrl && !chord.alt {
            self.field = CallField::Method;
            return true;
        }
        text_key(&mut self.params, chord, vim)
    }
}

impl PaneEditor for JsonrpcEditor {
    fn kind(&self) -> &str {
        "jsonrpc"
    }

    fn tabs(&self) -> Vec<&'static str> {
        RpcTab::ALL.iter().map(|t| t.label()).collect()
    }

    fn tab(&self) -> usize {
        RpcTab::ALL.iter().position(|t| *t == self.tab).unwrap_or(0)
    }

    fn set_tab(&mut self, index: usize) {
        if let Some(tab) = RpcTab::ALL.get(index) {
            self.tab = *tab;
        }
    }

    fn method(&self) -> Option<&str> {
        Some(match self.base.subscription {
            Some(_) => "SUB",
            None => "RPC",
        })
    }

    fn url(&self) -> Option<&TextArea> {
        Some(&self.endpoint)
    }

    fn key(&mut self, focus: Focus, chord: Chord, vim: Option<&mut Vim>) -> bool {
        match focus {
            Focus::Url => self.endpoint_key(chord),
            Focus::EditorTabs => self.tabs_key(chord),
            Focus::EditorBody => self.content_key(chord, vim),
            _ => false,
        }
    }

    fn cycle_mode(&mut self) -> bool {
        false
    }

    fn request(&self) -> Result<Request, String> {
        self.to_jsonrpc().map(|r| Request::Jsonrpc(Box::new(r)))
    }

    fn render(&self, frame: &mut Frame, rect: Rect, ctx: &RenderCtx) {
        jsonrpc_view::content(frame, rect, self, ctx);
    }

    fn variable_at_cursor(&self, focus: Focus) -> Option<String> {
        let area = match (focus, self.tab, self.field) {
            (Focus::Url, _, _) => &self.endpoint,
            (Focus::EditorBody, RpcTab::Call, CallField::Method) => return None,
            (Focus::EditorBody, tab, _) => self.area(tab),
            _ => return None,
        };
        let cursor = area.cursor();
        vars_view::at(area.lines().get(cursor.row)?, cursor.col)
    }

    fn external_text(&self) -> Option<(String, &'static str)> {
        match self.tab {
            RpcTab::Call if self.base.batch.is_some() => None,
            RpcTab::Call => Some((self.params.text(), "json")),
            tab => Some((self.area(tab).text(), "yaml")),
        }
    }

    fn set_external_text(&mut self, text: &str) {
        let text = text.strip_suffix('\n').unwrap_or(text);
        let text = text.strip_suffix('\r').unwrap_or(text);
        *self.area_mut(self.tab) = TextArea::from_text(&text.replace("\r\n", "\n"));
    }

    fn response_tabs(&self) -> &'static [&'static str] {
        &["Result", "Error", "Events"]
    }

    fn jsonrpc(&self) -> Option<&JsonrpcEditor> {
        Some(self)
    }

    fn jsonrpc_mut(&mut self) -> Option<&mut JsonrpcEditor> {
        Some(self)
    }
}

/// Typing in a text area: modal with vim keys, plain otherwise.
fn text_key(area: &mut TextArea, chord: Chord, vim: Option<&mut Vim>) -> bool {
    match vim {
        Some(vim) => vim.key(area, chord),
        None => vim::type_key(area, chord),
    }
}

/// `value` as the YAML the tabs show, without the trailing newline.
fn yaml_text(value: &Value) -> String {
    render(&json_node(value)).trim_end().to_owned()
}

/// The params as typed: a JSON array or object, `None` when empty.
fn params_value(text: &str) -> Result<Option<Value>, String> {
    if text.trim().is_empty() {
        return Ok(None);
    }
    match serde_json::from_str::<Value>(text) {
        Ok(value @ (Value::Array(_) | Value::Object(_))) => Ok(Some(value)),
        Ok(_) => Err("Params: params must be a list or a map".to_owned()),
        Err(e) => Err(format!("Params: {e}")),
    }
}

/// A YAML tab as typed, `None` when empty.
fn yaml_value(label: &str, text: &str) -> Result<Option<Value>, String> {
    if text.trim().is_empty() {
        return Ok(None);
    }
    from_yaml::<Value>(Path::new(label), text)
        .map(Some)
        .map_err(|e| format!("{label}: {e}"))
}

fn put(doc: &mut Map<String, Value>, key: &str, value: Option<Value>) {
    match value {
        Some(value) => {
            doc.insert(key.to_owned(), value);
        }
        None => {
            doc.remove(key);
        }
    }
}

/// The members of a jsonrpc file in schema order; unknown ones last.
const ORDER: [&str; 19] = [
    "version",
    "kind",
    "name",
    "description",
    "tags",
    "endpoint",
    "method",
    "params",
    "id",
    "notification",
    "batch",
    "subscription",
    "idempotent",
    "headers",
    "auth",
    "settings",
    "variables",
    "assertions",
    "captures",
];

/// `doc` as a YAML node with its members in [`ORDER`].
fn ordered(doc: &Map<String, Value>) -> Node {
    let rank = |key: &str| ORDER.iter().position(|k| *k == key).unwrap_or(ORDER.len());
    let mut members: Vec<(&String, &Value)> = doc.iter().collect();
    members.sort_by_key(|(key, _)| rank(key));
    Node::Map(
        members
            .into_iter()
            .map(|(key, value)| (key.clone(), json_node(value)))
            .collect(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::keymap::chord::parse_chord;
    use rustman_core::model::KindRegistry;
    use serde_json::json;

    fn request(text: &str) -> JsonrpcRequest {
        let full = format!("version: 1\nkind: jsonrpc\nname: R\nendpoint: http://x/\n{text}");
        match KindRegistry::default().load(Path::new("r.rman.yaml"), &full) {
            Ok(Request::Jsonrpc(jsonrpc)) => *jsonrpc,
            other => panic!("not a jsonrpc request: {other:?}"),
        }
    }

    fn keys(editor: &mut JsonrpcEditor, focus: Focus, keys: &str) {
        for key in keys.split(' ') {
            editor.key(focus, parse_chord(key).unwrap(), None);
        }
    }

    fn type_text(editor: &mut JsonrpcEditor, text: &str) {
        for c in text.chars() {
            editor.key(Focus::EditorBody, Chord::plain(Code::Char(c)), None);
        }
    }

    #[test]
    fn an_unedited_request_keeps_its_file_text() {
        let base = request("method: eth_call\nparams: [{to: '0x1'}, latest]\n# kept\n");
        let editor = JsonrpcEditor::new(&base);
        assert_eq!(
            editor.tabs(),
            ["Call", "Batch", "Headers", "Auth", "Assertions"]
        );
        assert_eq!(editor.method.text(), "eth_call");
        assert_eq!(editor.to_jsonrpc().unwrap(), base);
        assert_eq!(editor.method(), Some("RPC"));
    }

    #[test]
    fn edits_are_written_in_schema_order_and_checked() {
        let base = request(
            "assertions:\n  - rpc_error: none\nmethod: eth_blockNumber\nheaders:\n  - name: A\n    value: b\n",
        );
        let mut editor = JsonrpcEditor::new(&base);
        editor.field = CallField::Params;
        type_text(&mut editor, "[\"0x10\"]");
        let edited = editor.to_jsonrpc().unwrap();
        assert_eq!(*edited.params.as_ref().unwrap().get(), json!(["0x10"]));
        let text = &edited.source;
        let at = |key: &str| text.find(key).unwrap_or_else(|| panic!("{key} in {text}"));
        assert!(at("method:") < at("params:") && at("params:") < at("headers:"));
        assert!(at("headers:") < at("assertions:"), "{text}");
        editor.params = TextArea::from_text("5");
        assert_eq!(
            editor.to_jsonrpc().unwrap_err(),
            "Params: params must be a list or a map"
        );
        editor.params = TextArea::from_text("");
        editor.method = TextArea::from_text("");
        assert!(
            editor
                .to_jsonrpc()
                .unwrap_err()
                .contains("jsonrpc request requires 'method'")
        );
    }

    #[test]
    fn yaml_tabs_edit_their_member() {
        let base = request("method: m\n");
        let mut editor = JsonrpcEditor::new(&base);
        editor.tab = RpcTab::Assertions;
        editor.assertions = TextArea::from_text("- rpc_error: {code: -32602}");
        let edited = editor.to_jsonrpc().unwrap();
        assert_eq!(edited.assertions.as_ref().unwrap().len(), 1);
        editor.headers = TextArea::from_text("- name: [");
        assert!(editor.to_jsonrpc().unwrap_err().starts_with("Headers: "));
        assert_eq!(editor.external_text().unwrap().1, "yaml");
    }

    #[test]
    fn the_catalog_completes_the_method_with_param_names() {
        let base = request("method: eth\n");
        let mut editor = JsonrpcEditor::new(&base);
        let doc = json!({"methods": [
            {"name": "eth_getBalance", "params": [
                {"name": "address", "required": true}, {"name": "block"}]},
            {"name": "eth_getCode", "params": [{"name": "address", "required": true}]},
            {"name": "net_version", "params": []}
        ]});
        editor.catalog = CatalogState::Ready(Some(Arc::new(Catalog::parse(&doc).unwrap())));
        keys(&mut editor, Focus::EditorBody, "End");
        type_text(&mut editor, "_get");
        let popup = editor.popup.clone().unwrap();
        assert_eq!(popup.items.len(), 2);
        assert_eq!(popup.items[0].params, "address, block?");
        keys(&mut editor, Focus::EditorBody, "Down Enter");
        assert_eq!(editor.method.text(), "eth_getCode");
        assert!(editor.popup.is_none());
        keys(&mut editor, Focus::EditorBody, "Down");
        assert_eq!(editor.field, CallField::Params);
        keys(&mut editor, Focus::EditorBody, "Up");
        assert_eq!(editor.field, CallField::Method);
    }

    #[test]
    fn a_batch_file_is_edited_in_the_batch_tab() {
        let base = request("batch:\n  - method: a\n  - method: b\n    params: [1]\n");
        let mut editor = JsonrpcEditor::new(&base);
        assert!(!editor.key(Focus::EditorBody, parse_chord("x").unwrap(), None));
        assert_eq!(
            editor.batch.text(),
            "- method: a\n- method: b\n  params:\n    - 1"
        );
        keys(&mut editor, Focus::EditorTabs, "2");
        assert_eq!(editor.tab, RpcTab::Batch);
        editor.batch = TextArea::from_text("- method: a");
        let edited = editor.to_jsonrpc().unwrap();
        assert_eq!(edited.batch.as_ref().unwrap().len(), 1);
    }
}
