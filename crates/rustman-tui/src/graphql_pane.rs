//! The `graphql` request pane (spec: TUI GraphQL pane): the endpoint, a
//! document editor with highlighting, inline validation diagnostics and a
//! completion popup, a variables editor (JSON) and a schema explorer.
//!
//! The schema comes from a background load ([`crate::graphql_schema`]);
//! until it arrives, and when there is none, only the syntax is checked.
//! Fields the pane does not show (headers, auth, transport, assertions...)
//! are kept as loaded; an edited request is written with
//! [`graphql_to_yaml`], an unedited one keeps its file text.

use std::fmt;
use std::sync::Arc;

use ratatui::Frame;
use ratatui::layout::Rect;
use rustman_core::format::write::graphql_to_yaml;
use rustman_core::model::{GraphqlMethod, GraphqlRequest, GraphqlTransport, Request};
use rustman_graphql::{
    Completion, GraphqlSchema, SchemaOverview, TypeDetail, Validation, complete, validate,
};
use serde_json::{Map, Value};

use crate::focus::Focus;
use crate::graphql_view;
use crate::keymap::chord::{Chord, Code};
use crate::pane::{PaneEditor, RenderCtx, RequestPane};
use crate::textarea::TextArea;
use crate::vars_view;
use crate::vim::{self, Vim};

/// The editor tabs, in order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GqlTab {
    Query,
    Variables,
    Schema,
}

impl GqlTab {
    pub const ALL: [Self; 3] = [Self::Query, Self::Variables, Self::Schema];

    pub fn label(self) -> &'static str {
        match self {
            Self::Query => "Query",
            Self::Variables => "Variables",
            Self::Schema => "Schema",
        }
    }
}

/// A schema the editor validates and completes against.
#[derive(Debug, Clone)]
pub struct LoadedSchema {
    pub schema: Arc<GraphqlSchema>,
    /// Where it came from: `cache (fetched ...)`, `schema_file x`.
    pub origin: String,
    pub overview: SchemaOverview,
}

/// Where the editor's schema stands.
#[derive(Debug, Clone)]
pub enum SchemaState {
    /// Not asked for yet: the next update starts a load.
    Unloaded,
    /// A load (or a refresh) is running.
    Loading,
    /// Loaded; `None` when the endpoint has no schema yet.
    Ready(Option<LoadedSchema>),
    /// The load failed (redacted reason); only syntax is checked.
    Failed(String),
}

/// The completion popup.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Popup {
    pub items: Vec<Completion>,
    pub selected: usize,
}

/// The schema explorer: the type list, or one type's members.
#[derive(Debug, Clone, Default)]
pub struct Explorer {
    /// The selected row of the type list.
    pub selected: usize,
    /// The open type, with its selected member row.
    pub open: Option<(TypeDetail, usize)>,
    /// Types opened before it, for going back.
    pub trail: Vec<String>,
}

/// The `graphql` pane.
pub struct GraphqlPane;

impl RequestPane for GraphqlPane {
    fn kind(&self) -> &'static str {
        "graphql"
    }

    fn open(&self, request: &Request) -> Option<Box<dyn PaneEditor>> {
        match request {
            Request::Graphql(graphql) => Some(Box::new(GraphqlEditor::new(graphql))),
            _ => None,
        }
    }
}

/// Editing state of a `graphql` request.
#[derive(Clone)]
pub struct GraphqlEditor {
    base: GraphqlRequest,
    pub endpoint: TextArea,
    /// The inline document, or the text of `document_file` once loaded.
    pub document: TextArea,
    pub variables: TextArea,
    pub tab: GqlTab,
    pub schema: SchemaState,
    /// Diagnostics of the document and variables as typed.
    pub validation: Validation,
    /// Why the variables do not parse.
    pub variables_error: Option<String>,
    pub popup: Option<Popup>,
    pub explorer: Explorer,
}

impl fmt::Debug for GraphqlEditor {
    /// Variables and headers (tokens) are left out.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("GraphqlEditor")
            .field("endpoint", &self.endpoint.text())
            .field("tab", &self.tab)
            .field("diagnostics", &self.validation.diagnostics.len())
            .finish_non_exhaustive()
    }
}

