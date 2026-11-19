//! The realtime pane for `kind: ws` and `kind: sse` requests
//! (add-realtime-client "Kind registration", "Message timeline"): the URL
//! on the URL row, and the tabs Timeline (the live session with direction,
//! type and text filters), Compose (a text, JSON or binary frame to send
//! while connected), Headers, Steps (WebSocket) or Body (SSE), and
//! Assertions, each that part of the file as YAML.
//!
//! `request.send` connects; while connected it sends the composed frame
//! (Compose tab) and `Esc` closes the session. An unedited request keeps
//! its file text; once edited, the file is rebuilt from the loaded document
//! with the edited parts in place, in schema order, and parsed again.

use std::path::Path;

use ratatui::Frame;
use ratatui::layout::Rect;
use rustman_core::format::from_yaml;
use rustman_core::format::write::{Node, json_node, render};
use rustman_core::model::{Request, SseRequest, WsRequest};
use serde_json::{Map, Value};

use crate::focus::Focus;
use crate::keymap::chord::{Chord, Code};
use crate::pane::{PaneEditor, RenderCtx, RequestPane};
use crate::realtime::{ComposeMode, Live, TimelineFilter};
use crate::realtime_view;
use crate::textarea::TextArea;
use crate::vars_view;
use crate::vim::{self, Vim};

/// The editor tabs, in order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RtTab {
    Timeline,
    Compose,
    Headers,
    /// `steps` (WebSocket) or `body` (SSE).
    Script,
    Assertions,
}

impl RtTab {
    pub const ALL: [Self; 5] = [
        Self::Timeline,
        Self::Compose,
        Self::Headers,
        Self::Script,
        Self::Assertions,
    ];
}

/// The realtime pane, registered once per kind.
pub struct RealtimePane {
    kind: &'static str,
}

impl RealtimePane {
    pub const WS: Self = Self { kind: "ws" };
    pub const SSE: Self = Self { kind: "sse" };
}

impl RequestPane for RealtimePane {
    fn kind(&self) -> &'static str {
        self.kind
    }

    fn open(&self, request: &Request) -> Option<Box<dyn PaneEditor>> {
        match request {
            Request::Ws(ws) => Some(Box::new(RealtimeEditor::new(request, &ws.source, &ws.url))),
            Request::Sse(sse) => Some(Box::new(RealtimeEditor::new(
                request,
                &sse.source,
                &sse.url,
            ))),
            _ => None,
        }
    }
}

/// Editing state of a `ws` or `sse` request, and its live session.
pub struct RealtimeEditor {
    base: Request,
    /// The loaded file as a document, for the members the pane leaves alone.
    doc: Map<String, Value>,
    loaded: [String; 4],
    pub url: TextArea,
    pub headers: TextArea,
    /// `steps` (WebSocket) or `body` (SSE).
    pub script: TextArea,
    pub assertions: TextArea,
    pub compose: TextArea,
    pub mode: ComposeMode,
    pub tab: RtTab,
    pub filter: TimelineFilter,
    /// Typing goes to the text filter.
    pub editing_filter: bool,
    /// The selected entry among the filtered ones; `None` follows the
    /// newest.
    pub selected: Option<usize>,
    /// The session of the last connect.
    pub live: Live,
}

impl std::fmt::Debug for RealtimeEditor {
    /// Headers and frames (which may hold tokens) are left out.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RealtimeEditor")
            .field("kind", &self.base.kind())
            .field("url", &self.url.text())
            .field("tab", &self.tab)
            .finish_non_exhaustive()
    }
}

impl RealtimeEditor {
    pub fn new(request: &Request, source: &str, url: &str) -> Self {
        let doc = match from_yaml::<Value>(Path::new("request"), source) {
            Ok(Value::Object(map)) => map,
            _ => Map::new(),
        };
        let yaml = |key: &str| doc.get(key).map(yaml_text).unwrap_or_default();
        let mut editor = Self {
            loaded: Default::default(),
            url: TextArea::from_text(url),
            headers: TextArea::from_text(&yaml("headers")),
            script: TextArea::from_text(&yaml(script_key(request.kind()))),
            assertions: TextArea::from_text(&yaml("assertions")),
            compose: TextArea::from_text(""),
            mode: ComposeMode::Text,
            tab: RtTab::Timeline,
            filter: TimelineFilter::default(),
            editing_filter: false,
            selected: None,
            live: Live::new(max_timeline(request)),
            base: request.clone(),
            doc,
        };
        editor.loaded = editor.texts();
        editor
    }

