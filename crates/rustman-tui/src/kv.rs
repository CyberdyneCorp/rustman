//! Editable key/value tables with per-row enable toggles: the Params,
//! Headers and form-body editors (spec: HTTP request editor tabs).
//!
//! Keys while a row is selected: `Enter` edits the name (then `Tab` moves to
//! the value and `Enter` commits, `Esc` cancels), `Space` toggles the row,
//! `a`/`Insert` adds a row, `x`/`Delete` removes it, `Up`/`Down` move.

use rustman_core::model::NameValue;

use crate::keymap::chord::{Chord, Code};
use crate::textarea::TextArea;
use crate::vim;

/// The key that adds a row. Tables handle their keys before the keymap,
/// so it is fixed; hints name it from here.
pub const ADD_KEY: char = 'a';

/// One row of a table.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Row {
    pub name: String,
    pub value: String,
    pub enabled: bool,
}

impl Row {
    pub fn new(name: impl Into<String>, value: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            value: value.into(),
            enabled: true,
        }
    }
}

/// Which cell of the selected row is being edited.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Cell {
    Name,
    Value,
}

/// A cell being edited: its text before and its live text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CellEdit {
    pub cell: Cell,
    pub text: TextArea,
    /// The row was added by this edit and goes away if it is cancelled.
    pub added: bool,
    /// The cell's text before the edit, restored by `Esc`.
    pub original: String,
}

/// What a key did to the table.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    /// Not a table key: let the keymap have it.
    Ignored,
    /// Handled without changing the rows (moving, starting an edit).
    Moved,
    /// The rows changed.
    Changed,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct KvTable {
    pub rows: Vec<Row>,
    pub selected: usize,
    pub edit: Option<CellEdit>,
    /// Names are fixed (the Auth form): no adding, removing or toggling.
    pub fixed: bool,
}

impl KvTable {
    pub fn from_pairs(pairs: &[NameValue]) -> Self {
        let rows = pairs
            .iter()
            .map(|p| Row {
                name: p.name.clone(),
                value: p.value.clone(),
                enabled: p.enabled,
            })
            .collect();
        Self {
            rows,
            ..Self::default()
        }
    }

    /// A form with fixed field names.
    pub fn fixed(rows: Vec<Row>) -> Self {
        Self {
            rows,
            fixed: true,
            ..Self::default()
        }
    }

    /// Every row with a name, disabled ones included.
    pub fn to_pairs(&self) -> Vec<NameValue> {
        self.rows
            .iter()
            .filter(|r| !r.name.is_empty())
            .map(|r| NameValue::new(&r.name, &r.value).enabled(r.enabled))
            .collect()
    }

    /// The value of the row named `name`, for fixed forms.
    pub fn value(&self, name: &str) -> &str {
        self.rows
            .iter()
            .find(|r| r.name == name)
            .map_or("", |r| r.value.as_str())
    }

    /// Add an enabled row at the end.
    pub fn push(&mut self, name: &str, value: &str) {
        self.rows.push(Row::new(name, value));
        self.selected = self.rows.len() - 1;
    }

    /// Handle a key.
    pub fn key(&mut self, chord: Chord) -> Outcome {
        if self.edit.is_some() {
            return self.edit_key(chord);
        }
        if chord.ctrl || chord.alt {
            return Outcome::Ignored;
        }
        match chord.code {
            Code::Up | Code::Char('k') => self.select(self.selected.saturating_sub(1)),
            Code::Down | Code::Char('j') => self.select(self.selected + 1),
            Code::Enter | Code::Char('e') => self.start(Cell::Name, false),
            Code::Char(' ') if !self.fixed => self.toggle(),
            Code::Char(ADD_KEY) | Code::Insert if !self.fixed => self.add(),
            Code::Char('x') | Code::Delete if !self.fixed => self.remove(),
            _ => Outcome::Ignored,
        }
    }

    fn select(&mut self, index: usize) -> Outcome {
        self.selected = index.min(self.rows.len().saturating_sub(1));
        Outcome::Moved
    }

    fn start(&mut self, cell: Cell, added: bool) -> Outcome {
        let Some(row) = self.rows.get(self.selected) else {
            return if self.fixed {
                Outcome::Moved
            } else {
                self.add()
            };
        };
        // A fixed form edits values only.
        let cell = if self.fixed { Cell::Value } else { cell };
        let text = match cell {
            Cell::Name => &row.name,
            Cell::Value => &row.value,
        };
        let mut area = TextArea::from_text(text);
        area.line_end(true);
        self.edit = Some(CellEdit {
            cell,
            original: text.clone(),
            text: area,
            added,
        });
        Outcome::Moved
    }

