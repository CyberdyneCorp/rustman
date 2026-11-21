//! The messaging pane for `kind: kafka`, `kind: mqtt` and `kind: amqp`
//! requests (add-messaging-client "Kind registration", "TUI messaging
//! pane"): the broker address on the URL row, and the tabs Messages (the
//! live consumer list with pause, each value rendered as JSON, decoded
//! from the Schema Registry, text or hex), Compose (the messages a produce
//! sends), Broker (the destination and connection settings) and
//! Assertions, each that part of the file as YAML.
//!
//! `request.send` runs the operation; a consume runs until `Esc` stops it
//! or its buffer of 10000 messages is full. An unedited request keeps its
//! file text; once edited, the file is rebuilt from the loaded document
//! with the edited parts in place, in schema order, and parsed again.

use std::path::Path;

use ratatui::Frame;
use ratatui::layout::Rect;
use rustman_core::format::from_yaml;
use rustman_core::format::write::{Node, json_node, render};
use rustman_core::model::{MessagingRequest, Operation, Request};
use serde_json::{Map, Value};

use crate::focus::Focus;
use crate::keymap::chord::{Chord, Code};
use crate::messaging::Consumer;
use crate::messaging_view;
use crate::pane::{PaneEditor, RenderCtx, RequestPane};
use crate::textarea::TextArea;
use crate::vars_view;
use crate::vim::{self, Vim};

/// The editor tabs, in order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MsgTab {
    Messages,
    Compose,
    Broker,
    Assertions,
}

impl MsgTab {
    pub const ALL: [Self; 4] = [
        Self::Messages,
        Self::Compose,
        Self::Broker,
        Self::Assertions,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::Messages => "Messages",
            Self::Compose => "Compose",
            Self::Broker => "Broker",
            Self::Assertions => "Assertions",
        }
    }
}

/// The members the Broker tab edits: the destination and the connection.
const BROKER_KEYS: [&str; 24] = [
    "topic",
    "partition",
    "partitions",
    "offset",
    "acks",
    "exchange",
    "routing_key",
    "binding_key",
    "queue",
    "mandatory",
    "destructive",
    "ack",
    "requeue",
    "protocol_version",
    "qos",
    "retain",
    "client_id",
    "clean_session",
    "properties",
    "security",
    "auth",
    "tls",
    "schema_registry",
    "limits",
];

/// The messaging pane, registered once per kind.
pub struct MessagingPane {
    kind: &'static str,
}

impl MessagingPane {
    pub const KAFKA: Self = Self { kind: "kafka" };
    pub const MQTT: Self = Self { kind: "mqtt" };
    pub const AMQP: Self = Self { kind: "amqp" };
}

impl RequestPane for MessagingPane {
    fn kind(&self) -> &'static str {
        self.kind
    }

    fn open(&self, request: &Request) -> Option<Box<dyn PaneEditor>> {
        match request {
            Request::Messaging(m) => Some(Box::new(MessagingEditor::new(request, m))),
            _ => None,
        }
    }
}

/// Editing state of a messaging request, and its live consumer.
pub struct MessagingEditor {
    base: Request,
    /// The loaded file as a document, for the members the pane leaves alone.
    doc: Map<String, Value>,
    loaded: [String; 4],
    /// `brokers` (Kafka) or `broker`.
    pub broker: TextArea,
    pub compose: TextArea,
    pub settings: TextArea,
    pub assertions: TextArea,
    pub tab: MsgTab,
    pub operation: Operation,
    /// The selected message; `None` follows the newest.
    pub selected: Option<usize>,
    /// The consume of the last send.
    pub consumer: Consumer,
}

impl std::fmt::Debug for MessagingEditor {
    /// Settings and messages (which may hold credentials) are left out.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MessagingEditor")
            .field("kind", &self.base.kind())
            .field("tab", &self.tab)
            .finish_non_exhaustive()
    }
}

