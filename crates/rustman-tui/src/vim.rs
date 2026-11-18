//! Modal editing in text areas (spec: Vim mode): normal, insert and visual
//! modes with `i a o x dd yy p u`, `h j k l w b e 0 $ gg G` and
//! `Ctrl+D`/`Ctrl+U`. Not a full vim emulation (design, Risks).

use crate::keymap::chord::{Chord, Code};
use crate::status::VimMode;
use crate::textarea::{Pos, TextArea};

/// Lines moved by `Ctrl+D` / `Ctrl+U`.
const HALF_PAGE: usize = 10;

/// The modal keys, for the help overlay and the docs: (keys, description).
pub const KEYS: [(&str, &str); 14] = [
    (
        "i a o",
        "insert before / after the cursor, on a new line below",
    ),
    ("Esc", "back to normal mode"),
    ("h j k l", "left, down, up, right (also the arrow keys)"),
    ("w b e", "next word, previous word, end of word"),
    ("0 $", "start, end of the line"),
    ("gg G", "first, last line"),
    ("Ctrl+D Ctrl+U", "down, up half a page"),
    ("x", "delete the character under the cursor"),
    ("dd", "cut the line"),
    ("yy", "copy the line"),
    ("p", "paste after the cursor"),
    ("u", "undo"),
    ("v", "visual mode: move to select"),
    ("y d", "in visual mode: copy, cut the selection"),
];

/// The vim state of one text area.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Vim {
    pub mode: VimMode,
    /// The first key of `dd`, `yy` or `gg`.
    pending: Option<char>,
    /// Where visual mode started.
    pub anchor: Pos,
}

