//! The `grpc` request pane (add-grpc-client "TUI gRPC pane"): the target
//! on the URL row, and the tabs Method (the method browser over the
//! target's schema), Message (the request message as JSON, seeded from the
//! method's template), Metadata and Assertions (YAML).
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
use rustman_core::model::{GrpcRequest, Request};
use rustman_grpc::{CallType, MethodInfo, Schema, template};
use serde_json::{Map, Value};

use crate::focus::Focus;
use crate::grpc_view;
use crate::keymap::chord::{Chord, Code};
use crate::pane::{PaneEditor, RenderCtx, RequestPane};
use crate::textarea::TextArea;
use crate::vars_view;
use crate::vim::{self, Vim};

/// The editor tabs, in order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GrpcTab {
    Method,
    Message,
    Metadata,
    Assertions,
}

impl GrpcTab {
    pub const ALL: [Self; 4] = [
        Self::Method,
        Self::Message,
        Self::Metadata,
        Self::Assertions,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::Method => "Method",
            Self::Message => "Message",
            Self::Metadata => "Metadata",
            Self::Assertions => "Assertions",
        }
    }
}

/// A target's schema, as the method browser uses it.
#[derive(Debug)]
pub struct LoadedSchema {
    pub schema: Schema,
    /// Every method of every service, in order.
    pub methods: Vec<MethodInfo>,
    /// Where it came from: `reflection`, `.proto files`, or the cache with
    /// its fetch time.
    pub origin: String,
    /// Read from the descriptor cache: it may be stale.
    pub stale: bool,
}

impl LoadedSchema {
    pub fn new(schema: Schema) -> Self {
        let methods = schema
            .services()
            .into_iter()
            .flat_map(|s| s.methods)
            .collect();
        let (origin, stale) = match &schema.origin {
            rustman_grpc::SchemaOrigin::Reflection => ("reflection".to_owned(), false),
            rustman_grpc::SchemaOrigin::Cache { fetched_at } => {
                (format!("descriptor cache, fetched {fetched_at}"), true)
            }
            rustman_grpc::SchemaOrigin::Protos => (".proto files".to_owned(), false),
            rustman_grpc::SchemaOrigin::DescriptorSet => ("descriptor set".to_owned(), false),
        };
        Self {
            schema,
            methods,
            origin,
            stale,
        }
    }

    /// The template of `method`'s request message, as the Message tab
    /// holds it: a list of one for client-streaming and bidi methods.
    pub fn template(&self, method: &str) -> Option<Value> {
        let descriptor = self.schema.method(method).ok()?;
        let one = template(&descriptor.input());
        Some(match CallType::of(&descriptor) {
            CallType::ClientStreaming | CallType::Bidi => Value::Array(vec![one]),
            _ => one,
        })
    }

    /// Why `message` does not fit `method`'s request type, if it does not.
    pub fn check(&self, method: &str, message: &Value) -> Result<(), String> {
        let descriptor = self.schema.method(method).map_err(|e| e.to_string())?;
        rustman_grpc::to_message(&descriptor.input(), message)
            .map(drop)
            .map_err(|e| e.to_string())
    }
}

/// Where the target's schema stands.
#[derive(Debug, Clone, Default)]
pub enum SchemaState {
    /// Not loaded yet: the next update starts a load.
    #[default]
    Unloaded,
    Loading,
    Ready(Arc<LoadedSchema>),
    /// The schema could not be loaded (redacted reason).
    Failed(String),
}

/// The `grpc` pane.
pub struct GrpcPane;

impl RequestPane for GrpcPane {
    fn kind(&self) -> &'static str {
        "grpc"
    }

    fn open(&self, request: &Request) -> Option<Box<dyn PaneEditor>> {
        match request {
            Request::Grpc(grpc) => Some(Box::new(GrpcEditor::new(grpc))),
            _ => None,
        }
    }
}

/// The text of every editable part, to tell an edit from the loaded file.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Texts([String; 5]);

