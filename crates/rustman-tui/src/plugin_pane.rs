//! The declarative pane of a plugin request kind (plugins spec, "Request
//! kind extension point"; design D7). The plugin describes its fields (with
//! an editor type) and its response views; the TUI draws them with its own
//! widgets. Plugins never draw. The fields are shown from the request file,
//! which is where they are edited.

use std::path::Path;

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Paragraph, Wrap};
use rustman_core::model::Request;
use rustman_core::plugins::{FieldEditor, PaneView, PluginKind};
use serde_json::Value;

use crate::focus::Focus;
use crate::keymap::Chord;
use crate::pane::{PaneEditor, RenderCtx};
use crate::theme::Token;
use crate::vim::Vim;

/// A request of a plugin kind, shown through the kind's pane.
#[derive(Debug, Clone)]
pub struct PluginKindEditor {
    request: Request,
    kind: PluginKind,
    /// The request file's fields, as JSON.
    fields: Value,
    pub scroll: usize,
}

impl PluginKindEditor {
    pub fn new(request: &Request, kind: &PluginKind) -> Self {
        let fields = match request {
            Request::Opaque(opaque) => opaque
                .kind_fields(Path::new("request"))
                .unwrap_or(Value::Null),
            _ => Value::Null,
        };
        Self {
            request: request.clone(),
            kind: kind.clone(),
            fields,
            scroll: 0,
        }
    }

    /// The pane as text lines: each field with its editor, then the
    /// response views.
    pub fn lines(&self) -> Vec<(String, String)> {
        let mut lines = Vec::new();
        for field in &self.kind.pane.fields {
            let value = self.fields.get(&field.name);
            lines.push((field.label.clone(), editor_hint(&field.editor)));
            for line in value_lines(&field.editor, value) {
                lines.push((String::new(), line));
            }
        }
        let views: Vec<&str> = self
            .kind
            .pane
            .views
            .iter()
            .map(|v| view_label(*v))
            .collect();
        lines.push(("Response views".to_owned(), views.join(", ")));
        lines
    }
}

fn editor_hint(editor: &FieldEditor) -> String {
    match editor {
        FieldEditor::Text => "text".to_owned(),
        FieldEditor::Code(language) => format!("code ({language})"),
        FieldEditor::KeyValue => "key-value".to_owned(),
        FieldEditor::Select(options) => format!("one of: {}", options.join(", ")),
    }
}

fn view_label(view: PaneView) -> &'static str {
    match view {
        PaneView::Raw => "Raw",
        PaneView::JsonTree => "JSON tree",
        PaneView::Table => "Table",
    }
}

/// How a field's value is shown: text as is (code line by line),
/// key-value maps one pair per line, anything else as JSON.
fn value_lines(editor: &FieldEditor, value: Option<&Value>) -> Vec<String> {
    let Some(value) = value else {
        return vec!["(not set)".to_owned()];
    };
    match (editor, value) {
        (_, Value::String(text)) => text.lines().map(str::to_owned).collect(),
        (FieldEditor::KeyValue, Value::Object(map)) => map
            .iter()
            .map(|(k, v)| {
                format!(
                    "{k}: {}",
                    v.as_str().map_or_else(|| v.to_string(), str::to_owned)
                )
            })
            .collect(),
        (_, other) => vec![other.to_string()],
    }
}

impl PaneEditor for PluginKindEditor {
    fn kind(&self) -> &str {
        &self.kind.name
    }

    fn tabs(&self) -> Vec<&'static str> {
        vec!["Fields"]
    }

    fn tab(&self) -> usize {
        0
    }

    fn set_tab(&mut self, _index: usize) {}

    fn method(&self) -> Option<&str> {
        None
    }

    fn url(&self) -> Option<&crate::textarea::TextArea> {
        None
    }

    fn key(&mut self, focus: Focus, chord: Chord, _vim: Option<&mut Vim>) -> bool {
        use crate::keymap::chord::Code;
        if focus != Focus::EditorBody || chord.ctrl || chord.alt {
            return false;
        }
        match chord.code {
            Code::Up | Code::Char('k') => self.scroll = self.scroll.saturating_sub(1),
            Code::Down | Code::Char('j') => {
                let last = self.lines().len().saturating_sub(1);
                self.scroll = (self.scroll + 1).min(last);
            }
            _ => return false,
        }
        true
    }

    fn cycle_mode(&mut self) -> bool {
        false
    }

    fn request(&self) -> Result<Request, String> {
        Ok(self.request.clone())
    }

    fn render(&self, frame: &mut Frame, rect: Rect, ctx: &RenderCtx) {
        let label = ctx.ui.theme.fg(Token::Accent);
        let text = ctx.ui.theme.fg(Token::Text);
        let dim = ctx.ui.theme.fg(Token::Muted);
        let lines: Vec<Line> = self
            .lines()
            .into_iter()
            .skip(self.scroll)
            .map(|(name, value)| {
                if name.is_empty() {
                    Line::styled(format!("   {value}"), text)
                } else {
                    Line::from(vec![
                        Span::styled(format!(" {name}"), label),
                        Span::styled(format!("  {value}"), dim),
                    ])
                }
            })
            .collect();
        frame.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), rect);
    }

    fn notice(&self) -> Option<String> {
        Some(format!(
            "Kind \"{}\" from plugin {}",
            self.kind.name, self.kind.plugin
        ))
    }

    fn variable_at_cursor(&self, _focus: Focus) -> Option<String> {
        None
    }
}
