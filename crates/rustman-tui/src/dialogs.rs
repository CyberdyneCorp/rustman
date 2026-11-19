//! Modal prompts: choices (Save / Discard / Cancel, delete confirmation,
//! the external edit conflict) and one-line text input (new request,
//! rename, save as).

use std::path::PathBuf;

use ratatui::Frame;
use ratatui::style::Modifier;
use ratatui::text::{Line, Span};

use crate::config::UiOptions;
use crate::keymap::chord::{Chord, Code};
use crate::tabs::TabId;
use crate::textarea::TextArea;
use crate::theme::Token;
use crate::view::{dialog, hint};
use crate::vim;

/// What a choice does.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Decision {
    SaveAndClose(TabId),
    DiscardAndClose(TabId),
    SaveAllAndQuit,
    DiscardAndQuit,
    /// Delete this workspace-relative file or folder.
    Delete(PathBuf),
    KeepMine(TabId),
    LoadFromDisk(TabId),
    /// Start the webhook listener on a network-reachable address.
    WebhookStart,
    /// Start the mock server on a network-reachable address.
    MockStart,
    /// Send the tab's GraphQL request without checking its document.
    SendUnvalidated(TabId),
    Cancel,
}

/// A question with a few answers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Confirm {
    pub title: String,
    pub message: String,
    pub choices: Vec<(&'static str, Decision)>,
    pub selected: usize,
}

impl Confirm {
    /// Save / Discard / Cancel before closing a dirty tab.
    pub fn close_tab(id: TabId, title: &str) -> Self {
        Self {
            title: " Unsaved changes ".into(),
            message: format!("{title} has unsaved changes."),
            choices: vec![
                ("Save", Decision::SaveAndClose(id)),
                ("Discard", Decision::DiscardAndClose(id)),
                ("Cancel", Decision::Cancel),
            ],
            selected: 0,
        }
    }

    /// Save / Discard / Cancel before quitting with dirty tabs.
    pub fn quit(dirty: usize) -> Self {
        let what = if dirty == 1 {
            "1 tab has".to_owned()
        } else {
            format!("{dirty} tabs have")
        };
        Self {
            title: " Unsaved changes ".into(),
            message: format!("{what} unsaved changes."),
            choices: vec![
                ("Save", Decision::SaveAllAndQuit),
                ("Discard", Decision::DiscardAndQuit),
                ("Cancel", Decision::Cancel),
            ],
            selected: 0,
        }
    }

    /// Confirm deleting a request or folder (spec: Delete requires
    /// confirmation). Cancel is preselected.
    pub fn delete(path: PathBuf, shown: &str) -> Self {
        Self {
            title: " Delete ".into(),
            message: format!("Delete {shown}? This cannot be undone."),
            choices: vec![
                ("Delete", Decision::Delete(path)),
                ("Cancel", Decision::Cancel),
            ],
            selected: 1,
        }
    }

    /// The file changed on disk while the tab had unsaved changes.
    pub fn conflict(id: TabId, shown: &str) -> Self {
        Self {
            title: " Changed on disk ".into(),
            message: format!("{shown} was changed by another program."),
            choices: vec![
                ("Keep mine", Decision::KeepMine(id)),
                ("Load from disk", Decision::LoadFromDisk(id)),
            ],
            selected: 0,
        }
    }

    /// Start the webhook listener on an address reachable from the network
    /// (spec: Non-loopback bind warning). Cancel is preselected.
    pub fn webhook_remote(warning: &str) -> Self {
        Self {
            title: " Webhook listener ".into(),
            message: format!("Start anyway? {warning}."),
            choices: vec![
                ("Start", Decision::WebhookStart),
                ("Cancel", Decision::Cancel),
            ],
            selected: 1,
        }
    }

    /// Start the mock server on an address reachable from the network
    /// (spec: Loopback by default). Cancel is preselected.
    pub fn mock_remote(warning: &str) -> Self {
        Self {
            title: " Mock server ".into(),
            message: format!("Start anyway? {warning}."),
            choices: vec![("Start", Decision::MockStart), ("Cancel", Decision::Cancel)],
            selected: 1,
        }
    }

    /// The GraphQL document has errors: send it anyway? Cancel is
    /// preselected (spec: Pre-send validation).
    pub fn invalid_graphql(id: TabId, error: &str) -> Self {
        Self {
            title: " Invalid GraphQL ".into(),
            message: format!("The document has errors: {error}"),
            choices: vec![
                ("Send anyway", Decision::SendUnvalidated(id)),
                ("Cancel", Decision::Cancel),
            ],
            selected: 1,
        }
    }

    /// Move the selection, or return the chosen decision.
    pub fn key(&mut self, chord: Chord) -> Option<Decision> {
        let n = self.choices.len();
        match chord.code {
            Code::Left | Code::Up | Code::BackTab => self.selected = (self.selected + n - 1) % n,
            Code::Right | Code::Down | Code::Tab => self.selected = (self.selected + 1) % n,
            Code::Enter => return Some(self.choices[self.selected].1.clone()),
            Code::Esc => return Some(self.escape()),
            Code::Char(c) if !chord.ctrl && !chord.alt => return self.by_letter(c),
            _ => {}
        }
        None
    }

    /// `Esc` cancels, or keeps the user's version in a conflict.
    fn escape(&self) -> Decision {
        let cancel = self.choices.iter().find(|(_, d)| *d == Decision::Cancel);
        cancel.map_or_else(|| self.choices[0].1.clone(), |(_, d)| d.clone())
    }