/// Editing state of a `grpc` request.
#[derive(Clone)]
pub struct GrpcEditor {
    base: GrpcRequest,
    /// The loaded file as a document, for the members the pane leaves alone.
    doc: Map<String, Value>,
    loaded: Texts,
    pub target: TextArea,
    /// `package.Service/Method`, picked in the browser.
    pub method: String,
    pub message: TextArea,
    pub metadata: TextArea,
    pub assertions: TextArea,
    pub tab: GrpcTab,
    pub schema: SchemaState,
    /// The browser row under the cursor.
    pub selected: usize,
    /// The template last put in the Message tab, to tell it from typing.
    seeded: Option<String>,
}

impl std::fmt::Debug for GrpcEditor {
    /// The message and metadata (which may hold tokens) are left out.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GrpcEditor")
            .field("target", &self.target.text())
            .field("method", &self.method)
            .field("tab", &self.tab)
            .finish_non_exhaustive()
    }
}

impl GrpcEditor {
    pub fn new(grpc: &GrpcRequest) -> Self {
        let doc = match from_yaml::<Value>(Path::new("request"), &grpc.source) {
            Ok(Value::Object(map)) => map,
            _ => Map::new(),
        };
        let message = doc
            .get("message")
            .or_else(|| doc.get("messages"))
            .and_then(|m| serde_json::to_string_pretty(m).ok())
            .unwrap_or_default();
        let yaml = |key: &str| doc.get(key).map(yaml_text).unwrap_or_default();
        let mut editor = Self {
            base: grpc.clone(),
            loaded: Texts(Default::default()),
            target: TextArea::from_text(&grpc.target),
            method: grpc.method.clone(),
            message: TextArea::from_text(&message),
            metadata: TextArea::from_text(&yaml("metadata")),
            assertions: TextArea::from_text(&yaml("assertions")),
            doc,
            tab: GrpcTab::Method,
            schema: SchemaState::Unloaded,
            selected: 0,
            seeded: None,
        };
        editor.loaded = editor.texts();
        editor
    }

    /// The request file as loaded.
    pub fn base(&self) -> &GrpcRequest {
        &self.base
    }

    /// The loaded schema, if any.
    pub fn loaded(&self) -> Option<&LoadedSchema> {
        match &self.schema {
            SchemaState::Ready(loaded) => Some(loaded),
            _ => None,
        }
    }

    /// The picked method's call type, when the schema knows it.
    pub fn call_type(&self) -> Option<CallType> {
        let loaded = self.loaded()?;
        let method = loaded.methods.iter().find(|m| m.path == self.method)?;
        Some(method.call_type)
    }

    /// Whether the picked method is bidi: its stream stays open for typed
    /// messages. Without a schema, a file listing `messages` counts.
    pub fn is_bidi(&self) -> bool {
        match self.call_type() {
            Some(call_type) => call_type == CallType::Bidi,
            None => self.base.has_message_list(),
        }
    }

    /// A schema load finished: show it, select the picked method and seed
    /// an empty Message tab from its template.
    pub fn schema_loaded(&mut self, state: SchemaState) {
        self.schema = state;
        let Some(loaded) = self.loaded() else {
            return;
        };
        if let Some(i) = loaded.methods.iter().position(|m| m.path == self.method) {
            self.selected = i;
        }
        if self.message.text().trim().is_empty() {
            self.seed();
        }
    }

    /// Put the picked method's template in the Message tab.
    fn seed(&mut self) {
        let Some(template) = self.loaded().and_then(|l| l.template(&self.method)) else {
            return;
        };
        let text = serde_json::to_string_pretty(&template).unwrap_or_default();
        self.message = TextArea::from_text(&text);
        self.seeded = Some(text);
    }

    /// Pick the browser row `index`: the method changes, and the Message
    /// tab takes its template unless it holds typed text.
    pub fn pick(&mut self, index: usize) {
        let Some(path) = self
            .loaded()
            .and_then(|l| l.methods.get(index))
            .map(|m| m.path.clone())
        else {
            return;
        };
        self.selected = index;
        if path == self.method {
            return;
        }
        self.method = path;
        let typed = self.message.text();
        let untouched = typed.trim().is_empty()
            || typed.trim() == "{}"
            || self.seeded.as_deref() == Some(typed.as_str());
        if untouched {
            self.seed();
        }
    }