    /// `ws` or `sse`.
    pub fn is_ws(&self) -> bool {
        matches!(self.base, Request::Ws(_))
    }

    pub fn tab_label(&self, tab: RtTab) -> &'static str {
        match tab {
            RtTab::Timeline => "Timeline",
            RtTab::Compose => "Compose",
            RtTab::Headers => "Headers",
            RtTab::Script if self.is_ws() => "Steps",
            RtTab::Script => "Body",
            RtTab::Assertions => "Assertions",
        }
    }

    fn texts(&self) -> [String; 4] {
        [&self.url, &self.headers, &self.script, &self.assertions].map(TextArea::text)
    }

    /// The request as edited.
    pub fn to_request(&self) -> Result<Request, String> {
        if self.texts() == self.loaded {
            return Ok(self.base.clone());
        }
        let mut doc = self.doc.clone();
        doc.insert("url".into(), Value::String(self.url.text()));
        let parts = [
            ("headers", "Headers", &self.headers),
            (
                script_key(self.base.kind()),
                self.tab_label(RtTab::Script),
                &self.script,
            ),
            ("assertions", "Assertions", &self.assertions),
        ];
        for (key, label, area) in parts {
            match yaml_value(label, &area.text())? {
                Some(value) => doc.insert(key.to_owned(), value),
                None => doc.remove(key),
            };
        }
        let text = render(&ordered(&doc));
        let file = Path::new("request");
        let parsed = if self.is_ws() {
            WsRequest::parse(file, &text).map(|r| Request::Ws(Box::new(r)))
        } else {
            SseRequest::parse(file, &text).map(|r| Request::Sse(Box::new(r)))
        };
        parsed.map_err(|e| e.to_string())
    }

    fn area_mut(&mut self, tab: RtTab) -> Option<&mut TextArea> {
        match tab {
            RtTab::Timeline => None,
            RtTab::Compose => Some(&mut self.compose),
            RtTab::Headers => Some(&mut self.headers),
            RtTab::Script => Some(&mut self.script),
            RtTab::Assertions => Some(&mut self.assertions),
        }
    }

    pub fn area(&self, tab: RtTab) -> Option<&TextArea> {
        match tab {
            RtTab::Timeline => None,
            RtTab::Compose => Some(&self.compose),
            RtTab::Headers => Some(&self.headers),
            RtTab::Script => Some(&self.script),
            RtTab::Assertions => Some(&self.assertions),
        }
    }

    fn tabs_key(&mut self, chord: Chord) -> bool {
        let i = RtTab::ALL.iter().position(|t| *t == self.tab).unwrap_or(0);
        let n = RtTab::ALL.len();
        match chord.code {
            _ if chord.ctrl || chord.alt => return false,
            Code::Right | Code::Char('l') => self.tab = RtTab::ALL[(i + 1) % n],
            Code::Left | Code::Char('h') => self.tab = RtTab::ALL[(i + n - 1) % n],
            Code::Char(c @ '1'..='5') => self.tab = RtTab::ALL[usize::from(c as u8 - b'1')],
            _ => return false,
        }
        true
    }

    fn content_key(&mut self, chord: Chord, vim: Option<&mut Vim>) -> bool {
        if self.tab == RtTab::Timeline {
            return self.timeline_key(chord);
        }
        let Some(area) = self.area_mut(self.tab) else {
            return false;
        };
        match vim {
            Some(vim) => vim.key(area, chord),
            None => vim::type_key(area, chord),
        }
    }

    /// The Timeline tab: filters and the selection.
    fn timeline_key(&mut self, chord: Chord) -> bool {
        if chord.ctrl || chord.alt {
            return false;
        }
        if self.editing_filter {
            return self.filter_key(chord);
        }
        match chord.code {
            Code::Char('/') => self.editing_filter = true,
            Code::Char('d') => self.refilter(TimelineFilter::cycle_dir),
            Code::Char('t') => {
                let kinds = self.live.kinds();
                self.refilter(|f| f.cycle_kind(&kinds));
            }
            Code::Char('c') => self.refilter(|f| *f = TimelineFilter::default()),
            Code::Up | Code::Char('k') => self.move_selection(-1),
            Code::Down | Code::Char('j') => self.move_selection(1),
            Code::Home | Code::Char('g') => self.selected = Some(0),
            Code::End | Code::Char('G') => self.selected = None,
            _ => return false,
        }
        true
    }

    /// Change the filter with `change`; the view follows the newest entry.
    fn refilter(&mut self, change: impl FnOnce(&mut TimelineFilter)) {
        change(&mut self.filter);
        self.selected = None;
    }

    /// Typing the text filter; `Enter` ends it.
    fn filter_key(&mut self, chord: Chord) -> bool {
        match chord.code {
            Code::Enter | Code::Esc => self.editing_filter = false,
            Code::Backspace => {
                self.filter.text.pop();
            }
            Code::Char(c) => self.filter.text.push(c),
            _ => return false,
        }
        self.selected = None;
        true
    }

    /// Move the selection `by` rows among the shown entries.
    fn move_selection(&mut self, by: isize) {
        let shown = self.live.matching(&self.filter).len();
        if shown == 0 {
            self.selected = None;
            return;
        }
        let at = self.selected.unwrap_or(shown - 1);
        let next = at.saturating_add_signed(by).min(shown - 1);
        self.selected = Some(next);
    }
}

