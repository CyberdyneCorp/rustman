//! The protocol pane extension point (design D8, spec: Protocol pane
//! extension point).
//!
//! A [`RequestPane`] is registered per request `kind` and opens a
//! [`PaneEditor`] for a request file: the editor's tabs, how each one is
//! drawn and edited, how edits turn back into a request file, and the
//! response tabs. A kind with no registered pane opens as a read-only raw
//! view. Later changes register their panes in [`Registry::builtin`].

use std::fmt;

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::text::Line;
use ratatui::widgets::{Paragraph, Wrap};
use rustman_core::model::Request;

use crate::config::UiOptions;
use crate::focus::Focus;
use crate::graphql_pane::{GraphqlEditor, GraphqlPane};
use crate::grpc_pane::{GrpcEditor, GrpcPane};
use crate::http_pane::HttpPane;
use crate::jsonrpc_pane::{JsonrpcEditor, JsonrpcPane};
use crate::keymap::{Chord, Keymap};
use crate::realtime_pane::{RealtimeEditor, RealtimePane};
use crate::theme::Token;
use crate::vars_view::VarScope;
use crate::vim::Vim;

/// What the editor content is drawn with.
pub struct RenderCtx<'a> {
    pub ui: &'a UiOptions,
    /// Where the keyboard focus is (the content draws its cursor only when
    /// it has it).
    pub focus: Focus,
    pub vars: &'a VarScope<'a>,
    /// The effective keymap, for the keys hints name.
    pub keymap: &'a Keymap,
}

/// The editing state of one open request.
pub trait PaneEditor: fmt::Debug + Send {
    /// The request kind, e.g. `http`.
    fn kind(&self) -> &str;
    /// The editor tab labels, e.g. Params, Headers, Body.
    fn tabs(&self) -> Vec<&'static str>;
    fn tab(&self) -> usize;
    fn set_tab(&mut self, index: usize);
    /// The method shown left of the URL; `None` when there is no URL row.
    fn method(&self) -> Option<&str>;
    /// The URL as typed, query included.
    fn url(&self) -> Option<&crate::textarea::TextArea>;
    /// Handle a key for the focused editor element; false when it is not
    /// an editing key there.
    fn key(&mut self, focus: Focus, chord: Chord, vim: Option<&mut Vim>) -> bool;
    /// Cycle the mode of the current tab (body mode, auth type).
    fn cycle_mode(&mut self) -> bool;
    /// The request as edited; `Err` explains what does not parse.
    fn request(&self) -> Result<Request, String>;
    /// Draw the content of the current tab.
    fn render(&self, frame: &mut Frame, rect: Rect, ctx: &RenderCtx);
    /// A notice shown above the content (e.g. why it is read-only).
    fn notice(&self) -> Option<String> {
        None
    }
    /// The `{{variable}}` under the cursor of the focused element.
    fn variable_at_cursor(&self, focus: Focus) -> Option<String>;
    /// The text `Ctrl+E` opens in an external editor, with a file extension
    /// for it (`json`, `xml`, `txt`, ...).
    fn external_text(&self) -> Option<(String, &'static str)> {
        None
    }
    /// Replace the text `external_text` returned.
    fn set_external_text(&mut self, _text: &str) {}
    /// The response viewer tabs for this kind.
    fn response_tabs(&self) -> &'static [&'static str] {
        &[]
    }
    /// The GraphQL editor, for its schema, completion and subscriptions.
    fn graphql(&self) -> Option<&GraphqlEditor> {
        None
    }
    fn graphql_mut(&mut self) -> Option<&mut GraphqlEditor> {
        None
    }
    /// The JSON-RPC editor, for its catalog, completion and subscriptions.
    fn jsonrpc(&self) -> Option<&JsonrpcEditor> {
        None
    }
    fn jsonrpc_mut(&mut self) -> Option<&mut JsonrpcEditor> {
        None
    }
    /// The gRPC editor, for its schema, method browser and streams.
    fn grpc(&self) -> Option<&GrpcEditor> {
        None
    }
    fn grpc_mut(&mut self) -> Option<&mut GrpcEditor> {
        None
    }
    /// The realtime editor, for its live session.
    fn realtime(&self) -> Option<&RealtimeEditor> {
        None
    }
    fn realtime_mut(&mut self) -> Option<&mut RealtimeEditor> {
        None
    }
}