impl Default for Vim {
    fn default() -> Self {
        Self {
            mode: VimMode::Normal,
            pending: None,
            anchor: Pos::default(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Motion {
    Left,
    Right,
    Up,
    Down,
    WordForward,
    WordBack,
    WordEnd,
    LineStart,
    LineEnd,
    Bottom,
    HalfPageDown,
    HalfPageUp,
}

fn motion(chord: Chord) -> Option<Motion> {
    if chord.ctrl {
        return match chord.code {
            Code::Char('d') => Some(Motion::HalfPageDown),
            Code::Char('u') => Some(Motion::HalfPageUp),
            _ => None,
        };
    }
    Some(match chord.code {
        Code::Char('h') | Code::Left => Motion::Left,
        Code::Char('l') | Code::Right => Motion::Right,
        Code::Char('k') | Code::Up => Motion::Up,
        Code::Char('j') | Code::Down => Motion::Down,
        Code::Char('w') => Motion::WordForward,
        Code::Char('b') => Motion::WordBack,
        Code::Char('e') => Motion::WordEnd,
        Code::Char('0') | Code::Home => Motion::LineStart,
        Code::Char('$') | Code::End => Motion::LineEnd,
        Code::Char('G') => Motion::Bottom,
        _ => return None,
    })
}

fn apply(area: &mut TextArea, motion: Motion) {
    match motion {
        Motion::Left => area.left(),
        Motion::Right => area.right(false),
        Motion::Up => area.up(false),
        Motion::Down => area.down(false),
        Motion::WordForward => area.word_forward(),
        Motion::WordBack => area.word_back(),
        Motion::WordEnd => area.word_end(),
        Motion::LineStart => area.line_start(),
        Motion::LineEnd => area.line_end(false),
        Motion::Bottom => area.bottom(),
        Motion::HalfPageDown => (0..HALF_PAGE).for_each(|_| area.down(false)),
        Motion::HalfPageUp => (0..HALF_PAGE).for_each(|_| area.up(false)),
    }
}

impl Vim {
    /// Handle a key in `area`; false when the key is not a vim key here and
    /// should go to the keymap (`Tab`, `Ctrl+P`, `Esc` in normal mode, ...).
    pub fn key(&mut self, area: &mut TextArea, chord: Chord) -> bool {
        match self.mode {
            VimMode::Insert => insert_key(self, area, chord),
            VimMode::Normal => self.normal_key(area, chord),
            VimMode::Visual => self.visual_key(area, chord),
        }
    }

    /// Whether typed characters are text (so `?` does not open help).
    pub fn accepts_text(&self) -> bool {
        self.mode == VimMode::Insert
    }

    fn insert(&mut self, area: &mut TextArea) {
        area.checkpoint();
        self.mode = VimMode::Insert;
    }

    fn normal_key(&mut self, area: &mut TextArea, chord: Chord) -> bool {
        if let Some(first) = self.pending.take() {
            pending_key(area, first, chord);
            return true;
        }
        if let Some(motion) = motion(chord) {
            apply(area, motion);
            return true;
        }
        let Some(c) = chord.typed_char() else {
            return false;
        };
        self.normal_char(area, c)
    }

    fn normal_char(&mut self, area: &mut TextArea, c: char) -> bool {
        match c {
            'i' => self.insert(area),
            'a' => {
                self.insert(area);
                area.right(true);
            }
            'o' => {
                self.insert(area);
                area.open_below();
            }
            'x' => edit(area, |a| {
                a.delete_char();
                a.clamp(false);
            }),
            'p' => edit(area, TextArea::paste),
            'u' => {
                area.undo();
                area.clamp(false);
            }
            'v' => {
                self.anchor = area.cursor();
                self.mode = VimMode::Visual;
            }
            'd' | 'y' | 'g' => self.pending = Some(c),
            _ => return false,
        }
        true
    }

    fn visual_key(&mut self, area: &mut TextArea, chord: Chord) -> bool {
        if let Some(motion) = motion(chord) {
            apply(area, motion);
            return true;
        }
        let (anchor, cursor) = (self.anchor, area.cursor());
        match (chord.code, chord.typed_char()) {
            (_, Some('y')) => area.yank_range(anchor, cursor),
            (_, Some('d' | 'x')) => edit(area, |a| a.delete_range(anchor, cursor)),
            (Code::Esc, _) | (_, Some('v')) => {}
            _ => return chord.typed_char().is_some(),
        }
        self.mode = VimMode::Normal;
        true
    }
}

/// An undoable change.
fn edit(area: &mut TextArea, change: impl FnOnce(&mut TextArea)) {
    area.checkpoint();
    change(area);
}

/// The second key of `dd`, `yy` or `gg`; anything else cancels.
fn pending_key(area: &mut TextArea, first: char, chord: Chord) {
    match (first, chord.typed_char()) {
        ('d', Some('d')) => edit(area, TextArea::delete_line),
        ('y', Some('y')) => area.yank_line(),
        ('g', Some('g')) => area.top(),
        _ => {}
    }
}

fn insert_key(vim: &mut Vim, area: &mut TextArea, chord: Chord) -> bool {
    match chord.code {
        Code::Esc => {
            vim.mode = VimMode::Normal;
            area.left();
            area.clamp(false);
            true
        }
        _ => type_key(area, chord),
    }
}

/// Plain (non-modal) text editing: typing, `Enter`, `Backspace`, `Delete`,
/// arrows, `Home`/`End`. False for keys that are not editing keys.
pub fn type_key(area: &mut TextArea, chord: Chord) -> bool {
    if let Some(c) = chord.typed_char() {
        area.insert_char(c);
        return true;
    }
    if chord.ctrl || chord.alt {
        return false;
    }
    match chord.code {
        Code::Enter => area.newline(),
        Code::Backspace => area.backspace(),
        Code::Delete => area.delete_char(),
        Code::Left => area.left(),
        Code::Right => area.right(true),
        Code::Up => area.up(true),
        Code::Down => area.down(true),
        Code::Home => area.line_start(),
        Code::End => area.line_end(true),
        _ => return false,
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::keymap::chord::{parse, parse_chord};

    /// Feed space-separated keys (`i a b c Esc`) and return what was not
    /// consumed.
    fn feed(vim: &mut Vim, area: &mut TextArea, keys: &str) -> Vec<String> {
        let mut unhandled = Vec::new();
        for word in keys.split(' ') {
            let chords = if word.chars().count() > 1 && !word.contains('+') {
                parse(word).unwrap()
            } else {
                vec![parse_chord(word).unwrap()]
            };
            for chord in chords {
                if !vim.key(area, chord) {
                    unhandled.push(chord.to_string());
                }
            }
        }
        unhandled
    }

    fn run(text: &str, keys: &str) -> (Vim, TextArea) {
        let mut vim = Vim::default();
        let mut area = TextArea::from_text(text);
        let unhandled = feed(&mut vim, &mut area, keys);
        assert!(unhandled.is_empty(), "unhandled {unhandled:?}");
        (vim, area)
    }

    /// The keys that put the editor in the state a `KEYS` entry acts on:
    /// a mode, something to paste or an edit to undo.
    fn mode_prefix(keys: &str) -> &'static str {
        match keys {
            "Esc" => "i",
            "y d" => "v l",
            "p" => "yy",
            "u" => "x",
            _ => "",
        }
    }

    #[test]
    fn every_documented_key_is_handled() {
        // Guards the help overlay and docs table (built from KEYS) against
        // drifting from the key handling here.
        for (keys, _) in KEYS {
            for key in keys.split(' ') {
                let prefix = mode_prefix(keys);
                let (mut vim, mut area) = run("one two\nthree four\nfive", "j l");
                if !prefix.is_empty() {
                    assert!(feed(&mut vim, &mut area, prefix).is_empty());
                }
                let before = (vim.clone(), area.clone());
                let unhandled = feed(&mut vim, &mut area, key);
                assert!(unhandled.is_empty(), "{key:?} of {keys:?} not handled");
                let after = (vim.clone(), area.clone());
                assert_ne!(before, after, "{key:?} of {keys:?} did nothing");
            }
        }
    }

    #[test]
    fn insert_abc_then_esc_returns_to_normal() {
        // Scenario "Modal editing in the body".
        let (vim, area) = run("", "i a b c Esc");
        assert_eq!(area.text(), "abc");
        assert_eq!(vim.mode, VimMode::Normal);
        assert_eq!(area.cursor().col, 2);
    }

    #[test]
    fn append_and_open_line() {
        let (_, area) = run("ac", "a b Esc");
        assert_eq!(area.text(), "abc");
        let (_, area) = run("one", "o t w o Esc");
        assert_eq!(area.text(), "one\ntwo");
    }

    #[test]
    fn delete_yank_paste_undo() {
        let (_, area) = run("a\nb\nc", "j dd");
        assert_eq!(area.text(), "a\nc");
        let (_, area) = run("a\nb", "yy p");
        assert_eq!(area.text(), "a\na\nb");
        let (_, area) = run("abc", "x x u");
        assert_eq!(area.text(), "bc");
        let (_, area) = run("abc", "l x p");
        assert_eq!(area.text(), "acb");
        let (_, area) = run("", "i h i Esc u");
        assert_eq!(area.text(), "");
    }

    #[test]
    fn motions() {
        let (_, area) = run("foo bar baz", "w");
        assert_eq!(area.cursor().col, 4);
        let (_, area) = run("foo bar baz", "$ b");
        assert_eq!(area.cursor().col, 8);
        let (_, area) = run("foo bar", "e");
        assert_eq!(area.cursor().col, 2);
        let (_, area) = run("a\nb\nc", "G k gg j");
        assert_eq!(area.cursor().row, 1);
        let text: String = (0..30).map(|i| format!("{i}\n")).collect();
        let (_, area) = run(&text, "Ctrl+D Ctrl+D Ctrl+U");
        assert_eq!(area.cursor().row, 10);
        let (_, area) = run("abc", "$ 0 l");
        assert_eq!(area.cursor().col, 1);
    }

    #[test]
    fn visual_mode_yanks_and_deletes_the_selection() {
        let (vim, area) = run("hello world", "v e y");
        assert_eq!(vim.mode, VimMode::Normal);
        assert_eq!(area.register.text, "hello");
        let (_, area) = run("hello world", "w v $ d");
        assert_eq!(area.text(), "hello ");
        let (vim, _) = run("abc", "v Esc");
        assert_eq!(vim.mode, VimMode::Normal);
    }

    #[test]
    fn mode_changes_are_tracked() {
        let mut vim = Vim::default();
        let mut area = TextArea::default();
        assert!(!vim.accepts_text());
        feed(&mut vim, &mut area, "i");
        assert_eq!(vim.mode, VimMode::Insert);
        assert!(vim.accepts_text());
        feed(&mut vim, &mut area, "Esc v");
        assert_eq!(vim.mode, VimMode::Visual);
    }

    #[test]
    fn non_vim_keys_fall_through() {
        let mut vim = Vim::default();
        let mut area = TextArea::default();
        let unhandled = feed(&mut vim, &mut area, "Tab Ctrl+P Esc ? i Tab");
        assert_eq!(unhandled, ["Tab", "Ctrl+P", "Esc", "?", "Tab"]);
        let unhandled = feed(&mut vim, &mut area, "Esc d z");
        assert!(unhandled.is_empty(), "an unknown second key cancels");
    }

    #[test]
    fn plain_typing() {
        let mut area = TextArea::default();
        for word in ["a", "Enter", "b", "Left", "Backspace", "Delete", "Ctrl+P"] {
            type_key(&mut area, parse_chord(word).unwrap());
        }
        assert_eq!(area.text(), "a");
        assert!(!type_key(&mut area, parse_chord("Ctrl+P").unwrap()));
        assert!(!type_key(&mut area, parse_chord("F5").unwrap()));
    }
}
