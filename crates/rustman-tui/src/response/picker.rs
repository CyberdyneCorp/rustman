//! The exchange diff overlays (spec: Diff two exchanges): `D` loads earlier
//! exchanges from `request-history` (off the render loop), a picker lists
//! them with the same request first, and the diff against the current
//! exchange is shown.

use std::path::Path;

use ratatui::Frame;
use ratatui::style::Modifier;
use ratatui::text::{Line, Span};
use rustman_core::history::{Entry, HistoryStore};
use rustman_core::run::HeaderRecord;
use serde_json::Value;

use super::Received;
use super::diff::{Change, Side, diff};
use crate::config::UiOptions;
use crate::keymap::chord::{Chord, Code};
use crate::theme::Token;
use crate::view::{dialog, hint};

/// How many earlier exchanges the picker lists.
const PICK_LIMIT: usize = 50;

/// An earlier exchange from history.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Earlier {
    pub label: String,
    /// Of the same request file as the current exchange.
    pub same: bool,
    pub side: Side,
}

/// Earlier exchanges in the history at `dir`: those of `file` first (newest
/// first), then the others. Reads the disk: run it off the render loop.
pub fn load(dir: &Path, file: &str) -> Result<Vec<Earlier>, String> {
    let store = HistoryStore::at(dir, usize::MAX);
    let listing = store.list().map_err(|e| e.to_string())?;
    let (mut same, other): (Vec<&Entry>, Vec<&Entry>) =
        listing.entries.iter().partition(|e| e.request == file);
    same.extend(other);
    Ok(same
        .into_iter()
        .take(PICK_LIMIT)
        .map(|e| Earlier {
            label: label(e),
            same: e.request == file,
            side: side_of(&e.record),
        })
        .collect())
}

fn label(entry: &Entry) -> String {
    let status = entry
        .status
        .map_or_else(|| "no response".to_owned(), |s| s.to_string());
    let time = entry.timestamp.replace('T', " ");
    let time = time.get(..19).unwrap_or(&time);
    format!("{time}  {status:<11} {}", entry.request)
}