impl GraphqlEditor {
    pub fn new(graphql: &GraphqlRequest) -> Self {
        let variables = graphql
            .variables
            .as_ref()
            .and_then(|v| serde_json::to_string_pretty(v).ok())
            .unwrap_or_default();
        let mut editor = Self {
            base: graphql.clone(),
            endpoint: TextArea::from_text(&graphql.endpoint),
            document: TextArea::from_text(graphql.document.as_deref().unwrap_or_default()),
            variables: TextArea::from_text(&variables),
            tab: GqlTab::Query,
            schema: SchemaState::Unloaded,
            validation: Validation::default(),
            variables_error: None,
            popup: None,
            explorer: Explorer::default(),
        };
        editor.revalidate();
        editor
    }

    /// The request file as loaded.
    pub fn base(&self) -> &GraphqlRequest {
        &self.base
    }

    /// The `document_file` the document comes from, if any: it is shown,
    /// not edited here.
    pub fn document_file(&self) -> Option<&str> {
        self.base.document_file.as_deref()
    }

    /// The loaded schema, if any.
    pub fn loaded(&self) -> Option<&LoadedSchema> {
        match &self.schema {
            SchemaState::Ready(Some(loaded)) => Some(loaded),
            _ => None,
        }
    }

    /// The schema arrived (or failed): show it and check the document
    /// against it. `document` is the text of `document_file`.
    pub fn schema_loaded(&mut self, state: SchemaState, document: Option<String>) {
        if let Some(text) = document.filter(|_| self.document_file().is_some()) {
            self.document = TextArea::from_text(&text);
        }
        self.schema = state;
        self.explorer = Explorer::default();
        self.popup = None;
        self.revalidate();
    }

    /// Check the document and the variables as typed.
    pub fn revalidate(&mut self) {
        let (variables, error) = match self.variables_map() {
            Ok(map) => (map.unwrap_or_default(), None),
            Err(message) => (Map::new(), Some(message)),
        };
        self.variables_error = error;
        let schema = self.loaded().map(|l| Arc::clone(&l.schema));
        let mut validation = validate(
            &self.document.text(),
            schema.as_deref(),
            self.base.operation_name.as_deref(),
            &variables,
        );
        // Placeholders are resolved on send: their types cannot be checked
        // while editing.
        if self.variables.text().contains("{{") {
            validation.diagnostics.retain(|d| d.position.is_some());
        }
        self.validation = validation;
    }

    /// The variables as typed: `None` when empty.
    fn variables_map(&self) -> Result<Option<Map<String, Value>>, String> {
        let text = self.variables.text();
        if text.trim().is_empty() {
            return Ok(None);
        }
        match serde_json::from_str::<Value>(&text) {
            Ok(Value::Object(map)) => Ok(Some(map)),
            Ok(_) => Err("Variables: not a JSON object".to_owned()),
            Err(e) => Err(format!("Variables: {e}")),
        }
    }

    /// The request as edited.
    pub fn to_graphql(&self) -> Result<GraphqlRequest, String> {
        let mut graphql = self.base.clone();
        graphql.endpoint = self.endpoint.text();
        if graphql.document_file.is_none() {
            graphql.document = Some(self.document.text());
        }
        graphql.variables = self.variables_map()?;
        if graphql == self.base {
            return Ok(graphql);
        }
        graphql.source = graphql_to_yaml(&graphql);
        Ok(graphql)
    }