/// A pane registered for one request kind.
pub trait RequestPane: Sync {
    fn kind(&self) -> &'static str;
    /// The editor for `request`, which has this pane's kind.
    fn open(&self, request: &Request) -> Option<Box<dyn PaneEditor>>;
}

/// The panes known to this build, keyed by kind.
pub struct Registry {
    panes: Vec<&'static dyn RequestPane>,
}

impl Registry {
    /// The panes shipped here: `http`, `graphql`, `jsonrpc`, `grpc`, and
    /// the realtime pane for `ws` and `sse`.
    pub fn builtin() -> Self {
        Self {
            panes: vec![
                &HttpPane,
                &GraphqlPane,
                &JsonrpcPane,
                &GrpcPane,
                &RealtimePane::WS,
                &RealtimePane::SSE,
            ],
        }
    }

    pub fn kinds(&self) -> Vec<&'static str> {
        self.panes.iter().map(|p| p.kind()).collect()
    }

    /// The editor for `request`: its kind's pane, else a read-only view.
    pub fn open(&self, request: &Request) -> Box<dyn PaneEditor> {
        self.panes
            .iter()
            .find(|p| p.kind() == request.kind())
            .and_then(|p| p.open(request))
            .unwrap_or_else(|| Box::new(ReadOnly::new(request)))
    }
}

/// The raw view of a request whose kind has no pane.
#[derive(Debug, Clone)]
pub struct ReadOnly {
    request: Request,
    text: String,
    pub scroll: usize,
}

impl ReadOnly {
    pub fn new(request: &Request) -> Self {
        Self {
            request: request.clone(),
            text: rustman_core::format::write::request_to_yaml(request),
            scroll: 0,
        }
    }
}

impl PaneEditor for ReadOnly {
    fn kind(&self) -> &str {
        self.request.kind()
    }

    fn tabs(&self) -> Vec<&'static str> {
        vec!["Raw"]
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
                let last = self.text.lines().count().saturating_sub(1);
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
        let style = ctx.ui.theme.fg(Token::Text);
        let lines: Vec<Line> = self
            .text
            .lines()
            .skip(self.scroll)
            .map(|l| Line::styled(format!(" {l}"), style))
            .collect();
        frame.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), rect);
    }

    fn notice(&self) -> Option<String> {
        Some(format!("No editor for kind \"{}\"", self.request.kind()))
    }

    fn variable_at_cursor(&self, _focus: Focus) -> Option<String> {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rustman_core::model::KindRegistry;
    use std::path::Path;

    fn load(text: &str) -> Request {
        KindRegistry::default()
            .with_opaque_fallback()
            .load(Path::new("r.rman.yaml"), text)
            .unwrap()
    }

    #[test]
    fn http_has_a_pane_and_unknown_kinds_open_read_only() {
        let registry = Registry::builtin();
        assert_eq!(
            registry.kinds(),
            ["http", "graphql", "jsonrpc", "grpc", "ws", "sse"]
        );
        let http = load("version: 1\nkind: http\nname: A\nurl: http://x/\n");
        let editor = registry.open(&http);
        assert_eq!(editor.kind(), "http");
        assert!(editor.notice().is_none());
        assert_eq!(
            editor.tabs(),
            ["Params", "Headers", "Body", "Auth", "Assertions"]
        );

        let text = "version: 1\nkind: soap\nname: Hello\nservice: Greeter\n";
        let mut editor = registry.open(&load(text));
        assert_eq!(
            editor.notice().as_deref(),
            Some("No editor for kind \"soap\"")
        );
        assert_eq!(editor.method(), None);
        assert_eq!(editor.request().unwrap(), load(text), "never changed");
        let typed = crate::keymap::chord::parse_chord("a").unwrap();
        assert!(!editor.key(Focus::EditorBody, typed, None), "read-only");
        assert!(!editor.cycle_mode());
        assert_eq!(editor.external_text(), None);
    }
}
