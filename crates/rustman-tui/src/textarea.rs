//! A multi-line text buffer with a cursor, undo and a yank register: the
//! model behind text areas (the request body). Positions are in characters,
//! not bytes. Key handling lives elsewhere: plain typing in `app`, modal
//! editing in [`crate::vim`].

/// A cursor position: line and character column.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default)]
pub struct Pos {
    pub row: usize,
    pub col: usize,
}

/// What `y`/`d` stored for `p`.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Register {
    pub text: String,
    /// Whole lines (`yy`, `dd`): pasted below the cursor line.
    pub linewise: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TextArea {
    lines: Vec<String>,
    cursor: Pos,
    undo: Vec<(Vec<String>, Pos)>,
    pub register: Register,
}

impl Default for TextArea {
    fn default() -> Self {
        Self {
            lines: vec![String::new()],
            cursor: Pos::default(),
            undo: Vec::new(),
            register: Register::default(),
        }
    }
}

/// The character class used by word motions.
#[derive(PartialEq, Eq, Clone, Copy)]
enum Class {
    Space,
    Word,
    Punct,
}

fn class(c: char) -> Class {
    if c.is_whitespace() {
        Class::Space
    } else if c.is_alphanumeric() || c == '_' {
        Class::Word
    } else {
        Class::Punct
    }
}

fn byte_at(line: &str, col: usize) -> usize {
    line.char_indices().nth(col).map_or(line.len(), |(i, _)| i)
}

impl TextArea {
    pub fn from_text(text: &str) -> Self {
        Self {
            lines: text.split('\n').map(str::to_owned).collect(),
            ..Self::default()
        }
    }

    pub fn text(&self) -> String {
        self.lines.join("\n")
    }

    pub fn lines(&self) -> &[String] {
        &self.lines
    }

    pub fn cursor(&self) -> Pos {
        self.cursor
    }

    pub fn is_empty(&self) -> bool {
        self.lines.len() == 1 && self.lines[0].is_empty()
    }

    fn line_len(&self, row: usize) -> usize {
        self.lines[row].chars().count()
    }

    fn char_at(&self, pos: Pos) -> Option<char> {
        self.lines.get(pos.row)?.chars().nth(pos.col)
    }

    /// Remember the current text so `undo` can restore it.
    pub fn checkpoint(&mut self) {
        self.undo.push((self.lines.clone(), self.cursor));
    }

    /// Restore the last checkpoint; false when there is none.
    pub fn undo(&mut self) -> bool {
        let Some((lines, cursor)) = self.undo.pop() else {
            return false;
        };
        self.lines = lines;
        self.cursor = cursor;
        true
    }

    // ---- editing -------------------------------------------------------

    pub fn insert_char(&mut self, c: char) {
        if c == '\n' {
            self.newline();
            return;
        }
        let Pos { row, col } = self.cursor;
        let at = byte_at(&self.lines[row], col);
        self.lines[row].insert(at, c);
        self.cursor.col += 1;
    }

    pub fn insert_str(&mut self, text: &str) {
        text.chars().for_each(|c| self.insert_char(c));
    }

    pub fn newline(&mut self) {
        let Pos { row, col } = self.cursor;
        let at = byte_at(&self.lines[row], col);
        let rest = self.lines[row].split_off(at);
        self.lines.insert(row + 1, rest);
        self.cursor = Pos {
            row: row + 1,
            col: 0,
        };
    }

    /// Delete the character before the cursor, joining lines at column 0.
    pub fn backspace(&mut self) {
        let Pos { row, col } = self.cursor;
        if col > 0 {
            let at = byte_at(&self.lines[row], col - 1);
            self.lines[row].remove(at);
            self.cursor.col -= 1;
        } else if row > 0 {
            let line = self.lines.remove(row);
            let col = self.line_len(row - 1);
            self.lines[row - 1].push_str(&line);
            self.cursor = Pos { row: row - 1, col };
        }
    }

    /// Delete the character under the cursor (`x`).
    pub fn delete_char(&mut self) {
        let Pos { row, col } = self.cursor;
        if col < self.line_len(row) {
            let at = byte_at(&self.lines[row], col);
            let removed = self.lines[row].remove(at);
            self.register = Register {
                text: removed.to_string(),
                linewise: false,
            };
        }
    }

    /// Open an empty line below the cursor line and move to it (`o`).
    pub fn open_below(&mut self) {
        let row = self.cursor.row + 1;
        self.lines.insert(row, String::new());
        self.cursor = Pos { row, col: 0 };
    }

    /// Copy the cursor line into the register (`yy`).
    pub fn yank_line(&mut self) {
        self.register = Register {
            text: self.lines[self.cursor.row].clone(),
            linewise: true,
        };
    }