    fn by_letter(&self, c: char) -> Option<Decision> {
        let c = c.to_ascii_lowercase();
        self.choices
            .iter()
            .find(|(label, _)| label.to_ascii_lowercase().starts_with(c))
            .map(|(_, d)| d.clone())
    }

    pub fn render(&self, frame: &mut Frame, ui: &UiOptions) {
        let mut spans = Vec::new();
        for (i, (label, _)) in self.choices.iter().enumerate() {
            let style = if i == self.selected {
                ui.theme.selection().add_modifier(Modifier::BOLD)
            } else {
                ui.theme.fg(Token::Text)
            };
            spans.push(Span::styled(format!(" {label} "), style));
            spans.push(Span::raw("  "));
        }
        let g = &ui.glyphs;
        let keys = format!(
            "{} choose {} Enter confirm {} Esc cancel",
            "Left/Right", g.dot, g.dot
        );
        let lines = vec![
            Line::from(self.message.clone()),
            Line::default(),
            Line::from(spans),
            Line::default(),
            hint(&keys, ui),
        ];
        dialog(frame, frame.area(), &self.title, lines, ui);
    }
}

/// What a text prompt is for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Purpose {
    /// A new request in this workspace-relative folder.
    NewRequest { folder: PathBuf },
    /// Rename this workspace-relative file or folder.
    Rename { from: PathBuf },
    /// Save a new tab under a name.
    SaveAs { tab: TabId },
    /// Save a tab's response body to a file.
    SaveBody { tab: TabId },
    /// A webhook pane prompt (replay or save a capture).
    Webhook(crate::webhook::Prompt),
    /// A mock server pane prompt (the document to serve).
    Mock(crate::mock_server::Prompt),
    /// An MCP inspector prompt (save the last call).
    Mcp(crate::mcp::Prompt),
}

/// A one-line text prompt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Input {
    pub title: String,
    pub label: String,
    pub purpose: Purpose,
    pub text: TextArea,
    pub error: Option<String>,
}

/// What a key did to a prompt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Typed {
    Editing,
    Submit(String),
    Cancel,
}

impl Input {
    pub fn new(title: &str, label: &str, purpose: Purpose, initial: &str) -> Self {
        let mut text = TextArea::from_text(initial);
        text.line_end(true);
        Self {
            title: format!(" {title} "),
            label: label.to_owned(),
            purpose,
            text,
            error: None,
        }
    }

    pub fn key(&mut self, chord: Chord) -> Typed {
        match chord.code {
            Code::Enter => Typed::Submit(self.text.text()),
            Code::Esc => Typed::Cancel,
            Code::Up | Code::Down => Typed::Editing,
            _ => {
                vim::type_key(&mut self.text, chord);
                self.error = None;
                Typed::Editing
            }
        }
    }

    pub fn render(&self, frame: &mut Frame, ui: &UiOptions) {
        let text_style = ui.theme.fg(Token::Text);
        let cursor = self.text.cursor().col;
        let value = self.text.text();
        let before: String = value.chars().take(cursor).collect();
        let at: String = value.chars().nth(cursor).map_or(" ".into(), String::from);
        let after: String = value.chars().skip(cursor + 1).collect();
        let mut lines = vec![
            Line::from(self.label.clone()),
            Line::from(vec![
                Span::styled(format!("{} {before}", ui.glyphs.pointer), text_style),
                Span::styled(at, text_style.add_modifier(Modifier::REVERSED)),
                Span::styled(after, text_style),
            ]),
        ];
        if let Some(error) = &self.error {
            lines.push(Line::styled(error.clone(), ui.theme.fg(Token::Error)));
        }
        let g = &ui.glyphs;
        lines.extend([
            Line::default(),
            hint(&format!("Enter confirm {} Esc cancel", g.dot), ui),
        ]);
        dialog(frame, frame.area(), &self.title, lines, ui);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::keymap::chord::parse_chord;

    fn key(text: &str) -> Chord {
        parse_chord(text).unwrap()
    }

    #[test]
    fn choices_move_and_confirm() {
        let mut confirm = Confirm::close_tab(7, "get-user");
        assert_eq!(confirm.key(key("Right")), None);
        assert_eq!(
            confirm.key(key("Enter")),
            Some(Decision::DiscardAndClose(7))
        );
        assert_eq!(confirm.key(key("s")), Some(Decision::SaveAndClose(7)));
        assert_eq!(confirm.key(key("Esc")), Some(Decision::Cancel));
        let mut conflict = Confirm::conflict(3, "get-user");
        assert_eq!(conflict.key(key("l")), Some(Decision::LoadFromDisk(3)));
        assert_eq!(conflict.key(key("Esc")), Some(Decision::KeepMine(3)));
        let mut delete = Confirm::delete("a".into(), "a");
        assert_eq!(
            delete.key(key("Enter")),
            Some(Decision::Cancel),
            "cancel is preselected"
        );
        assert_eq!(Confirm::quit(2).message, "2 tabs have unsaved changes.");
    }

    #[test]
    fn input_edits_and_submits() {
        let mut input = Input::new(
            "New request",
            "Name:",
            Purpose::NewRequest { folder: "".into() },
            "ab",
        );
        assert_eq!(input.key(key("Backspace")), Typed::Editing);
        assert_eq!(input.key(key("c")), Typed::Editing);
        assert_eq!(input.key(key("Enter")), Typed::Submit("ac".into()));
        assert_eq!(input.key(key("Esc")), Typed::Cancel);
    }
}
