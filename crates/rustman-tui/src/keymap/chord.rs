//! Key chords and key sequences: parsing (`Ctrl+Enter`, `F5`, `gg`, `z M`),
//! display, and normalising crossterm key events so that a parsed chord and
//! a pressed key compare equal.

use std::fmt;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

/// One key with its modifiers, normalised (see [`Chord::from_event`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Chord {
    pub code: Code,
    pub ctrl: bool,
    pub alt: bool,
    /// Only for non-character keys: `Shift+F5`. A shifted character is its
    /// own character (`G`, `?`).
    pub shift: bool,
}

/// The keys a chord can name.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Code {
    Char(char),
    F(u8),
    Enter,
    Esc,
    Tab,
    BackTab,
    Backspace,
    Delete,
    Insert,
    Home,
    End,
    PageUp,
    PageDown,
    Up,
    Down,
    Left,
    Right,
}

/// A sequence of chords that triggers one action (`Ctrl+P`, or `g g`).
pub type Keys = Vec<Chord>;

const NAMED: [(&str, Code); 17] = [
    ("Enter", Code::Enter),
    ("Esc", Code::Esc),
    ("Tab", Code::Tab),
    ("Backspace", Code::Backspace),
    ("Delete", Code::Delete),
    ("Insert", Code::Insert),
    ("Home", Code::Home),
    ("End", Code::End),
    ("PageUp", Code::PageUp),
    ("PageDown", Code::PageDown),
    ("Up", Code::Up),
    ("Down", Code::Down),
    ("Left", Code::Left),
    ("Right", Code::Right),
    ("Space", Code::Char(' ')),
    ("Escape", Code::Esc),
    ("Del", Code::Delete),
];

impl Chord {
    pub const fn plain(code: Code) -> Self {
        Self {
            code,
            ctrl: false,
            alt: false,
            shift: false,
        }
    }

    /// The chord for a pressed key, or `None` for keys no binding can name
    /// (media keys, modifiers on their own, ...).
    pub fn from_event(key: KeyEvent) -> Option<Self> {
        let code = match key.code {
            KeyCode::Char(c) => Code::Char(c),
            KeyCode::F(n) => Code::F(n),
            KeyCode::BackTab => Code::BackTab,
            other => named_code(other)?,
        };
        Some(Self::with_modifiers(code, key.modifiers).normalised())
    }

    fn with_modifiers(code: Code, modifiers: KeyModifiers) -> Self {
        Self {
            code,
            ctrl: modifiers.contains(KeyModifiers::CONTROL),
            alt: modifiers.contains(KeyModifiers::ALT),
            shift: modifiers.contains(KeyModifiers::SHIFT),
        }
    }

    /// Terminals disagree on how they report shift and control with
    /// characters; reduce every report to one form.
    fn normalised(mut self) -> Self {
        match self.code {
            Code::Char(c) => {
                self.shift = false;
                if self.ctrl || self.alt {
                    self.code = Code::Char(c.to_ascii_lowercase());
                }
            }
            Code::BackTab => self.shift = false,
            Code::Tab if self.shift => {
                self.code = Code::BackTab;
                self.shift = false;
            }
            _ => {}
        }
        self
    }

    /// A character typed without `Ctrl`/`Alt`: text in a text input.
    pub fn typed_char(self) -> Option<char> {
        match self.code {
            Code::Char(c) if !self.ctrl && !self.alt => Some(c),
            _ => None,
        }
    }
}

fn named_code(code: KeyCode) -> Option<Code> {
    Some(match code {
        KeyCode::Enter => Code::Enter,
        KeyCode::Esc => Code::Esc,
        KeyCode::Tab => Code::Tab,
        KeyCode::Backspace => Code::Backspace,
        KeyCode::Delete => Code::Delete,
        KeyCode::Insert => Code::Insert,
        KeyCode::Home => Code::Home,
        KeyCode::End => Code::End,
        KeyCode::PageUp => Code::PageUp,
        KeyCode::PageDown => Code::PageDown,
        KeyCode::Up => Code::Up,
        KeyCode::Down => Code::Down,
        KeyCode::Left => Code::Left,
        KeyCode::Right => Code::Right,
        _ => return None,
    })
}