    fn texts(&self) -> Texts {
        Texts([
            self.target.text(),
            self.method.clone(),
            self.message.text(),
            self.metadata.text(),
            self.assertions.text(),
        ])
    }

    /// The Message tab as typed: one message, or a list for `messages`.
    pub fn message_value(&self) -> Result<Value, String> {
        let text = self.message.text();
        if text.trim().is_empty() {
            return Ok(Value::Object(Map::new()));
        }
        match serde_json::from_str::<Value>(&text) {
            Ok(value @ (Value::Object(_) | Value::Array(_))) => Ok(value),
            Ok(_) => Err("Message: a message must be a JSON object, or a list of them".to_owned()),
            Err(e) => Err(format!("Message: {e}")),
        }
    }

    /// The request as edited.
    pub fn to_grpc(&self) -> Result<GrpcRequest, String> {
        if self.texts() == self.loaded {
            return Ok(self.base.clone());
        }
        let mut doc = self.doc.clone();
        put(&mut doc, "target", Some(Value::String(self.target.text())));
        put(&mut doc, "method", Some(Value::String(self.method.clone())));
        let message = self.message_value()?;
        let (key, other) = if message.is_array() {
            ("messages", "message")
        } else {
            ("message", "messages")
        };
        doc.remove(other);
        put(&mut doc, key, Some(message));
        put(
            &mut doc,
            "metadata",
            yaml_value("Metadata", &self.metadata.text())?,
        );
        put(
            &mut doc,
            "assertions",
            yaml_value("Assertions", &self.assertions.text())?,
        );
        let text = render(&ordered(&doc));
        GrpcRequest::parse(Path::new("request"), &text).map_err(|e| e.to_string())
    }

    fn area(&self, tab: GrpcTab) -> Option<&TextArea> {
        match tab {
            GrpcTab::Method => None,
            GrpcTab::Message => Some(&self.message),
            GrpcTab::Metadata => Some(&self.metadata),
            GrpcTab::Assertions => Some(&self.assertions),
        }
    }

    fn area_mut(&mut self, tab: GrpcTab) -> Option<&mut TextArea> {
        match tab {
            GrpcTab::Method => None,
            GrpcTab::Message => Some(&mut self.message),
            GrpcTab::Metadata => Some(&mut self.metadata),
            GrpcTab::Assertions => Some(&mut self.assertions),
        }
    }

    // ---- keys -----------------------------------------------------------

    fn target_key(&mut self, chord: Chord) -> bool {
        let changed = match chord.code {
            Code::Enter | Code::Up | Code::Down if !chord.ctrl && !chord.alt => return true,
            _ => vim::type_key(&mut self.target, chord),
        };
        if changed {
            self.schema = SchemaState::Unloaded;
        }
        changed
    }

    fn tabs_key(&mut self, chord: Chord) -> bool {
        let i = GrpcTab::ALL
            .iter()
            .position(|t| *t == self.tab)
            .unwrap_or(0);
        let n = GrpcTab::ALL.len();
        match chord.code {
            _ if chord.ctrl || chord.alt => return false,
            Code::Right | Code::Char('l') => self.tab = GrpcTab::ALL[(i + 1) % n],
            Code::Left | Code::Char('h') => self.tab = GrpcTab::ALL[(i + n - 1) % n],
            Code::Char(c @ '1'..='4') => self.tab = GrpcTab::ALL[usize::from(c as u8 - b'1')],
            _ => return false,
        }
        true
    }

    /// The browser: `Up` / `Down` move, `Enter` picks the method.
    fn browser_key(&mut self, chord: Chord) -> bool {
        let n = self.loaded().map_or(0, |l| l.methods.len());
        if n == 0 || chord.ctrl || chord.alt {
            return false;
        }
        match chord.code {
            Code::Up | Code::Char('k') => self.selected = self.selected.saturating_sub(1),
            Code::Down | Code::Char('j') => self.selected = (self.selected + 1).min(n - 1),
            Code::Enter => self.pick(self.selected),
            _ => return false,
        }
        true
    }

