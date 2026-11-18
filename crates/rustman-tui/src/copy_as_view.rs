//! Drawing the "Copy as…" picker and the read-only snippet pane.

use ratatui::Frame;
use ratatui::layout::{Constraint, Flex, Layout, Rect};
use ratatui::style::Modifier;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Clear, Padding, Paragraph};

use crate::config::UiOptions;
use crate::copy_as::{Picker, SELECT_HINT, SnippetPane, WARNING};
use crate::theme::Token;
use crate::view::{block, hint};

/// A centred rectangle of at most `width` x `height` near the top of `area`.
fn centred(area: Rect, width: u16, height: u16) -> Rect {
    let width = area.width.saturating_sub(4).min(width);
    let height = area.height.saturating_sub(2).min(height);
    let [column] = Layout::horizontal([Constraint::Length(width)])
        .flex(Flex::Center)
        .areas(area);
    Rect {
        y: area.y + area.height.saturating_sub(height).min(2),
        height,
        ..column
    }
}

/// Draw the picker: the query, the warning row while secrets are
/// revealed, then the languages.
pub fn picker(frame: &mut Frame, area: Rect, picker: &Picker, ui: &UiOptions) {
    let rect = centred(area, 60, 18);
    frame.render_widget(Clear, rect);
    let block = block(" Copy as… ", ui)
        .border_style(ui.theme.fg(Token::Focus))
        .padding(Padding::horizontal(1));
    let inner = block.inner(rect);
    frame.render_widget(block, rect);
    let mut lines = vec![Line::from(vec![
        Span::styled(
            format!("{} ", ui.glyphs.pointer),
            ui.theme.fg(Token::Accent),
        ),
        Span::styled(picker.query.clone(), ui.theme.fg(Token::Text)),
        Span::styled(" ", ui.theme.selection()),
    ])];
    if picker.revealed {
        let warn = ui.theme.fg(Token::Warn).add_modifier(Modifier::BOLD);
        lines.push(Line::styled(format!("{} {WARNING}", ui.glyphs.warn), warn));
    }
    lines.push(Line::default());
    let fixed = lines.len() + 2;
    let rows = usize::from(inner.height).saturating_sub(fixed);
    lines.extend(language_lines(picker, rows, ui));
    if picker.ranked.is_empty() {
        lines.push(hint("No matches.", ui));
    }
    frame.render_widget(Paragraph::new(lines), inner);
    let footer = Rect {
        y: inner.bottom().saturating_sub(1),
        height: 1.min(inner.height),
        ..inner
    };
    let g = &ui.glyphs;
    let enter = if picker.armed {
        "Enter again to copy"
    } else {
        "Enter copy"
    };
    let keys = format!("{} choose {} {enter} {} Esc close", g.up_down, g.dot, g.dot);
    frame.render_widget(Paragraph::new(hint(&keys, ui)), footer);
}

fn language_lines(picker: &Picker, rows: usize, ui: &UiOptions) -> Vec<Line<'static>> {
    let skip = (picker.selected + 1).saturating_sub(rows);
    picker
        .visible()
        .enumerate()
        .skip(skip)
        .take(rows)
        .map(|(i, entry)| {
            let style = if i == picker.selected {
                ui.theme.selection().add_modifier(Modifier::BOLD)
            } else {
                ui.theme.fg(Token::Text)
            };
            Line::styled(format!(" {}", entry.label), style)
        })
        .collect()
}

/// Draw the read-only snippet pane, for selecting the text by hand.
pub fn pane(frame: &mut Frame, area: Rect, pane: &SnippetPane, ui: &UiOptions) {
    let rect = centred(area, 100, area.height);
    frame.render_widget(Clear, rect);
    let title = format!(" {} snippet ", pane.lang);
    let block = block(&title, ui).border_style(ui.theme.fg(Token::Focus));
    let inner = block.inner(rect);
    frame.render_widget(block, rect);
    let [body, footer] = Layout::vertical([Constraint::Min(0), Constraint::Length(1)]).areas(inner);
    let text = Paragraph::new(pane.code.clone())
        .style(ui.theme.fg(Token::Text))
        .scroll((pane.scroll, 0));
    frame.render_widget(text, body);
    let g = &ui.glyphs;
    let keys = format!(
        "{SELECT_HINT} {} {} scroll {} Esc close",
        g.dot, g.up_down, g.dot
    );
    frame.render_widget(Paragraph::new(hint(&keys, ui)), footer);
}