impl MessagingEditor {
    pub fn new(request: &Request, messaging: &MessagingRequest) -> Self {
        let doc = match from_yaml::<Value>(Path::new("request"), &messaging.source) {
            Ok(Value::Object(map)) => map,
            _ => Map::new(),
        };
        let yaml = |key: &str| doc.get(key).map(yaml_text).unwrap_or_default();
        let settings: Map<String, Value> = BROKER_KEYS
            .iter()
            .filter_map(|k| doc.get(*k).map(|v| ((*k).to_owned(), v.clone())))
            .collect();
        let settings = if settings.is_empty() {
            String::new()
        } else {
            yaml_text(&Value::Object(settings))
        };
        let mut editor = Self {
            loaded: Default::default(),
            broker: TextArea::from_text(&broker_text(&doc)),
            compose: TextArea::from_text(&yaml("messages")),
            settings: TextArea::from_text(&settings),
            assertions: TextArea::from_text(&yaml("assertions")),
            tab: MsgTab::Messages,
            operation: messaging.effective_operation(),
            selected: None,
            consumer: Consumer::default(),
            base: request.clone(),
            doc,
        };
        editor.loaded = editor.texts();
        editor
    }

    fn texts(&self) -> [String; 4] {
        [
            &self.broker,
            &self.compose,
            &self.settings,
            &self.assertions,
        ]
        .map(TextArea::text)
    }

    /// The broker member: `brokers` for Kafka, `broker` otherwise.
    fn broker_key(&self) -> &'static str {
        if self.base.kind() == "kafka" {
            "brokers"
        } else {
            "broker"
        }
    }

    /// The request as edited.
    pub fn to_request(&self) -> Result<Request, String> {
        if self.texts() == self.loaded {
            return Ok(self.base.clone());
        }
        let mut doc = self.doc.clone();
        doc.insert(self.broker_key().into(), Value::String(self.broker.text()));
        for key in BROKER_KEYS {
            doc.remove(key);
        }
        if let Some(Value::Object(settings)) = yaml_value("Broker", &self.settings.text())? {
            doc.extend(settings);
        }
        for (key, label, area) in [
            ("messages", "Compose", &self.compose),
            ("assertions", "Assertions", &self.assertions),
        ] {
            match yaml_value(label, &area.text())? {
                Some(value) => doc.insert(key.to_owned(), value),
                None => doc.remove(key),
            };
        }
        let text = render(&ordered(&doc));
        MessagingRequest::parse(Path::new("request"), &text)
            .map(|r| Request::Messaging(Box::new(r)))
            .map_err(|e| e.to_string())
    }

    pub fn area(&self, tab: MsgTab) -> Option<&TextArea> {
        match tab {
            MsgTab::Messages => None,
            MsgTab::Compose => Some(&self.compose),
            MsgTab::Broker => Some(&self.settings),
            MsgTab::Assertions => Some(&self.assertions),
        }
    }

    fn area_mut(&mut self, tab: MsgTab) -> Option<&mut TextArea> {
        match tab {
            MsgTab::Messages => None,
            MsgTab::Compose => Some(&mut self.compose),
            MsgTab::Broker => Some(&mut self.settings),
            MsgTab::Assertions => Some(&mut self.assertions),
        }
    }

    fn tabs_key(&mut self, chord: Chord) -> bool {
        let i = MsgTab::ALL.iter().position(|t| *t == self.tab).unwrap_or(0);
        let n = MsgTab::ALL.len();
        match chord.code {
            _ if chord.ctrl || chord.alt => return false,
            Code::Right | Code::Char('l') => self.tab = MsgTab::ALL[(i + 1) % n],
            Code::Left | Code::Char('h') => self.tab = MsgTab::ALL[(i + n - 1) % n],
            Code::Char(c @ '1'..='4') => self.tab = MsgTab::ALL[usize::from(c as u8 - b'1')],
            _ => return false,
        }
        true
    }

    fn content_key(&mut self, chord: Chord, vim: Option<&mut Vim>) -> bool {
        if self.tab == MsgTab::Messages {
            return self.messages_key(chord);
        }
        let Some(area) = self.area_mut(self.tab) else {
            return false;
        };
        match vim {
            Some(vim) => vim.key(area, chord),
            None => vim::type_key(area, chord),
        }
    }

    /// The Messages tab: the selection and the pause.
    fn messages_key(&mut self, chord: Chord) -> bool {
        if chord.ctrl || chord.alt {
            return false;
        }
        match chord.code {
            Code::Char('p') | Code::Char(' ') => self.consumer.toggle_pause(),
            Code::Up | Code::Char('k') => self.move_selection(-1),
            Code::Down | Code::Char('j') => self.move_selection(1),
            Code::Home | Code::Char('g') => self.selected = Some(0),
            Code::End | Code::Char('G') => self.selected = None,
            _ => return false,
        }
        true
    }

    /// Move the selection `by` rows among the shown messages.
    fn move_selection(&mut self, by: isize) {
        let shown = self.consumer.shown();
        if shown == 0 {
            self.selected = None;
            return;
        }
        let at = self.selected.unwrap_or(shown - 1);
        self.selected = Some(at.saturating_add_signed(by).min(shown - 1));
    }
}

