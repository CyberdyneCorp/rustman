//! The selected capture's body as display lines, built once per capture
//! and body mode and reused by every frame, and the scroll position of the
//! detail view. Only the rows on screen are drawn.

use std::cell::{Cell, RefCell};
use std::sync::Arc;

use rustman_core::Redactor;
use rustman_webhook::Capture;
use rustman_webhook::body::{BodyView, hex_dump, view as body_view};

use super::BodyMode;

/// Longer lines are cut into pieces of this many characters, so wrapping
/// one huge line (minified JSON) stays cheap.
const MAX_LINE_CHARS: usize = 2000;

/// Rows a page key moves before the first frame told the real height.
const DEFAULT_PAGE: usize = 10;

/// The body lines of the last capture and mode drawn.
#[derive(Debug, Default)]
pub struct BodyCache {
    cached: RefCell<Option<Cached>>,
    /// How many times lines were built (a cache miss).
    builds: Cell<usize>,
}

#[derive(Debug)]
struct Cached {
    /// Held so the pointer comparison can never match a newer capture.
    capture: Arc<Capture>,
    mode: BodyMode,
    lines: Arc<[String]>,
}

impl BodyCache {
    /// The body of `capture` in `mode`, redacted, one entry per row.
    pub fn lines(
        &self,
        capture: &Arc<Capture>,
        mode: BodyMode,
        redactor: &Redactor,
    ) -> Arc<[String]> {
        let mut cached = self.cached.borrow_mut();
        if let Some(hit) = cached
            .as_ref()
            .filter(|c| c.mode == mode && Arc::ptr_eq(&c.capture, capture))
        {
            return Arc::clone(&hit.lines);
        }
        let lines: Arc<[String]> = body_lines(capture, mode, redactor).into();
        self.builds.set(self.builds.get() + 1);
        *cached = Some(Cached {
            capture: Arc::clone(capture),
            mode,
            lines: Arc::clone(&lines),
        });
        lines
    }

    /// Forget the lines (the redactor changed).
    pub fn reset(&self) {
        self.cached.replace(None);
    }

    #[cfg(test)]
    pub fn builds(&self) -> usize {
        self.builds.get()
    }
}

/// Where the detail view is scrolled to.
#[derive(Debug, Default)]
pub struct Scroll {
    /// The first row shown.
    pub top: usize,
    /// Rows the last frame showed; a page.
    pub page: Cell<usize>,
    /// The last first row the last frame allowed.
    pub max_top: Cell<Option<usize>>,
}

impl Scroll {
    pub fn page_down(&mut self) {
        let top = self.top.saturating_add(self.page_rows());
        self.top = self.max_top.get().map_or(top, |max| top.min(max));
    }

    pub fn page_up(&mut self) {
        self.top = self.top.saturating_sub(self.page_rows());
    }

    fn page_rows(&self) -> usize {
        match self.page.get() {
            0 => DEFAULT_PAGE,
            rows => rows,
        }
    }

    /// The first row to draw out of `total` in `rows`, keeping the last
    /// page full; remembered for the page keys.
    pub fn first(&self, total: usize, rows: usize) -> usize {
        let max = total.saturating_sub(rows);
        self.page.set(rows);
        self.max_top.set(Some(max));
        self.top.min(max)
    }
}

/// The body as the mode shows it, redacted, cut into rows. Bytes that are
/// not UTF-8 are only ever shown as hex.
fn body_lines(capture: &Capture, mode: BodyMode, redactor: &Redactor) -> Vec<String> {
    let text = body_text(capture, mode, redactor);
    let mut lines = Vec::new();
    for line in text.lines() {
        push_cut(&mut lines, line);
    }
    lines
}

/// `line` as rows of at most [`MAX_LINE_CHARS`] characters.
fn push_cut(lines: &mut Vec<String>, line: &str) {
    let mut rest = line;
    loop {
        let cut = rest
            .char_indices()
            .nth(MAX_LINE_CHARS)
            .map_or(rest.len(), |(i, _)| i);
        let (row, tail) = rest.split_at(cut);
        lines.push(row.to_owned());
        if tail.is_empty() {
            return;
        }
        rest = tail;
    }
}

fn body_text(capture: &Capture, mode: BodyMode, redactor: &Redactor) -> String {
    let body = redactor.bytes(&capture.body);
    let content_type = capture.content_type();
    match (mode, std::str::from_utf8(&body)) {
        (BodyMode::Hex, _) | (_, Err(_)) => hex_dump(&body),
        (BodyMode::Text, Ok(text)) => text.to_owned(),
        (BodyMode::Json, Ok(text)) => serde_json::from_str::<serde_json::Value>(text)
            .ok()
            .and_then(|v| serde_json::to_string_pretty(&v).ok())
            .unwrap_or_else(|| format!("(not JSON)\n{text}")),
        (BodyMode::Auto, Ok(_)) => auto_text(body_view(content_type.as_deref(), &body)),
    }
}

fn auto_text(view: BodyView) -> String {
    match view {
        BodyView::Empty => "(empty body)".to_owned(),
        BodyView::Json(text) | BodyView::Text(text) | BodyView::Hex(text) => text,
        BodyView::Form(fields) => fields
            .iter()
            .map(|(name, value)| format!("{name} = {value}"))
            .collect::<Vec<_>>()
            .join("\n"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn long_lines_are_cut_on_character_boundaries() {
        let mut lines = Vec::new();
        let line = "é".repeat(MAX_LINE_CHARS * 2 + 3);
        push_cut(&mut lines, &line);
        let counts: Vec<_> = lines.iter().map(|l| l.chars().count()).collect();
        assert_eq!(counts, [MAX_LINE_CHARS, MAX_LINE_CHARS, 3]);
        assert_eq!(lines.concat(), line);
    }

    #[test]
    fn an_empty_line_stays_one_row() {
        let mut lines = Vec::new();
        push_cut(&mut lines, "");
        assert_eq!(lines, [""]);
    }

    #[test]
    fn scrolling_stops_at_the_last_page() {
        let mut scroll = Scroll::default();
        scroll.page_down();
        assert_eq!(scroll.top, DEFAULT_PAGE, "before the first frame");
        assert_eq!(scroll.first(100, 30), DEFAULT_PAGE);
        scroll.page_down();
        assert_eq!(scroll.first(100, 30), 40);
        scroll.page_down();
        scroll.page_down();
        assert_eq!(scroll.top, 70, "the last page stays full");
        scroll.page_up();
        assert_eq!(scroll.first(100, 30), 40, "one page up from the end");
        assert_eq!(scroll.first(5, 30), 0);
        scroll.page_up();
        scroll.page_up();
        assert_eq!(scroll.top, 0);
    }
}