impl fmt::Display for Chord {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.ctrl {
            f.write_str("Ctrl+")?;
        }
        if self.alt {
            f.write_str("Alt+")?;
        }
        if self.shift {
            f.write_str("Shift+")?;
        }
        match self.code {
            Code::Char(' ') => f.write_str("Space"),
            Code::Char(c) if self.ctrl || self.alt => write!(f, "{}", c.to_ascii_uppercase()),
            Code::Char(c) => write!(f, "{c}"),
            Code::F(n) => write!(f, "F{n}"),
            Code::BackTab => f.write_str("Shift+Tab"),
            code => {
                let name = NAMED.iter().find(|(_, c)| *c == code).map(|(n, _)| *n);
                f.write_str(name.unwrap_or("?"))
            }
        }
    }
}

/// Display a key sequence: `gg` for plain characters, else chords joined by
/// spaces (`Ctrl+K Ctrl+S`).
pub fn display(keys: &[Chord]) -> String {
    let plain = keys
        .iter()
        .all(|k| k.typed_char().is_some_and(|c| c != ' '));
    if plain && keys.len() > 1 {
        return keys.iter().map(ToString::to_string).collect();
    }
    let parts: Vec<String> = keys.iter().map(ToString::to_string).collect();
    parts.join(" ")
}

/// Parse a key sequence: whitespace-separated chords, where a word of plain
/// characters that is not a key name (`gg`, `zM`) is one chord per character.
pub fn parse(text: &str) -> Result<Keys, String> {
    let mut keys = Vec::new();
    for word in text.split_whitespace() {
        keys.extend(parse_word(word)?);
    }
    match keys.len() {
        0 => Err("empty key".to_owned()),
        1..=3 => Ok(keys),
        _ => Err(format!("`{text}` is longer than 3 keys")),
    }
}

fn parse_word(word: &str) -> Result<Keys, String> {
    let f_key = word.len() > 1
        && word.starts_with(['F', 'f'])
        && word[1..].chars().all(|c| c.is_ascii_digit());
    if word.chars().count() > 1 && !word.contains('+') && !f_key && key_name(word).is_none() {
        return Ok(word.chars().map(|c| Chord::plain(Code::Char(c))).collect());
    }
    parse_chord(word).map(|chord| vec![chord])
}

/// Parse one chord such as `Ctrl+Shift+F5`, `Alt+e`, `?` or `+`.
pub fn parse_chord(text: &str) -> Result<Chord, String> {
    let (modifiers, key) = split_modifiers(text);
    let mut chord = Chord::plain(Code::Char(' '));
    for modifier in modifiers {
        match modifier.to_ascii_lowercase().as_str() {
            "ctrl" | "control" => chord.ctrl = true,
            "alt" | "meta" | "option" => chord.alt = true,
            "shift" => chord.shift = true,
            _ => return Err(format!("unknown modifier `{modifier}` in `{text}`")),
        }
    }
    chord.code = key_code(key).ok_or_else(|| format!("unknown key `{key}` in `{text}`"))?;
    Ok(chord.normalised())
}

/// Split `Ctrl+Alt+x` into the modifiers and the key; a trailing `+` is the
/// key `+` itself (`Ctrl++`).
fn split_modifiers(text: &str) -> (Vec<&str>, &str) {
    if text == "+" {
        return (Vec::new(), "+");
    }
    let (head, key) = match text.strip_suffix("++") {
        Some(head) => (head, "+"),
        None => text.rsplit_once('+').unwrap_or(("", text)),
    };
    let modifiers = head.split('+').filter(|m| !m.is_empty()).collect();
    (modifiers, key)
}

fn key_code(key: &str) -> Option<Code> {
    if let Some(code) = key_name(key) {
        return Some(code);
    }
    let mut chars = key.chars();
    match (chars.next(), chars.next()) {
        (Some(c), None) => Some(Code::Char(c)),
        _ => None,
    }
}

