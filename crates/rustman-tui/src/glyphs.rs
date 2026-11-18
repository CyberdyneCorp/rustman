//! The glyph sets: Unicode by default, plain ASCII with `--ascii` or
//! `borders = "ascii"` (spec: ASCII fallback).

use ratatui::symbols::border;

/// Every non-letter symbol the UI draws.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Glyphs {
    pub border: border::Set<'static>,
    /// Between status-bar fields.
    pub separator: &'static str,
    /// In front of the selected item of a list.
    pub pointer: &'static str,
    pub ellipsis: &'static str,
    pub up_down: &'static str,
    pub dot: &'static str,
    pub tree_open: &'static str,
    pub tree_closed: &'static str,
    pub spinner: &'static [&'static str],
    pub sparkline: &'static [&'static str],
    pub ok: &'static str,
    pub fail: &'static str,
    pub info: &'static str,
    pub warn: &'static str,
    /// The unsaved-changes marker on tabs and sidebar rows.
    pub dirty: &'static str,
    /// How a secret value is shown.
    pub mask: &'static str,
}

pub const UNICODE: Glyphs = Glyphs {
    border: border::PLAIN,
    separator: "│",
    pointer: "›",
    ellipsis: "…",
    up_down: "↑↓",
    dot: "·",
    tree_open: "▾",
    tree_closed: "▸",
    spinner: &["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"],
    sparkline: &["▁", "▂", "▃", "▄", "▅", "▆", "▇", "█"],
    ok: "✔",
    fail: "✖",
    info: "ℹ",
    warn: "⚠",
    dirty: "●",
    mask: "••••••••",
};

pub const ASCII: Glyphs = Glyphs {
    border: border::Set {
        top_left: "+",
        top_right: "+",
        bottom_left: "+",
        bottom_right: "+",
        vertical_left: "|",
        vertical_right: "|",
        horizontal_top: "-",
        horizontal_bottom: "-",
    },
    separator: "|",
    pointer: ">",
    ellipsis: "...",
    up_down: "Up/Down",
    dot: "-",
    tree_open: "v",
    tree_closed: ">",
    spinner: &["|", "/", "-", "\\"],
    sparkline: &["_", ".", "-", "=", "+", "*", "#", "@"],
    ok: "+",
    fail: "x",
    info: "i",
    warn: "!",
    dirty: "*",
    mask: "********",
};

#[cfg(test)]
mod tests {
    use super::*;

    fn strings(g: &Glyphs) -> Vec<&'static str> {
        let b = g.border;
        let mut all = vec![
            b.top_left,
            b.top_right,
            b.bottom_left,
            b.bottom_right,
            b.vertical_left,
            b.vertical_right,
            b.horizontal_top,
            b.horizontal_bottom,
            g.separator,
            g.pointer,
            g.ellipsis,
            g.up_down,
            g.dot,
            g.tree_open,
            g.tree_closed,
            g.ok,
            g.fail,
            g.info,
            g.warn,
            g.dirty,
            g.mask,
        ];
        all.extend(g.spinner);
        all.extend(g.sparkline);
        all
    }

    #[test]
    fn the_ascii_set_is_ascii_and_borders_use_plus_minus_bar() {
        assert!(strings(&ASCII).iter().all(|s| s.is_ascii()));
        let b = ASCII.border;
        let used: String = [b.top_left, b.horizontal_top, b.vertical_left].concat();
        assert_eq!(used, "+-|");
    }

    #[test]
    fn both_sets_fill_every_glyph() {
        for set in [UNICODE, ASCII] {
            assert!(strings(&set).iter().all(|s| !s.is_empty()));
            assert_eq!(set.sparkline.len(), 8);
        }
    }
}