/// The compared parts of a recorded request record (JSON).
fn side_of(record: &Value) -> Side {
    let response = record.pointer("/exchange/response");
    let headers = response
        .and_then(|r| r.get("headers"))
        .and_then(Value::as_array)
        .map(|list| {
            list.iter()
                .filter_map(|h| {
                    Some(HeaderRecord {
                        name: h.get("name")?.as_str()?.to_owned(),
                        value: h.get("value")?.as_str()?.to_owned(),
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    Side {
        status: response
            .and_then(|r| r.get("status"))
            .and_then(Value::as_u64)
            .and_then(|s| u16::try_from(s).ok()),
        headers,
        body: response
            .and_then(|r| r.get("body"))
            .and_then(Value::as_str)
            .map(str::to_owned),
    }
}

/// The current exchange as the diff compares it.
pub fn current(received: &Received) -> Side {
    let response = received.record.exchange.as_ref().map(|e| &e.response);
    Side {
        status: response.map(|r| r.status),
        headers: response.map(|r| r.headers.clone()).unwrap_or_default(),
        body: response.and_then(|r| r.body.clone()),
    }
}

/// The picker of earlier exchanges.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Picker {
    pub items: Vec<Earlier>,
    pub selected: usize,
    pub current: Side,
    /// A diff is being computed off the render loop.
    pub pending: bool,
}

/// What a key did in the picker or the diff.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Picked {
    Stay,
    Close,
    /// Compute this diff off the render loop (bodies can be megabytes).
    Compute(Box<DiffJob>),
}

/// A diff to compute: an earlier exchange against the current one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiffJob {
    pub title: String,
    pub earlier: Side,
    pub current: Side,
}

impl Picker {
    pub fn key(&mut self, chord: Chord) -> Picked {
        let last = self.items.len().saturating_sub(1);
        match chord.code {
            Code::Esc => return Picked::Close,
            Code::Up | Code::Char('k') => self.selected = self.selected.saturating_sub(1),
            Code::Down | Code::Char('j') => self.selected = (self.selected + 1).min(last),
            Code::Enter if !self.pending => {
                if let Some(earlier) = self.items.get(self.selected) {
                    self.pending = true;
                    return Picked::Compute(Box::new(DiffJob {
                        title: format!(" Diff: {} → current ", earlier.label.trim()),
                        earlier: earlier.side.clone(),
                        current: self.current.clone(),
                    }));
                }
            }
            _ => {}
        }
        Picked::Stay
    }

    pub fn render(&self, frame: &mut Frame, ui: &UiOptions) {
        let mut lines = Vec::new();
        if self.items.is_empty() {
            lines.push(hint("No earlier exchanges in history.", ui));
        }
        let room = usize::from(frame.area().height.saturating_sub(6)).max(1);
        let start = (self.selected + 1).saturating_sub(room);
        for (i, item) in self.items.iter().enumerate().skip(start).take(room) {
            let token = if item.same { Token::Text } else { Token::Muted };
            let line = Line::styled(item.label.clone(), ui.theme.fg(token));
            lines.push(if i == self.selected {
                line.style(ui.theme.selection())
            } else {
                line
            });
        }
        let footer = if self.pending {
            format!("Diffing{}", ui.glyphs.ellipsis)
        } else {
            format!(
                "Enter diff {} Esc close (this request first)",
                ui.glyphs.dot
            )
        };
        lines.extend([Line::default(), hint(&footer, ui)]);
        dialog(frame, frame.area(), " Diff with… ", lines, ui);
    }
}

/// The differences between an earlier exchange and the current one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiffView {
    pub title: String,
    pub changes: Vec<Change>,
    pub scroll: usize,
}

impl DiffView {
    /// Compute the diff of `job`. This parses and walks both bodies: run it
    /// off the render loop.
    pub fn compute(job: &DiffJob) -> Self {
        Self {
            title: job.title.clone(),
            changes: diff(&job.earlier, &job.current),
            scroll: 0,
        }
    }

    pub fn key(&mut self, chord: Chord) -> Picked {
        match chord.code {
            Code::Esc | Code::Char('q') => return Picked::Close,
            Code::Up | Code::Char('k') => self.scroll = self.scroll.saturating_sub(1),
            Code::Down | Code::Char('j') => {
                self.scroll = (self.scroll + 1).min(self.changes.len().saturating_sub(1));
            }
            _ => {}
        }
        Picked::Stay
    }

    pub fn render(&self, frame: &mut Frame, ui: &UiOptions) {
        let mut lines: Vec<Line<'static>> = Vec::new();
        if self.changes.is_empty() {
            lines.push(hint("No differences.", ui));
        }
        let room = usize::from(frame.area().height.saturating_sub(6)).max(1);
        for change in self.changes.iter().skip(self.scroll).take(room) {
            let token = match change {
                Change::Changed { .. } => Token::Warn,
                Change::Added(_) => Token::Ok,
                Change::Removed(_) => Token::Error,
            };
            lines.push(Line::from(Span::styled(
                change.text(),
                ui.theme.fg(token).add_modifier(Modifier::BOLD),
            )));
        }
        lines.extend([Line::default(), hint("Esc close", ui)]);
        dialog(frame, frame.area(), &self.title, lines, ui);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::keymap::chord::parse_chord;
    use serde_json::json;

    fn earlier(body: &str) -> Earlier {
        Earlier {
            label: "2026-11-18 10:00:00  200 a.rman.yaml".into(),
            same: true,
            side: side_of(&json!({"exchange": {"response": {
                "status": 200,
                "headers": [{"name": "Content-Type", "value": "application/json"}],
                "body": body,
            }}})),
        }
    }

    #[test]
    fn picking_an_exchange_shows_the_structural_diff() {
        // Scenario "Structural JSON diff", through the picker.
        let current = earlier(r#"{"b":3,"a":1}"#).side;
        let mut picker = Picker {
            items: vec![earlier(r#"{"a":1,"b":2}"#), earlier("{}")],
            selected: 0,
            current,
            pending: false,
        };
        assert_eq!(picker.key(parse_chord("Down").unwrap()), Picked::Stay);
        assert_eq!(picker.key(parse_chord("Up").unwrap()), Picked::Stay);
        let Picked::Compute(job) = picker.key(parse_chord("Enter").unwrap()) else {
            panic!("expected a diff job");
        };
        // The diff is an effect: a second Enter while it runs does nothing.
        assert!(picker.pending);
        assert_eq!(picker.key(parse_chord("Enter").unwrap()), Picked::Stay);
        let view = DiffView::compute(&job);
        let lines: Vec<String> = view.changes.iter().map(Change::text).collect();
        assert_eq!(lines, ["$.b: 2 → 3"]);
        assert_eq!(picker.key(parse_chord("Esc").unwrap()), Picked::Close);
    }

    #[test]
    fn history_entries_load_same_request_first() {
        let tmp = tempfile::tempdir().unwrap();
        let store = HistoryStore::at(tmp.path(), 100);
        for file in ["a.rman.yaml", "b.rman.yaml", "a.rman.yaml"] {
            let mut record = super::super::received::fixtures::record(200, &[("X", "1")], 2);
            record.file = file.into();
            let entry = Entry::new(rustman_core::history::NewEntry {
                record: &record,
                sent: record.exchange.as_ref().unwrap().request.clone(),
                request: Some(file),
                environment: None,
                spill: None,
                replay: None,
            });
            store.record(&entry).unwrap();
        }
        let items = load(tmp.path(), "b.rman.yaml").unwrap();
        let same: Vec<bool> = items.iter().map(|i| i.same).collect();
        assert_eq!(same, [true, false, false]);
        assert_eq!(items[0].side.status, Some(200));
        assert_eq!(items[0].side.headers[0].name, "X");
        assert!(items[0].label.ends_with("b.rman.yaml"));
    }
}