fn key_name(word: &str) -> Option<Code> {
    if let Some(n) = word
        .strip_prefix(['F', 'f'])
        .and_then(|n| n.parse::<u8>().ok())
    {
        return (1..=24).contains(&n).then_some(Code::F(n));
    }
    NAMED
        .iter()
        .find(|(name, _)| name.eq_ignore_ascii_case(word))
        .map(|(_, code)| *code)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn event(code: KeyCode, modifiers: KeyModifiers) -> Chord {
        Chord::from_event(KeyEvent::new(code, modifiers)).unwrap()
    }

    #[test]
    fn chords_round_trip_through_display() {
        for text in [
            "Ctrl+Enter",
            "Ctrl+R",
            "F5",
            "Alt+E",
            "Shift+Tab",
            "?",
            "G",
            "Ctrl+PageUp",
            "Space",
            "Shift+F5",
            "Ctrl++",
        ] {
            let chord = parse_chord(text).unwrap();
            assert_eq!(chord.to_string(), text);
        }
    }

    #[test]
    fn parsing_is_lenient_about_case_and_aliases() {
        assert_eq!(parse_chord("ctrl+b"), parse_chord("Ctrl+B"));
        assert_eq!(parse_chord("control+escape"), parse_chord("Ctrl+Esc"));
        assert_eq!(parse_chord("f1").unwrap().to_string(), "F1");
        assert_eq!(parse_chord("Shift+Tab"), Ok(Chord::plain(Code::BackTab)));
        assert_ne!(
            parse_chord("g"),
            parse_chord("G"),
            "plain letters keep case"
        );
    }

    #[test]
    fn bad_chords_are_errors() {
        for text in ["Ctrl+Foo", "Hyper+x", "F99", "", "abcd"] {
            assert!(parse(text).is_err(), "{text} parsed");
        }
    }

    #[test]
    fn sequences_parse_and_display() {
        let gg = parse("gg").unwrap();
        assert_eq!(gg.len(), 2);
        assert_eq!(parse("g g").unwrap(), gg);
        assert_eq!(display(&gg), "gg");
        let zm = parse("zM").unwrap();
        assert_eq!(display(&zm), "zM");
        let ctrl = parse("Ctrl+K Ctrl+S").unwrap();
        assert_eq!(display(&ctrl), "Ctrl+K Ctrl+S");
        assert_eq!(parse("Esc").unwrap().len(), 1, "names win over letters");
    }

    #[test]
    fn events_normalise_to_parsed_chords() {
        let ctrl_b = event(
            KeyCode::Char('B'),
            KeyModifiers::CONTROL | KeyModifiers::SHIFT,
        );
        assert_eq!(ctrl_b, parse_chord("Ctrl+B").unwrap());
        let shifted_g = event(KeyCode::Char('G'), KeyModifiers::SHIFT);
        assert_eq!(shifted_g, parse_chord("G").unwrap());
        let question = event(KeyCode::Char('?'), KeyModifiers::SHIFT);
        assert_eq!(question, parse_chord("?").unwrap());
        assert_eq!(
            event(KeyCode::BackTab, KeyModifiers::SHIFT),
            parse_chord("Shift+Tab").unwrap()
        );
        assert_eq!(
            event(KeyCode::Tab, KeyModifiers::SHIFT),
            parse_chord("Shift+Tab").unwrap()
        );
        let ctrl_enter = event(KeyCode::Enter, KeyModifiers::CONTROL);
        assert_eq!(ctrl_enter, parse_chord("Ctrl+Enter").unwrap());
        assert!(Chord::from_event(KeyEvent::new(KeyCode::CapsLock, KeyModifiers::NONE)).is_none());
    }

    #[test]
    fn typed_chars_exclude_control_chords() {
        assert_eq!(parse_chord("x").unwrap().typed_char(), Some('x'));
        assert_eq!(parse_chord("Ctrl+X").unwrap().typed_char(), None);
        assert_eq!(parse_chord("F5").unwrap().typed_char(), None);
    }
}
