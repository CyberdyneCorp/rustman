//! The help overlay (spec: Help overlay): the bindings of the focused
//! context and the global ones, generated from the effective keymap so user
//! overrides show up.

use ratatui::Frame;
use ratatui::layout::{Constraint, Flex, Layout, Rect};
use ratatui::style::Modifier;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Clear, Padding, Paragraph};

use crate::config::UiOptions;
use crate::keymap::{Context, Keymap, chord};
use crate::theme::Token;
use crate::view::{block, hint};
use crate::vim;

/// The widest key column before it is cut.
const KEY_COLUMN: usize = 24;

/// The help text for `context`: its section (unless global), then global,
/// then the vim text keys when `vim_text` is set.
pub fn lines(
    keymap: &Keymap,
    context: Context,
    vim_text: bool,
    ui: &UiOptions,
) -> Vec<Line<'static>> {
    let mut sections: Vec<(&str, Vec<(String, String)>)> = Vec::new();
    let contexts = if context == Context::Global {
        &[Context::Global][..]
    } else {
        &[context, Context::Global][..]
    };
    for &ctx in contexts {
        let rows = keymap
            .bindings_in(ctx)
            .into_iter()
            .map(|(action, keys)| {
                let keys: Vec<String> = keys.iter().map(|k| chord::display(k)).collect();
                (keys.join(", "), action.title().to_owned())
            })
            .collect();
        sections.push((ctx.title(), rows));
    }
    if vim_text {
        let rows = vim::KEYS
            .iter()
            .map(|(keys, what)| ((*keys).to_owned(), (*what).to_owned()))
            .collect();
        sections.push(("Vim text editing", rows));
    }
    let mut out = Vec::new();
    for (title, rows) in sections.into_iter().filter(|(_, rows)| !rows.is_empty()) {
        if !out.is_empty() {
            out.push(Line::default());
        }
        let heading = ui.theme.fg(Token::Accent).add_modifier(Modifier::BOLD);
        out.push(Line::from(Span::styled(title.to_owned(), heading)));
        out.extend(rows.into_iter().map(|(keys, what)| row(&keys, what, ui)));
    }
    out
}

fn row(keys: &str, what: String, ui: &UiOptions) -> Line<'static> {
    let mut keys: String = keys.chars().take(KEY_COLUMN).collect();
    let pad = KEY_COLUMN.saturating_sub(keys.chars().count());
    keys.push_str(&" ".repeat(pad));
    Line::from(vec![
        Span::styled(
            format!("  {keys} "),
            ui.theme.fg(Token::Text).add_modifier(Modifier::BOLD),
        ),
        Span::styled(what, ui.theme.fg(Token::Muted)),
    ])
}

/// Draw the overlay with `lines` scrolled by `scroll` rows (clamped).
pub fn render(
    frame: &mut Frame,
    area: Rect,
    lines: Vec<Line<'static>>,
    scroll: u16,
    ui: &UiOptions,
) {
    let width = area.width.saturating_sub(4).min(72);
    let wanted = u16::try_from(lines.len() + 4).unwrap_or(u16::MAX);
    let height = wanted.min(area.height.saturating_sub(2));
    let [row] = Layout::vertical([Constraint::Length(height)])
        .flex(Flex::Center)
        .areas(area);
    let [rect] = Layout::horizontal([Constraint::Length(width)])
        .flex(Flex::Center)
        .areas(row);
    frame.render_widget(Clear, rect);
    let block = block(" Help ", ui)
        .border_style(ui.theme.fg(Token::Focus))
        .padding(Padding::horizontal(1));
    let inner = block.inner(rect);
    frame.render_widget(block, rect);
    let body_height = inner.height.saturating_sub(2);
    let max_scroll = u16::try_from(lines.len())
        .unwrap_or(u16::MAX)
        .saturating_sub(body_height);
    let body = Rect {
        height: body_height,
        ..inner
    };
    frame.render_widget(
        Paragraph::new(lines).scroll((scroll.min(max_scroll), 0)),
        body,
    );
    let footer = Rect {
        y: inner.bottom().saturating_sub(1),
        height: 1.min(inner.height),
        ..inner
    };
    let g = &ui.glyphs;
    let keys = format!("{} scroll {} Esc close", g.up_down, g.dot);
    frame.render_widget(Paragraph::new(hint(&keys, ui)), footer);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::keymap::UserLayer;

    fn text(lines: &[Line<'_>]) -> String {
        lines
            .iter()
            .map(|l| {
                l.spans
                    .iter()
                    .map(|s| s.content.as_ref())
                    .collect::<String>()
                    + "\n"
            })
            .collect()
    }

    #[test]
    fn the_focused_context_comes_before_global() {
        let ui = UiOptions::default();
        let text = text(&lines(&Keymap::default(), Context::Response, false, &ui));
        let response = text.find("Response viewer").unwrap();
        let global = text.find("Global").unwrap();
        assert!(response < global, "{text}");
        assert!(text.contains("Open in viz companion"));
        assert!(!text.contains("Vim text editing"));
        assert!(
            !text.contains("Edit in external editor"),
            "other contexts are left out"
        );
    }

    #[test]
    fn global_focus_lists_global_once_and_vim_adds_its_keys() {
        let ui = UiOptions::default();
        let text = text(&lines(
            &Keymap::new(true, &UserLayer::default()),
            Context::Global,
            true,
            &ui,
        ));
        assert_eq!(text.matches("Global").count(), 1, "{text}");
        assert!(text.contains("Vim text editing"));
        assert!(text.contains("dd"));
    }
}
