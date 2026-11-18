//! Rendering: `view(&State, &mut Frame)` only reads the state (design D2).

use ratatui::Frame;
use ratatui::layout::{Constraint, Flex, Layout, Rect};
use ratatui::style::Modifier;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Clear, Padding, Paragraph, Wrap};

use crate::app::{Choice, Prompt, Screen, State};
use crate::config::UiOptions;
use crate::keymap::Keymap;
use crate::keymap::actions::ActionId;
use crate::shell_view;
use crate::theme::Token;

/// Draw `state` into `frame`.
pub fn view(state: &State, frame: &mut Frame) {
    let area = frame.area();
    let ui = &state.ui;
    match &state.screen {
        Screen::Loading(root) => {
            let text = format!("Loading workspace {}{}", root.display(), ui.glyphs.ellipsis);
            dialog(frame, area, " Rustman ", vec![Line::from(text)], ui);
        }
        Screen::Prompt(prompt) => {
            let lines = prompt_lines(prompt, &quit_key(&state.keymap), ui);
            dialog(frame, area, " No workspace found ", lines, ui);
        }
        Screen::Shell(session) => shell_view::shell(frame, state, session),
        Screen::Failed(message) => {
            let mut lines: Vec<Line> = message.lines().map(|l| Line::from(l.to_owned())).collect();
            let quit = format!("{} quit", quit_key(&state.keymap));
            lines.extend([Line::default(), hint(&quit, ui)]);
            dialog(frame, area, " Workspace error ", lines, ui);
        }
    }
    shell_view::overlay(frame, state);
    shell_view::toasts(frame, state);
}

fn prompt_lines(prompt: &Prompt, quit: &str, ui: &UiOptions) -> Vec<Line<'static>> {
    let mut lines = vec![
        Line::from("No rustman.toml in this directory or any parent:"),
        Line::from(format!("  {}", prompt.dir.display())),
        Line::default(),
    ];
    lines.extend(
        Choice::ALL
            .iter()
            .map(|&c| choice_line(c, c == prompt.selected, ui)),
    );
    if let Some(error) = &prompt.error {
        let error = Span::styled(format!("Error: {error}"), ui.theme.fg(Token::Error));
        lines.extend([Line::default(), Line::from(error)]);
    }
    let g = &ui.glyphs;
    let keys = format!(
        "{} choose {} Enter confirm {} {quit} quit",
        g.up_down, g.dot, g.dot
    );
    lines.extend([Line::default(), hint(&keys, ui)]);
    lines
}

fn choice_line(choice: Choice, selected: bool, ui: &UiOptions) -> Line<'static> {
    if selected {
        let style = ui.theme.selection().add_modifier(Modifier::BOLD);
        Line::from(Span::styled(
            format!("{} {}", ui.glyphs.pointer, choice.label()),
            style,
        ))
    } else {
        Line::from(format!("  {}", choice.label()))
    }
}

/// The key that quits, for hints. The keymap always has one.
pub(crate) fn quit_key(keymap: &Keymap) -> String {
    keymap
        .key(ActionId::Quit)
        .unwrap_or_else(|| "Ctrl+Q".to_owned())
}

/// `press Ctrl+Enter to send`, with the first key of `action` in the
/// effective keymap; `None` when the user left it unbound.
pub(crate) fn press(keymap: &Keymap, action: ActionId, to: &str) -> Option<String> {
    keymap.key(action).map(|key| format!("press {key} to {to}"))
}

/// An empty state and what to do about it, `No rows · press a to add`;
/// just `what` without a `next` step.
pub(crate) fn empty_hint(what: &str, next: Option<String>, ui: &UiOptions) -> String {
    match next {
        Some(next) => format!("{what} {} {next}", ui.glyphs.dot),
        None => what.to_owned(),
    }
}

pub(crate) fn hint(text: &str, ui: &UiOptions) -> Line<'static> {
    Line::from(Span::styled(text.to_owned(), ui.theme.fg(Token::Muted)))
}

/// A bordered block in the UI's glyph set and border colour.
pub(crate) fn block(title: &str, ui: &UiOptions) -> Block<'static> {
    Block::bordered()
        .border_set(ui.glyphs.border)
        .border_style(ui.theme.fg(Token::Border))
        .title(title.to_owned())
}

/// Columns the dialog border and padding take: two borders, two pads.
const DIALOG_FRAME: u16 = 4;

/// A bordered box centred in `area`, sized to its lines after wrapping.
pub(crate) fn dialog(
    frame: &mut Frame,
    area: Rect,
    title: &str,
    lines: Vec<Line<'static>>,
    ui: &UiOptions,
) {
    let width = area.width.min(66);
    let paragraph = Paragraph::new(lines).wrap(Wrap { trim: false });
    let rows = paragraph.line_count(width.saturating_sub(DIALOG_FRAME));
    let height = u16::try_from(rows + 2).unwrap_or(u16::MAX).min(area.height);
    let [row] = Layout::vertical([Constraint::Length(height)])
        .flex(Flex::Center)
        .areas(area);
    let [rect] = Layout::horizontal([Constraint::Length(width)])
        .flex(Flex::Center)
        .areas(row);
    frame.render_widget(Clear, rect);
    let block = block(title, ui).padding(Padding::horizontal(1));
    frame.render_widget(paragraph.block(block), rect);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::launch::Launch;
    use crate::snapshot::{buffer_text, render};

    fn prompt_state() -> State {
        State::new(Launch::NoWorkspace("/home/dev/api".into())).0
    }

    #[test]
    fn no_workspace_prompt() {
        let text = buffer_text(&render(&prompt_state(), 80, 24));
        assert!(text.contains("Create workspace here") && text.contains("Scratch session"));
        insta::assert_snapshot!(text);
    }

    #[test]
    fn create_error_is_shown_in_the_prompt() {
        let mut prompt = Prompt::new("/ro".into());
        prompt.error = Some("permission denied".into());
        let state = State::with_screen(Screen::Prompt(prompt));
        let text = buffer_text(&render(&state, 80, 24));
        assert!(text.contains("Error: permission denied"), "{text}");
    }

    #[test]
    fn a_long_path_wraps_without_clipping_the_key_hint() {
        let deep = format!("/{}", ["deeply-nested-directory"; 6].join("/"));
        let mut prompt = Prompt::new(deep.into());
        prompt.error = Some("permission denied".into());
        let state = State::with_screen(Screen::Prompt(prompt));
        let text = buffer_text(&render(&state, 80, 24));
        assert!(text.contains("Error: permission denied"), "{text}");
        assert!(text.contains("Ctrl+Q quit"), "{text}");
        let bottom = text.lines().position(|l| l.contains('└')).unwrap();
        let hint = text
            .lines()
            .position(|l| l.contains("Ctrl+Q quit"))
            .unwrap();
        assert_eq!(hint + 1, bottom, "the hint is the last row:\n{text}");
    }

    #[test]
    fn loading_and_failure_screens() {
        let state = State::with_screen(Screen::Loading("/p".into()));
        assert!(buffer_text(&render(&state, 80, 24)).contains("Loading workspace /p…"));
        let state = State::with_screen(Screen::Failed("a.rman.yaml: bad".into()));
        assert!(buffer_text(&render(&state, 80, 24)).contains("a.rman.yaml: bad"));
    }

    #[test]
    fn tiny_terminals_do_not_panic() {
        for (w, h) in [(1, 1), (10, 3), (0, 0)] {
            render(&prompt_state(), w, h);
        }
    }
}