    /// Cut the cursor line (`dd`).
    pub fn delete_line(&mut self) {
        self.yank_line();
        if self.lines.len() == 1 {
            self.lines[0].clear();
        } else {
            self.lines.remove(self.cursor.row);
        }
        self.cursor.row = self.cursor.row.min(self.lines.len() - 1);
        self.cursor.col = 0;
    }

    /// Paste the register after the cursor (`p`).
    pub fn paste(&mut self) {
        let register = self.register.clone();
        if register.linewise {
            let row = self.cursor.row + 1;
            self.lines.insert(row, register.text);
            self.cursor = Pos { row, col: 0 };
        } else if !register.text.is_empty() {
            self.cursor.col = (self.cursor.col + 1).min(self.line_len(self.cursor.row));
            self.insert_str(&register.text);
            self.cursor.col = self.cursor.col.saturating_sub(1);
        }
    }

    /// The text between two positions, both included (visual mode).
    pub fn slice(&self, a: Pos, b: Pos) -> String {
        let (start, end) = if a <= b { (a, b) } else { (b, a) };
        let mut out = String::new();
        for row in start.row..=end.row {
            let chars = self.lines[row].chars();
            let from = if row == start.row { start.col } else { 0 };
            let line: String = if row == end.row {
                chars.take(end.col + 1).skip(from).collect()
            } else {
                chars.skip(from).collect()
            };
            out.push_str(&line);
            if row != end.row {
                out.push('\n');
            }
        }
        out
    }

    /// Copy the text between two positions into the register.
    pub fn yank_range(&mut self, a: Pos, b: Pos) {
        self.register = Register {
            text: self.slice(a, b),
            linewise: false,
        };
        self.cursor = a.min(b);
    }

    /// Cut the text between two positions, both included.
    pub fn delete_range(&mut self, a: Pos, b: Pos) {
        self.yank_range(a, b);
        let (start, end) = (a.min(b), a.max(b));
        let end_line = &self.lines[end.row];
        let tail = end_line[byte_at(end_line, end.col + 1)..].to_owned();
        let head_end = byte_at(&self.lines[start.row], start.col);
        self.lines[start.row].truncate(head_end);
        self.lines[start.row].push_str(&tail);
        self.lines.drain(start.row + 1..=end.row);
        self.cursor = start;
        self.clamp(false);
    }

    // ---- movement ------------------------------------------------------

    /// Keep the cursor on the text. In normal mode it sits on a character,
    /// in insert mode it may sit after the last one.
    pub fn clamp(&mut self, past_end: bool) {
        self.cursor.row = self.cursor.row.min(self.lines.len() - 1);
        let len = self.line_len(self.cursor.row);
        let max = if past_end { len } else { len.saturating_sub(1) };
        self.cursor.col = self.cursor.col.min(max);
    }

    pub fn left(&mut self) {
        self.cursor.col = self.cursor.col.saturating_sub(1);
    }

    pub fn right(&mut self, past_end: bool) {
        self.cursor.col += 1;
        self.clamp(past_end);
    }

    pub fn up(&mut self, past_end: bool) {
        self.cursor.row = self.cursor.row.saturating_sub(1);
        self.clamp(past_end);
    }

    pub fn down(&mut self, past_end: bool) {
        self.cursor.row += 1;
        self.clamp(past_end);
    }

    pub fn line_start(&mut self) {
        self.cursor.col = 0;
    }

    pub fn line_end(&mut self, past_end: bool) {
        self.cursor.col = usize::MAX;
        self.clamp(past_end);
    }

    pub fn top(&mut self) {
        self.cursor = Pos::default();
    }

    pub fn bottom(&mut self) {
        self.cursor = Pos {
            row: self.lines.len() - 1,
            col: 0,
        };
    }

    /// The position after `pos`, crossing line ends (a line end counts as
    /// one blank position so words stop at it).
    fn next_pos(&self, pos: Pos) -> Option<Pos> {
        if pos.col < self.line_len(pos.row) {
            return Some(Pos {
                col: pos.col + 1,
                ..pos
            });
        }
        (pos.row + 1 < self.lines.len()).then_some(Pos {
            row: pos.row + 1,
            col: 0,
        })
    }

    fn prev_pos(&self, pos: Pos) -> Option<Pos> {
        if pos.col > 0 {
            return Some(Pos {
                col: pos.col - 1,
                ..pos
            });
        }
        (pos.row > 0).then(|| Pos {
            row: pos.row - 1,
            col: self.line_len(pos.row - 1),
        })
    }

    fn class_at(&self, pos: Pos) -> Class {
        self.char_at(pos).map_or(Class::Space, class)
    }

    /// `w`: the start of the next word.
    pub fn word_forward(&mut self) {
        let mut pos = self.cursor;
        let start = self.class_at(pos);
        while let Some(next) = self.next_pos(pos) {
            pos = next;
            if self.class_at(pos) != start {
                break;
            }
        }
        while self.class_at(pos) == Class::Space {
            match self.next_pos(pos) {
                Some(next) => pos = next,
                None => break,
            }
        }
        self.cursor = pos;
        self.clamp(false);
    }