impl PaneEditor for MessagingEditor {
    fn kind(&self) -> &str {
        self.base.kind()
    }

    fn tabs(&self) -> Vec<&'static str> {
        MsgTab::ALL.iter().map(|t| t.label()).collect()
    }

    fn tab(&self) -> usize {
        MsgTab::ALL.iter().position(|t| *t == self.tab).unwrap_or(0)
    }

    fn set_tab(&mut self, index: usize) {
        if let Some(tab) = MsgTab::ALL.get(index) {
            self.tab = *tab;
        }
    }

    fn method(&self) -> Option<&str> {
        Some(match self.operation {
            Operation::Produce => "PRODUCE",
            Operation::Consume => "CONSUME",
        })
    }

    fn url(&self) -> Option<&TextArea> {
        Some(&self.broker)
    }

    fn key(&mut self, focus: Focus, chord: Chord, vim: Option<&mut Vim>) -> bool {
        match focus {
            Focus::Url => match chord.code {
                Code::Enter | Code::Up | Code::Down if !chord.ctrl && !chord.alt => true,
                _ => vim::type_key(&mut self.broker, chord),
            },
            Focus::EditorTabs => self.tabs_key(chord),
            Focus::EditorBody => self.content_key(chord, vim),
            _ => false,
        }
    }

    fn cycle_mode(&mut self) -> bool {
        false
    }

    fn request(&self) -> Result<Request, String> {
        self.to_request()
    }

    fn render(&self, frame: &mut Frame, rect: Rect, ctx: &RenderCtx) {
        messaging_view::content(frame, rect, self, ctx);
    }

    fn variable_at_cursor(&self, focus: Focus) -> Option<String> {
        let area = match focus {
            Focus::Url => &self.broker,
            Focus::EditorBody => self.area(self.tab)?,
            _ => return None,
        };
        let cursor = area.cursor();
        vars_view::at(area.lines().get(cursor.row)?, cursor.col)
    }

    fn external_text(&self) -> Option<(String, &'static str)> {
        Some((self.area(self.tab)?.text(), "yaml"))
    }

    fn set_external_text(&mut self, text: &str) {
        let text = text.strip_suffix('\n').unwrap_or(text);
        let text = text.strip_suffix('\r').unwrap_or(text);
        let tab = self.tab;
        if let Some(area) = self.area_mut(tab) {
            *area = TextArea::from_text(&text.replace("\r\n", "\n"));
        }
    }

    fn messaging(&self) -> Option<&MessagingEditor> {
        Some(self)
    }

    fn messaging_mut(&mut self) -> Option<&mut MessagingEditor> {
        Some(self)
    }
}

/// The broker address as typed: `broker`, else `brokers` (a list joined
/// with commas).
fn broker_text(doc: &Map<String, Value>) -> String {
    match doc.get("broker").or_else(|| doc.get("brokers")) {
        Some(Value::String(text)) => text.clone(),
        Some(Value::Array(list)) => list
            .iter()
            .filter_map(Value::as_str)
            .collect::<Vec<_>>()
            .join(","),
        _ => String::new(),
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

/// The members of a messaging file in schema order; unknown ones last.
const ORDER: [&str; 12] = [
    "version",
    "kind",
    "name",
    "description",
    "tags",
    "docs",
    "operation",
    "brokers",
    "broker",
    "messages",
    "variables",
    "assertions",
];

/// `doc` as a YAML node with its members in [`ORDER`], the Broker members
/// after the broker and `start`, `asyncapi` and `captures` last.
fn ordered(doc: &Map<String, Value>) -> Node {
    let rank = |key: &str| {
        let broker = ORDER.iter().position(|k| *k == "broker").unwrap_or(0);
        match ORDER.iter().position(|k| *k == key) {
            Some(at) if at > broker => at + BROKER_KEYS.len(),
            Some(at) => at,
            None => match BROKER_KEYS.iter().position(|k| *k == key) {
                Some(at) => broker + 1 + at,
                None => ORDER.len() + BROKER_KEYS.len(),
            },
        }
    };
    let mut members: Vec<(&String, &Value)> = doc.iter().collect();
    members.sort_by_key(|(key, _)| rank(key));
    Node::Map(
        members
            .into_iter()
            .map(|(key, value)| (key.clone(), json_node(value)))
            .collect(),
    )
}