    fn toggle(&mut self) -> Outcome {
        match self.rows.get_mut(self.selected) {
            Some(row) => {
                row.enabled = !row.enabled;
                Outcome::Changed
            }
            None => Outcome::Moved,
        }
    }

    fn add(&mut self) -> Outcome {
        self.rows.push(Row::new("", ""));
        self.selected = self.rows.len() - 1;
        self.start(Cell::Name, true);
        Outcome::Changed
    }

    fn remove(&mut self) -> Outcome {
        if self.selected >= self.rows.len() {
            return Outcome::Moved;
        }
        self.rows.remove(self.selected);
        self.selected = self.selected.min(self.rows.len().saturating_sub(1));
        Outcome::Changed
    }

    fn edit_key(&mut self, chord: Chord) -> Outcome {
        let Some(edit) = &mut self.edit else {
            return Outcome::Ignored;
        };
        match chord.code {
            Code::Esc => {
                let (added, original) = (edit.added, edit.original.clone());
                edit.text = TextArea::from_text(&original);
                self.commit();
                if added {
                    self.remove();
                }
                Outcome::Changed
            }
            Code::Enter => {
                self.commit();
                Outcome::Changed
            }
            Code::Tab | Code::BackTab if !self.fixed => {
                let next = match edit.cell {
                    Cell::Name => Cell::Value,
                    Cell::Value => Cell::Name,
                };
                let added = edit.added;
                self.commit();
                self.start(next, added);
                Outcome::Changed
            }
            // One line only: Enter commits instead of breaking the line.
            Code::Up | Code::Down => Outcome::Moved,
            _ if vim::type_key(&mut edit.text, chord) => {
                self.write_back();
                Outcome::Changed
            }
            _ => Outcome::Moved,
        }
    }

    /// Copy the edited text into its row (so the URL and dirty marker
    /// follow every keystroke).
    fn write_back(&mut self) {
        let (Some(edit), Some(row)) = (&self.edit, self.rows.get_mut(self.selected)) else {
            return;
        };
        let text = edit.text.text();
        match edit.cell {
            Cell::Name => row.name = text,
            Cell::Value => row.value = text,
        }
    }

    fn commit(&mut self) {
        self.write_back();
        self.edit = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::keymap::chord::parse_chord;

    fn press(table: &mut KvTable, keys: &str) -> Vec<Outcome> {
        keys.split(' ')
            .map(|k| table.key(parse_chord(k).unwrap()))
            .collect()
    }

    #[test]
    fn add_edit_toggle_and_remove_rows() {
        let mut table = KvTable::default();
        press(&mut table, "a p a g e Tab 2 Enter");
        assert_eq!(table.rows, [Row::new("page", "2")]);
        assert!(table.edit.is_none());
        press(&mut table, "Space");
        assert!(!table.rows[0].enabled);
        let pairs = table.to_pairs();
        assert_eq!(pairs, [NameValue::new("page", "2").enabled(false)]);
        press(
            &mut table,
            "Enter Backspace Backspace Backspace Backspace n Enter",
        );
        assert_eq!(table.rows[0].name, "n");
        press(&mut table, "x");
        assert!(table.rows.is_empty());
    }

    #[test]
    fn cancelling_a_new_row_removes_it_and_cancelling_an_edit_keeps_typed_text_out() {
        let mut table = KvTable::from_pairs(&[NameValue::new("a", "1")]);
        press(&mut table, "a x Esc");
        assert_eq!(table.rows.len(), 1);
        press(&mut table, "Enter b Esc");
        assert_eq!(table.rows, [Row::new("a", "1")]);
        assert_eq!(press(&mut table, "Ctrl+P"), [Outcome::Ignored]);
        assert_eq!(press(&mut table, "?"), [Outcome::Ignored]);
    }

    #[test]
    fn fixed_forms_edit_values_only() {
        let mut table = KvTable::fixed(vec![Row::new("token", "")]);
        assert_eq!(press(&mut table, "Space a x"), [Outcome::Ignored; 3]);
        press(&mut table, "Enter t k Enter");
        assert_eq!(table.value("token"), "tk");
        assert_eq!(table.value("missing"), "");
    }

    #[test]
    fn rows_without_a_name_are_not_written() {
        let mut table = KvTable::default();
        table.push("", "orphan");
        table.push("k", "v");
        assert_eq!(table.to_pairs(), [NameValue::new("k", "v")]);
        press(&mut table, "Up Up");
        assert_eq!(table.selected, 0);
        press(&mut table, "Down Down Down");
        assert_eq!(table.selected, 1);
    }
}