    /// `b`: the start of this or the previous word.
    pub fn word_back(&mut self) {
        let mut pos = self.cursor;
        while let Some(prev) = self.prev_pos(pos) {
            pos = prev;
            if self.class_at(pos) != Class::Space {
                break;
            }
        }
        let class = self.class_at(pos);
        while let Some(prev) = self.prev_pos(pos) {
            if self.class_at(prev) != class {
                break;
            }
            pos = prev;
        }
        self.cursor = pos;
        self.clamp(false);
    }

    /// `e`: the end of this or the next word.
    pub fn word_end(&mut self) {
        let mut pos = self.cursor;
        while let Some(next) = self.next_pos(pos) {
            pos = next;
            if self.class_at(pos) != Class::Space {
                break;
            }
        }
        let class = self.class_at(pos);
        while let Some(next) = self.next_pos(pos) {
            if self.class_at(next) != class {
                break;
            }
            pos = next;
        }
        self.cursor = pos;
        self.clamp(false);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(text: &str, row: usize, col: usize) -> TextArea {
        let mut area = TextArea::from_text(text);
        area.cursor = Pos { row, col };
        area
    }

    #[test]
    fn typing_newlines_and_backspace() {
        let mut area = TextArea::default();
        assert!(area.is_empty());
        area.insert_str("hé\nyo");
        assert_eq!(area.text(), "hé\nyo");
        assert_eq!(area.cursor(), Pos { row: 1, col: 2 });
        area.cursor = Pos { row: 1, col: 0 };
        area.backspace();
        assert_eq!(area.text(), "héyo");
        assert_eq!(area.cursor(), Pos { row: 0, col: 2 });
        area.backspace();
        assert_eq!(area.text(), "hyo");
    }

    #[test]
    fn word_motions() {
        let mut area = at("let x = foo.bar;\nnext", 0, 0);
        let mut stops = Vec::new();
        for _ in 0..7 {
            area.word_forward();
            stops.push(area.cursor());
        }
        let cols: Vec<_> = stops.iter().map(|p| (p.row, p.col)).collect();
        assert_eq!(
            cols,
            [(0, 4), (0, 6), (0, 8), (0, 11), (0, 12), (0, 15), (1, 0)]
        );
        area.word_back();
        assert_eq!(area.cursor(), Pos { row: 0, col: 15 });
        area.word_back();
        assert_eq!(area.cursor(), Pos { row: 0, col: 12 });
        let mut area = at("foo bar", 0, 0);
        area.word_end();
        assert_eq!(area.cursor().col, 2);
        area.word_end();
        assert_eq!(area.cursor().col, 6);
    }

    #[test]
    fn line_yank_delete_paste_and_undo() {
        let mut area = at("a\nb\nc", 1, 0);
        area.checkpoint();
        area.delete_line();
        assert_eq!(area.text(), "a\nc");
        area.paste();
        assert_eq!(area.text(), "a\nc\nb");
        area.yank_line();
        area.paste();
        assert_eq!(area.text(), "a\nc\nb\nb");
        assert!(area.undo());
        assert_eq!(area.text(), "a\nb\nc");
        assert!(!area.undo());
        let mut single = TextArea::from_text("only");
        single.delete_line();
        assert!(single.is_empty());
    }

    #[test]
    fn x_and_charwise_paste() {
        let mut area = at("abc", 0, 1);
        area.delete_char();
        assert_eq!(area.text(), "ac");
        area.paste();
        assert_eq!(area.text(), "acb");
        assert_eq!(area.cursor().col, 2);
    }

    #[test]
    fn ranges_across_lines() {
        let area = at("hello\nworld", 0, 0);
        assert_eq!(
            area.slice(Pos { row: 0, col: 3 }, Pos { row: 1, col: 1 }),
            "lo\nwo"
        );
        let mut area = at("hello\nworld", 0, 0);
        area.delete_range(Pos { row: 1, col: 1 }, Pos { row: 0, col: 3 });
        assert_eq!(area.text(), "helrld");
        assert_eq!(area.register.text, "lo\nwo");
        assert_eq!(area.cursor(), Pos { row: 0, col: 3 });
    }

    #[test]
    fn movement_is_clamped() {
        let mut area = at("ab\nlonger", 1, 5);
        area.up(false);
        assert_eq!(area.cursor(), Pos { row: 0, col: 1 });
        area.line_end(true);
        assert_eq!(area.cursor().col, 2);
        area.bottom();
        area.down(false);
        assert_eq!(area.cursor(), Pos { row: 1, col: 0 });
        area.open_below();
        assert_eq!(area.text(), "ab\nlonger\n");
    }
}