    /// The label left of the endpoint: how the operation travels.
    fn transport_label(&self) -> &'static str {
        match (
            self.base.effective_transport(),
            self.base.effective_method(),
        ) {
            (GraphqlTransport::Ws, _) => "WS",
            (GraphqlTransport::Sse, _) => "SSE",
            (GraphqlTransport::Http, GraphqlMethod::Get) => "GET",
            (GraphqlTransport::Http, GraphqlMethod::Post) => "POST",
        }
    }

    /// The cursor as a byte offset of the document text.
    pub fn cursor_offset(&self) -> usize {
        let cursor = self.document.cursor();
        let lines = self.document.lines();
        let before: usize = lines.iter().take(cursor.row).map(|l| l.len() + 1).sum();
        let line = lines.get(cursor.row).map_or("", String::as_str);
        let col = line
            .char_indices()
            .nth(cursor.col)
            .map_or(line.len(), |(i, _)| i);
        before + col
    }

    // ---- completion ---------------------------------------------------

    /// `Ctrl+Space`: open the popup at the cursor; false when there is
    /// nothing to complete (no schema, read-only document, no candidate).
    pub fn open_completion(&mut self) -> bool {
        self.tab = GqlTab::Query;
        self.popup = self.candidates().map(|items| Popup { items, selected: 0 });
        self.popup.is_some()
    }

    fn candidates(&self) -> Option<Vec<Completion>> {
        if self.document_file().is_some() {
            return None;
        }
        let schema = &self.loaded()?.schema;
        let items = complete(schema, &self.document.text(), self.cursor_offset());
        (!items.is_empty()).then_some(items)
    }

    fn popup_key(&mut self, chord: Chord, vim: Option<&mut Vim>) -> bool {
        let Some(popup) = &mut self.popup else {
            return false;
        };
        let n = popup.items.len();
        match chord.code {
            Code::Up if !chord.ctrl => popup.selected = (popup.selected + n - 1) % n,
            Code::Down if !chord.ctrl => popup.selected = (popup.selected + 1) % n,
            Code::Enter | Code::Tab => self.accept(),
            Code::Esc => self.popup = None,
            _ => return self.type_in_popup(chord, vim),
        }
        true
    }

    /// Typing while the popup is open edits the document and narrows it.
    fn type_in_popup(&mut self, chord: Chord, vim: Option<&mut Vim>) -> bool {
        let typed = text_key(&mut self.document, chord, vim);
        if typed {
            self.popup = self.candidates().map(|items| Popup { items, selected: 0 });
            self.revalidate();
        }
        typed
    }

    /// Insert the selected candidate in place of the name being typed.
    fn accept(&mut self) {
        let Some(popup) = self.popup.take() else {
            return;
        };
        let Some(item) = popup.items.get(popup.selected) else {
            return;
        };
        let typed = self.typed_prefix();
        let rest: String = item.label.chars().skip(typed.chars().count()).collect();
        self.document.checkpoint();
        self.document.insert_str(&rest);
        self.revalidate();
    }

    /// The name characters right before the cursor.
    fn typed_prefix(&self) -> String {
        let cursor = self.document.cursor();
        let line = self
            .document
            .lines()
            .get(cursor.row)
            .map_or("", String::as_str);
        let before: Vec<char> = line.chars().take(cursor.col).collect();
        let start = before
            .iter()
            .rposition(|c| !(c.is_alphanumeric() || *c == '_'))
            .map_or(0, |i| i + 1);
        before[start..].iter().collect()
    }

    // ---- keys -----------------------------------------------------------

    fn endpoint_key(&mut self, chord: Chord) -> bool {
        match chord.code {
            Code::Enter | Code::Up | Code::Down if !chord.ctrl && !chord.alt => true,
            _ => vim::type_key(&mut self.endpoint, chord),
        }
    }

    fn tabs_key(&mut self, chord: Chord) -> bool {
        let i = GqlTab::ALL.iter().position(|t| *t == self.tab).unwrap_or(0);
        let n = GqlTab::ALL.len();
        match chord.code {
            _ if chord.ctrl || chord.alt => return false,
            Code::Right | Code::Char('l') => self.tab = GqlTab::ALL[(i + 1) % n],
            Code::Left | Code::Char('h') => self.tab = GqlTab::ALL[(i + n - 1) % n],
            Code::Char(c @ '1'..='3') => self.tab = GqlTab::ALL[usize::from(c as u8 - b'1')],
            _ => return false,
        }
        true
    }

    fn content_key(&mut self, chord: Chord, vim: Option<&mut Vim>) -> bool {
        let changed = match self.tab {
            GqlTab::Query if self.popup.is_some() => return self.popup_key(chord, vim),
            GqlTab::Query if self.document_file().is_some() => return false,
            GqlTab::Query => text_key(&mut self.document, chord, vim),
            GqlTab::Variables => text_key(&mut self.variables, chord, vim),
            GqlTab::Schema => return self.explorer_key(chord),
        };
        if changed {
            self.revalidate();
        }
        changed
    }

    fn explorer_key(&mut self, chord: Chord) -> bool {
        if chord.ctrl || chord.alt {
            return false;
        }
        match chord.code {
            Code::Up | Code::Char('k') => self.explorer_move(false),
            Code::Down | Code::Char('j') => self.explorer_move(true),
            Code::Enter | Code::Right | Code::Char('l') => self.explorer_open(),
            Code::Backspace | Code::Left | Code::Char('h') => self.explorer_back(),
            _ => return false,
        }
        true
    }

    fn explorer_move(&mut self, down: bool) {
        let rows = self.explorer_rows();
        let selected = match &mut self.explorer.open {
            Some((_, row)) => row,
            None => &mut self.explorer.selected,
        };
        *selected = if down {
            (*selected + 1).min(rows.saturating_sub(1))
        } else {
            selected.saturating_sub(1)
        };
    }

    /// The rows of the list shown: types, or the open type's members.
    pub fn explorer_rows(&self) -> usize {
        match (&self.explorer.open, self.loaded()) {
            (Some((detail, _)), _) => graphql_view::member_rows(detail).len(),
            (None, Some(loaded)) => loaded.overview.types.len(),
            (None, None) => 0,
        }
    }

    /// Open the selected type, or the type of the selected member.
    fn explorer_open(&mut self) {
        let Some(loaded) = self.loaded() else { return };
        let name = match &self.explorer.open {
            Some((detail, row)) => graphql_view::member_rows(detail)
                .get(*row)
                .and_then(|r| r.target.clone()),
            None => loaded
                .overview
                .types
                .get(self.explorer.selected)
                .map(|t| t.name.clone()),
        };
        let Some(detail) = name.and_then(|n| loaded.schema.type_detail(&n)) else {
            return;
        };
        if let Some((current, _)) = self.explorer.open.take() {
            self.explorer.trail.push(current.name);
        }
        self.explorer.open = Some((detail, 0));
    }

    fn explorer_back(&mut self) {
        let previous = self.explorer.trail.pop();
        let detail = previous.and_then(|n| self.loaded()?.schema.type_detail(&n));
        self.explorer.open = detail.map(|d| (d, 0));
    }
}