impl PaneEditor for RealtimeEditor {
    fn kind(&self) -> &str {
        self.base.kind()
    }

    fn tabs(&self) -> Vec<&'static str> {
        RtTab::ALL.iter().map(|t| self.tab_label(*t)).collect()
    }

    fn tab(&self) -> usize {
        RtTab::ALL.iter().position(|t| *t == self.tab).unwrap_or(0)
    }

    fn set_tab(&mut self, index: usize) {
        if let Some(tab) = RtTab::ALL.get(index) {
            self.tab = *tab;
        }
    }

    fn method(&self) -> Option<&str> {
        Some(if self.is_ws() { "WS" } else { "SSE" })
    }

    fn url(&self) -> Option<&TextArea> {
        Some(&self.url)
    }

    fn key(&mut self, focus: Focus, chord: Chord, vim: Option<&mut Vim>) -> bool {
        match focus {
            Focus::Url => match chord.code {
                Code::Enter | Code::Up | Code::Down if !chord.ctrl && !chord.alt => true,
                _ => vim::type_key(&mut self.url, chord),
            },
            Focus::EditorTabs => self.tabs_key(chord),
            Focus::EditorBody => self.content_key(chord, vim),
            _ => false,
        }
    }

    /// The Compose tab cycles the frame type: text, JSON, binary.
    fn cycle_mode(&mut self) -> bool {
        if self.tab != RtTab::Compose {
            return false;
        }
        self.mode = self.mode.next();
        true
    }

    fn request(&self) -> Result<Request, String> {
        self.to_request()
    }

    fn render(&self, frame: &mut Frame, rect: Rect, ctx: &RenderCtx) {
        realtime_view::content(frame, rect, self, ctx);
    }

    fn variable_at_cursor(&self, focus: Focus) -> Option<String> {
        let area = match focus {
            Focus::Url => &self.url,
            Focus::EditorBody => self.area(self.tab)?,
            _ => return None,
        };
        let cursor = area.cursor();
        vars_view::at(area.lines().get(cursor.row)?, cursor.col)
    }

    fn external_text(&self) -> Option<(String, &'static str)> {
        let extension = match self.tab {
            RtTab::Timeline => return None,
            RtTab::Compose if self.mode == ComposeMode::Json => "json",
            RtTab::Compose => "txt",
            _ => "yaml",
        };
        Some((self.area(self.tab)?.text(), extension))
    }

    fn set_external_text(&mut self, text: &str) {
        let text = text.strip_suffix('\n').unwrap_or(text);
        let text = text.strip_suffix('\r').unwrap_or(text);
        let tab = self.tab;
        if let Some(area) = self.area_mut(tab) {
            *area = TextArea::from_text(&text.replace("\r\n", "\n"));
        }
    }

    fn realtime(&self) -> Option<&RealtimeEditor> {
        Some(self)
    }

    fn realtime_mut(&mut self) -> Option<&mut RealtimeEditor> {
        Some(self)
    }
}

/// The member the Steps / Body tab edits.
fn script_key(kind: &str) -> &'static str {
    if kind == "ws" { "steps" } else { "body" }
}

/// The timeline cap of `request` (`limits.max_timeline`).
fn max_timeline(request: &Request) -> u32 {
    match request {
        Request::Ws(ws) => ws.effective_limits().effective_max_timeline(),
        Request::Sse(sse) => sse.effective_limits().effective_max_timeline(),
        _ => rustman_core::model::DEFAULT_MAX_TIMELINE,
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

/// The members of a ws or sse file in schema order; unknown ones last.
const ORDER: [&str; 19] = [
    "version",
    "kind",
    "name",
    "description",
    "tags",
    "method",
    "url",
    "headers",
    "auth",
    "subprotocols",
    "body",
    "last_event_id",
    "reconnect",
    "steps",
    "limits",
    "tls",
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
