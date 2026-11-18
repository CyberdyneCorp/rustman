//! Rendering the UI off-screen as plain text or ANSI-coloured text, for
//! snapshot tests and README screenshots.

use std::fmt::Write;

use ratatui::Terminal;
use ratatui::backend::TestBackend;
use ratatui::buffer::Buffer;
use ratatui::style::{Color, Modifier, Style};

use crate::app::State;
use crate::view::view;

/// Render `state` into a `width` x `height` buffer.
pub fn render(state: &State, width: u16, height: u16) -> Buffer {
    let mut terminal = Terminal::new(TestBackend::new(width, height)).expect("test backend");
    terminal
        .draw(|frame| view(state, frame))
        .expect("test backend draw");
    terminal.backend().buffer().clone()
}

/// The buffer as text, one line per row, trailing spaces trimmed.
pub fn buffer_text(buffer: &Buffer) -> String {
    rows(buffer)
        .map(|row| row.iter().map(|(symbol, _)| *symbol).collect::<String>())
        .map(|line| line.trim_end().to_owned() + "\n")
        .collect()
}

/// The buffer as text with ANSI SGR escapes for its colours and modifiers.
pub fn buffer_ansi(buffer: &Buffer) -> String {
    let mut out = String::new();
    for row in rows(buffer) {
        let mut current: Option<Style> = None;
        for (symbol, style) in row {
            if current != Some(style) {
                out.push_str(&sgr(style));
                current = Some(style);
            }
            out.push_str(symbol);
        }
        out.push_str("\x1b[0m\n");
    }
    out
}

/// The style of the cell where `needle` first appears, for tests that check
/// colours.
pub fn style_of(buffer: &Buffer, needle: &str) -> Option<Style> {
    let cells = &buffer.content;
    let matches_at = |i: usize| {
        needle.chars().enumerate().all(|(j, c)| {
            cells
                .get(i + j)
                .is_some_and(|cell| cell.symbol().chars().eq([c]))
        })
    };
    (0..cells.len())
        .find(|&i| matches_at(i))
        .map(|i| cells[i].style())
}

fn rows(buffer: &Buffer) -> impl Iterator<Item = Vec<(&str, Style)>> {
    let width = usize::from(buffer.area.width.max(1));
    buffer
        .content
        .chunks(width)
        .map(|cells| cells.iter().map(|c| (c.symbol(), c.style())).collect())
}

/// The escape sequence that resets attributes and applies `style`.
pub fn sgr(style: Style) -> String {
    let mut codes: Vec<String> = vec!["0".to_owned()];
    codes.extend(style.fg.and_then(|c| color_code(c, 30)));
    codes.extend(style.bg.and_then(|c| color_code(c, 40)));
    codes.extend(modifier_codes(style.add_modifier).map(str::to_owned));
    let mut out = String::from("\x1b[");
    let _ = write!(out, "{}m", codes.join(";"));
    out
}

/// The SGR parameter for `color`, with `base` 30 for foreground or 40 for
/// background.
fn color_code(color: Color, base: u8) -> Option<String> {
    match color {
        Color::Reset => None,
        Color::Indexed(i) => Some(format!("{};5;{i}", base + 8)),
        Color::Rgb(r, g, b) => Some(format!("{};2;{r};{g};{b}", base + 8)),
        named => named_index(named).map(|i| {
            let code = if i < 8 { base + i } else { base + 60 + i - 8 };
            code.to_string()
        }),
    }
}

/// The 0..16 ANSI palette index of a named colour.
fn named_index(color: Color) -> Option<u8> {
    const NAMED: [Color; 16] = [
        Color::Black,
        Color::Red,
        Color::Green,
        Color::Yellow,
        Color::Blue,
        Color::Magenta,
        Color::Cyan,
        Color::Gray,
        Color::DarkGray,
        Color::LightRed,
        Color::LightGreen,
        Color::LightYellow,
        Color::LightBlue,
        Color::LightMagenta,
        Color::LightCyan,
        Color::White,
    ];
    NAMED
        .iter()
        .position(|&c| c == color)
        .and_then(|i| u8::try_from(i).ok())
}

fn modifier_codes(modifier: Modifier) -> impl Iterator<Item = &'static str> {
    const CODES: [(Modifier, &str); 6] = [
        (Modifier::BOLD, "1"),
        (Modifier::DIM, "2"),
        (Modifier::ITALIC, "3"),
        (Modifier::UNDERLINED, "4"),
        (Modifier::REVERSED, "7"),
        (Modifier::CROSSED_OUT, "9"),
    ];
    CODES
        .into_iter()
        .filter(move |(m, _)| modifier.contains(*m))
        .map(|(_, code)| code)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::layout::Rect;

    #[test]
    fn colours_map_to_sgr_codes() {
        assert_eq!(color_code(Color::Red, 30).as_deref(), Some("31"));
        assert_eq!(color_code(Color::LightCyan, 40).as_deref(), Some("106"));
        assert_eq!(
            color_code(Color::Indexed(208), 30).as_deref(),
            Some("38;5;208")
        );
        assert_eq!(
            color_code(Color::Rgb(1, 2, 3), 40).as_deref(),
            Some("48;2;1;2;3")
        );
        assert_eq!(color_code(Color::Reset, 30), None);
    }

    #[test]
    fn styles_become_one_escape() {
        let style = Style::new()
            .fg(Color::Green)
            .add_modifier(Modifier::BOLD | Modifier::REVERSED);
        assert_eq!(sgr(style), "\x1b[0;32;1;7m");
    }

    #[test]
    fn text_and_ansi_cover_every_row() {
        let mut buffer = Buffer::empty(Rect::new(0, 0, 4, 2));
        buffer.set_string(0, 0, "ab", Style::new().fg(Color::Blue));
        assert_eq!(buffer_text(&buffer), "ab\n\n");
        assert_eq!(
            buffer_ansi(&buffer),
            "\x1b[0;34mab\x1b[0m  \x1b[0m\n\x1b[0m    \x1b[0m\n"
        );
    }
}