impl PaneEditor for GraphqlEditor {
    fn kind(&self) -> &str {
        "graphql"
    }

    fn tabs(&self) -> Vec<&'static str> {
        GqlTab::ALL.iter().map(|t| t.label()).collect()
    }

    fn tab(&self) -> usize {
        GqlTab::ALL.iter().position(|t| *t == self.tab).unwrap_or(0)
    }

    fn set_tab(&mut self, index: usize) {
        if let Some(tab) = GqlTab::ALL.get(index) {
            self.tab = *tab;
        }
    }

    fn method(&self) -> Option<&str> {
        Some(self.transport_label())
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
        self.to_graphql().map(|g| Request::Graphql(Box::new(g)))
    }

    fn render(&self, frame: &mut Frame, rect: Rect, ctx: &RenderCtx) {
        graphql_view::content(frame, rect, self, ctx);
    }

    fn variable_at_cursor(&self, focus: Focus) -> Option<String> {
        let area = match (focus, self.tab) {
            (Focus::Url, _) => &self.endpoint,
            (Focus::EditorBody, GqlTab::Variables) => &self.variables,
            _ => return None,
        };
        let cursor = area.cursor();
        vars_view::at(area.lines().get(cursor.row)?, cursor.col)
    }

    fn external_text(&self) -> Option<(String, &'static str)> {
        match self.tab {
            GqlTab::Query if self.document_file().is_none() => {
                Some((self.document.text(), "graphql"))
            }
            GqlTab::Variables => Some((self.variables.text(), "json")),
            _ => None,
        }
    }

    fn set_external_text(&mut self, text: &str) {
        let text = text.strip_suffix('\n').unwrap_or(text);
        let text = text.strip_suffix('\r').unwrap_or(text);
        let area = TextArea::from_text(&text.replace("\r\n", "\n"));
        match self.tab {
            GqlTab::Variables => self.variables = area,
            _ => self.document = area,
        }
        self.revalidate();
    }

    fn response_tabs(&self) -> &'static [&'static str] {
        &["Data", "Errors", "Extensions", "Events"]
    }

    fn graphql(&self) -> Option<&GraphqlEditor> {
        Some(self)
    }

    fn graphql_mut(&mut self) -> Option<&mut GraphqlEditor> {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::keymap::chord::parse_chord;
    use rustman_core::model::KindRegistry;
    use std::path::Path;

    const SDL: &str = "type Query { user(id: ID!): User }\n\
        type User { id: ID! name: String email: String @deprecated(reason: \"use contact\") \
        posts: [Post!]! }\ntype Post { title: String }\n";

    fn request(text: &str) -> GraphqlRequest {
        let full =
            format!("version: 1\nkind: graphql\nname: Q\nendpoint: http://x/graphql\n{text}");
        match KindRegistry::default().load(Path::new("q.rman.yaml"), &full) {
            Ok(Request::Graphql(graphql)) => *graphql,
            other => panic!("not a graphql request: {other:?}"),
        }
    }

    fn with_schema(editor: &mut GraphqlEditor) {
        let schema = GraphqlSchema::from_sdl(SDL, "test.graphql").unwrap();
        let overview = schema.overview();
        let loaded = LoadedSchema {
            schema: Arc::new(schema),
            origin: "schema_file test.graphql".into(),
            overview,
        };
        editor.schema_loaded(SchemaState::Ready(Some(loaded)), None);
    }

    fn keys(editor: &mut GraphqlEditor, focus: Focus, keys: &[&str]) {
        for key in keys {
            editor.key(focus, parse_chord(key).unwrap(), None);
        }
    }

    fn type_text(editor: &mut GraphqlEditor, text: &str) {
        for c in text.chars() {
            let key = if c == ' ' {
                "Space".to_owned()
            } else {
                c.to_string()
            };
            editor.key(Focus::EditorBody, parse_chord(&key).unwrap(), None);
        }
    }

    #[test]
    fn an_unedited_request_keeps_its_file_text() {
        let graphql = request("document: '{ user(id: 1) { name } }'\nvariables:\n  id: 1\n");
        let editor = GraphqlEditor::new(&graphql);
        assert_eq!(editor.to_graphql().unwrap(), graphql);
        assert_eq!(editor.method(), Some("POST"));
        assert_eq!(editor.tabs(), ["Query", "Variables", "Schema"]);
    }

    #[test]
    fn edits_are_written_as_a_new_file_text() {
        let mut editor = GraphqlEditor::new(&request("document: '{ a }'\n"));
        keys(&mut editor, Focus::EditorBody, &["End"]);
        type_text(&mut editor, "x");
        editor.tab = GqlTab::Variables;
        type_text(&mut editor, "{\"id\":\"{{id}}\"}");
        let edited = editor.to_graphql().unwrap();
        assert_eq!(edited.document.as_deref(), Some("{ a }x"));
        assert_eq!(edited.variables.unwrap()["id"], "{{id}}");
        assert!(
            edited.source.contains("document: '{ a }x'"),
            "{}",
            edited.source
        );
        editor.variables = TextArea::from_text("[1]");
        assert_eq!(
            editor.to_graphql().unwrap_err(),
            "Variables: not a JSON object"
        );
    }

    #[test]
    fn without_a_schema_only_syntax_is_checked() {
        let editor = GraphqlEditor::new(&request("document: '{ user(id: 1) { emial }'\n"));
        assert!(editor.validation.syntax_only);
        let first = &editor.validation.diagnostics[0];
        assert!(first.message.contains("expected"), "{first:?}");
    }

    #[test]
    fn a_schema_reports_unknown_fields_where_they_are() {
        let mut editor = GraphqlEditor::new(&request("document: '{ user(id: 1) { emial } }'\n"));
        with_schema(&mut editor);
        let shown: Vec<String> = editor
            .validation
            .errors()
            .map(ToString::to_string)
            .collect();
        assert!(
            shown.contains(&"1:17: Cannot query field \"emial\" on type \"User\".".to_owned()),
            "{shown:?}"
        );
    }

    #[test]
    fn completion_inserts_the_rest_of_the_selected_name() {
        let mut editor = GraphqlEditor::new(&request("document: '{ user(id: 1) {  } }'\n"));
        with_schema(&mut editor);
        keys(&mut editor, Focus::EditorBody, &["Home"]);
        for _ in 0..16 {
            keys(&mut editor, Focus::EditorBody, &["Right"]);
        }
        type_text(&mut editor, "na");
        assert!(editor.open_completion());
        let labels: Vec<&str> = editor
            .popup
            .as_ref()
            .unwrap()
            .items
            .iter()
            .map(|c| c.label.as_str())
            .collect();
        assert_eq!(labels, ["name"]);
        keys(&mut editor, Focus::EditorBody, &["Enter"]);
        assert_eq!(editor.document.text(), "{ user(id: 1) { name } }");
        assert!(editor.popup.is_none());
        assert!(!editor.validation.has_errors(), "{:?}", editor.validation);
        // Typing narrows an open popup; Esc closes it.
        keys(
            &mut editor,
            Focus::EditorBody,
            &["Left", "Left", "Left", "Left"],
        );
        assert!(!editor.open_completion() || editor.popup.is_some());
        keys(&mut editor, Focus::EditorBody, &["Esc"]);
        assert!(editor.popup.is_none());
    }

    #[test]
    fn the_explorer_walks_from_a_type_to_its_field_types_and_back() {
        let mut editor = GraphqlEditor::new(&request("document: '{ a }'\n"));
        with_schema(&mut editor);
        editor.tab = GqlTab::Schema;
        let names: Vec<&str> = editor
            .loaded()
            .unwrap()
            .overview
            .types
            .iter()
            .map(|t| t.name.as_str())
            .collect();
        let user = names.iter().position(|n| *n == "User").unwrap();
        for _ in 0..user {
            keys(&mut editor, Focus::EditorBody, &["Down"]);
        }
        keys(&mut editor, Focus::EditorBody, &["Enter"]);
        assert_eq!(editor.explorer.open.as_ref().unwrap().0.name, "User");
        let rows = graphql_view::member_rows(&editor.explorer.open.as_ref().unwrap().0);
        let posts = rows
            .iter()
            .position(|r| r.text.starts_with("posts"))
            .unwrap();
        for _ in 0..posts {
            keys(&mut editor, Focus::EditorBody, &["Down"]);
        }
        keys(&mut editor, Focus::EditorBody, &["Enter"]);
        assert_eq!(editor.explorer.open.as_ref().unwrap().0.name, "Post");
        keys(&mut editor, Focus::EditorBody, &["Backspace"]);
        assert_eq!(editor.explorer.open.as_ref().unwrap().0.name, "User");
        keys(&mut editor, Focus::EditorBody, &["Backspace"]);
        assert!(editor.explorer.open.is_none());
    }

    #[test]
    fn a_document_file_is_shown_not_edited() {
        let mut editor = GraphqlEditor::new(&request("document_file: q.graphql\n"));
        editor.schema_loaded(SchemaState::Ready(None), Some("{ a }".into()));
        assert_eq!(editor.document.text(), "{ a }");
        assert!(!editor.key(Focus::EditorBody, parse_chord("x").unwrap(), None));
        assert_eq!(editor.external_text(), None);
        let saved = editor.to_graphql().unwrap();
        assert_eq!(saved.document, None, "the file stays the source");
    }
}