    fn content_key(&mut self, chord: Chord, vim: Option<&mut Vim>) -> bool {
        if self.tab == GrpcTab::Method {
            return self.browser_key(chord);
        }
        let tab = self.tab;
        match self.area_mut(tab) {
            Some(area) => text_key(area, chord, vim),
            None => false,
        }
    }
}

impl PaneEditor for GrpcEditor {
    fn kind(&self) -> &str {
        "grpc"
    }

    fn tabs(&self) -> Vec<&'static str> {
        GrpcTab::ALL.iter().map(|t| t.label()).collect()
    }

    fn tab(&self) -> usize {
        GrpcTab::ALL
            .iter()
            .position(|t| *t == self.tab)
            .unwrap_or(0)
    }

    fn set_tab(&mut self, index: usize) {
        if let Some(tab) = GrpcTab::ALL.get(index) {
            self.tab = *tab;
        }
    }

    fn method(&self) -> Option<&str> {
        Some("GRPC")
    }

    fn url(&self) -> Option<&TextArea> {
        Some(&self.target)
    }

    fn key(&mut self, focus: Focus, chord: Chord, vim: Option<&mut Vim>) -> bool {
        match focus {
            Focus::Url => self.target_key(chord),
            Focus::EditorTabs => self.tabs_key(chord),
            Focus::EditorBody => self.content_key(chord, vim),
            _ => false,
        }
    }

    fn cycle_mode(&mut self) -> bool {
        false
    }

    fn request(&self) -> Result<Request, String> {
        self.to_grpc().map(|r| Request::Grpc(Box::new(r)))
    }

    fn render(&self, frame: &mut Frame, rect: Rect, ctx: &RenderCtx) {
        grpc_view::content(frame, rect, self, ctx);
    }

    fn variable_at_cursor(&self, focus: Focus) -> Option<String> {
        let area = match focus {
            Focus::Url => &self.target,
            Focus::EditorBody => self.area(self.tab)?,
            _ => return None,
        };
        let cursor = area.cursor();
        vars_view::at(area.lines().get(cursor.row)?, cursor.col)
    }

    fn external_text(&self) -> Option<(String, &'static str)> {
        match self.tab {
            GrpcTab::Method => None,
            GrpcTab::Message => Some((self.message.text(), "json")),
            tab => self.area(tab).map(|a| (a.text(), "yaml")),
        }
    }

    fn set_external_text(&mut self, text: &str) {
        let text = text.strip_suffix('\n').unwrap_or(text);
        let text = text.strip_suffix('\r').unwrap_or(text);
        let tab = self.tab;
        if let Some(area) = self.area_mut(tab) {
            *area = TextArea::from_text(&text.replace("\r\n", "\n"));
        }
    }

    fn response_tabs(&self) -> &'static [&'static str] {
        &["Messages", "Status", "Timeline"]
    }

    fn grpc(&self) -> Option<&GrpcEditor> {
        Some(self)
    }

    fn grpc_mut(&mut self) -> Option<&mut GrpcEditor> {
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

/// The members of a grpc file in schema order; unknown ones last.
const ORDER: [&str; 18] = [
    "version",
    "kind",
    "name",
    "description",
    "tags",
    "target",
    "method",
    "message",
    "messages",
    "metadata",
    "deadline_ms",
    "tls",
    "transport",
    "schema",
    "stream",
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
    use prost_reflect::DescriptorPool;
    use rustman_core::model::KindRegistry;
    use rustman_grpc::SchemaOrigin;
    use rustman_test_support::grpc::descriptor_set;
    use serde_json::json;

    fn request(text: &str) -> GrpcRequest {
        let full = format!("version: 1\nkind: grpc\nname: R\ntarget: 'grpc://h:1'\n{text}");
        match KindRegistry::default().load(Path::new("r.rman.yaml"), &full) {
            Ok(Request::Grpc(grpc)) => *grpc,
            other => panic!("not a grpc request: {other:?}"),
        }
    }

    fn loaded() -> SchemaState {
        let pool = DescriptorPool::decode(descriptor_set().as_slice()).unwrap();
        SchemaState::Ready(Arc::new(LoadedSchema::new(Schema::new(
            pool,
            SchemaOrigin::DescriptorSet,
        ))))
    }

    fn keys(editor: &mut GrpcEditor, focus: Focus, keys: &str) {
        for key in keys.split(' ') {
            editor.key(focus, parse_chord(key).unwrap(), None);
        }
    }

    #[test]
    fn an_unedited_request_keeps_its_file_text() {
        let base = request("method: shop.v1.Catalog/GetProduct\nmessage: {id: p-1}\n# kept\n");
        let editor = GrpcEditor::new(&base);
        assert_eq!(
            editor.tabs(),
            ["Method", "Message", "Metadata", "Assertions"]
        );
        assert_eq!(editor.to_grpc().unwrap(), base);
        assert_eq!(editor.method(), Some("GRPC"));
        assert_eq!(editor.message_value().unwrap(), json!({"id": "p-1"}));
    }

    #[test]
    fn picking_a_method_seeds_its_template_unless_typed() {
        let base = request("method: shop.v1.Catalog/GetProduct\nmessage: {}\n");
        let mut editor = GrpcEditor::new(&base);
        editor.schema_loaded(loaded());
        assert_eq!(editor.selected, 0);
        assert_eq!(editor.call_type(), Some(CallType::Unary));
        keys(&mut editor, Focus::EditorBody, "Down Enter");
        assert_eq!(editor.method, "shop.v1.Catalog/ListProducts");
        assert_eq!(
            editor.message_value().unwrap(),
            json!({"pageSize": 0, "category": "CATEGORY_UNSPECIFIED"})
        );
        keys(&mut editor, Focus::EditorBody, "Down Down Enter");
        assert!(editor.is_bidi());
        assert_eq!(editor.message_value().unwrap(), json!([{"text": ""}]));
        let edited = editor.to_grpc().unwrap();
        assert!(edited.has_message_list(), "{}", edited.source);
        editor.message = TextArea::from_text("[{\"text\": \"typed\"}]");
        keys(&mut editor, Focus::EditorBody, "Up Enter");
        assert_eq!(editor.method, "shop.v1.Catalog/UploadProducts");
        assert_eq!(
            editor.message.text(),
            "[{\"text\": \"typed\"}]",
            "typed text is kept"
        );
    }

    #[test]
    fn edits_are_written_in_schema_order_and_checked() {
        let base = request(
            "assertions:\n  - grpc_status: OK\nmethod: shop.v1.Catalog/GetProduct\nmessage: {id: a}\n",
        );
        let mut editor = GrpcEditor::new(&base);
        editor.metadata = TextArea::from_text("- name: authorization\n  value: Bearer {{token}}");
        let edited = editor.to_grpc().unwrap();
        let text = &edited.source;
        let at = |key: &str| text.find(key).unwrap_or_else(|| panic!("{key} in {text}"));
        assert!(at("method:") < at("message:") && at("message:") < at("metadata:"));
        assert!(at("metadata:") < at("assertions:"), "{text}");
        editor.message = TextArea::from_text("5");
        assert!(editor.to_grpc().unwrap_err().starts_with("Message: "));
        editor.message = TextArea::from_text("{}");
        editor.metadata = TextArea::from_text("- name: [");
        assert!(editor.to_grpc().unwrap_err().starts_with("Metadata: "));
        keys(&mut editor, Focus::EditorTabs, "3");
        assert_eq!(editor.external_text().unwrap().1, "yaml");
    }

    #[test]
    fn a_typed_message_is_checked_against_the_schema() {
        let SchemaState::Ready(schema) = loaded() else {
            unreachable!()
        };
        let get = "shop.v1.Catalog/GetProduct";
        assert!(schema.check(get, &json!({"id": "p-1"})).is_ok());
        let err = schema.check(get, &json!({"idd": "p-1"})).unwrap_err();
        assert!(err.contains("unknown field 'idd'"), "{err}");
    }
}
