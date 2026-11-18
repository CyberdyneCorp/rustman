//! Text bodies: line access, `syntect` highlighting by content type, and the
//! hex dump of binary bodies (spec: Highlighting for non-JSON bodies).

use std::sync::OnceLock;

use syntect::parsing::{ParseState, Scope, ScopeStack, SyntaxReference, SyntaxSet};

use crate::theme::Token;

/// A text split into lines once, read by index.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TextLines {
    text: String,
    /// Byte offset of each line start.
    starts: Vec<usize>,
}

impl TextLines {
    pub fn new(text: String) -> Self {
        let mut starts = vec![0];
        starts.extend(text.match_indices('\n').map(|(i, _)| i + 1));
        if starts.len() > 1 && starts.last() == Some(&text.len()) {
            starts.pop();
        }
        Self { text, starts }
    }

    pub fn len(&self) -> usize {
        if self.text.is_empty() {
            0
        } else {
            self.starts.len()
        }
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Line `i` without its line ending.
    pub fn line(&self, i: usize) -> &str {
        let start = self.starts[i];
        let end = self.starts.get(i + 1).copied().unwrap_or(self.text.len());
        self.text[start..end].trim_end_matches(['\n', '\r'])
    }

    pub fn text(&self) -> &str {
        &self.text
    }
}

/// A highlighted line: runs of text with the token that colours them.
pub type StyledLine = Vec<(Token, String)>;

/// Bodies above this size are shown as plain text: highlighting is
/// sequential from the first line and would hold the viewer back.
pub const HIGHLIGHT_LIMIT: usize = 512 * 1024;

/// Bodies with a line longer than this are shown as plain text too: the
/// regex engine's cost grows with the line, and one minified 400 KB line of
/// HTML took seconds that cannot be interrupted.
pub const HIGHLIGHT_LINE_LIMIT: usize = 20 * 1024;

/// The language a content type is highlighted as, if any.
pub fn language(content_type: &str) -> Option<&'static str> {
    let mime = content_type
        .split(';')
        .next()
        .unwrap_or_default()
        .trim()
        .to_ascii_lowercase();
    let (kind, sub) = mime.split_once('/')?;
    let sub = sub.rsplit('+').next().unwrap_or(sub);
    match (kind, sub) {
        (_, "html" | "xhtml") => Some("html"),
        (_, "xml") => Some("xml"),
        (_, "yaml" | "x-yaml") => Some("yaml"),
        (_, "javascript" | "ecmascript" | "x-javascript") => Some("js"),
        ("text", "css") => Some("css"),
        _ => None,
    }
}

fn syntaxes() -> &'static SyntaxSet {
    static SET: OnceLock<SyntaxSet> = OnceLock::new();
    SET.get_or_init(SyntaxSet::load_defaults_newlines)
}

fn syntax(language: &str) -> Option<&'static SyntaxReference> {
    syntaxes().find_syntax_by_extension(language)
}

/// `text` highlighted as `language` (`html`, `xml`, `yaml`, `js`, `css`);
/// `None` when the language is unknown, the text is above
/// [`HIGHLIGHT_LIMIT`] or a line is above [`HIGHLIGHT_LINE_LIMIT`].
pub fn highlight(text: &str, language: &str) -> Option<Vec<StyledLine>> {
    if text.len() > HIGHLIGHT_LIMIT
        || text
            .split('\n')
            .any(|line| line.len() > HIGHLIGHT_LINE_LIMIT)
    {
        return None;
    }
    let syntax = syntax(language)?;
    let mut state = ParseState::new(syntax);
    let mut stack = ScopeStack::new();
    let mut lines = Vec::new();
    for line in text.split_inclusive('\n') {
        let ops = state.parse_line(line, syntaxes()).ok()?;
        let mut styled: StyledLine = Vec::new();
        let mut at = 0;
        for (offset, op) in ops {
            push_run(&mut styled, token(&stack), &line[at..offset]);
            stack.apply(&op).ok()?;
            at = offset;
        }
        push_run(&mut styled, token(&stack), &line[at..]);
        if let Some((_, last)) = styled.last_mut() {
            let trimmed = last.trim_end_matches(['\n', '\r']).len();
            last.truncate(trimmed);
        }
        lines.push(styled);
    }
    Some(lines)
}

fn push_run(line: &mut StyledLine, token: Token, text: &str) {
    if text.is_empty() {
        return;
    }
    match line.last_mut() {
        Some((last, run)) if *last == token => run.push_str(text),
        _ => line.push((token, text.to_owned())),
    }
}

/// The theme token for the innermost scope that has one.
fn token(stack: &ScopeStack) -> Token {
    stack
        .as_slice()
        .iter()
        .rev()
        .find_map(|scope| scope_token(*scope))
        .unwrap_or(Token::Text)
}

fn scope_token(scope: Scope) -> Option<Token> {
    let name = scope.build_string();
    const MAP: [(&str, Token); 10] = [
        ("comment", Token::Muted),
        ("string", Token::Ok),
        ("constant.numeric", Token::Info),
        ("constant.language", Token::Warn),
        ("constant.character", Token::Warn),
        ("entity.name.tag", Token::Accent),
        ("entity.other.attribute-name", Token::Info),
        ("keyword", Token::Accent),
        ("storage", Token::Accent),
        ("punctuation.definition.tag", Token::Muted),
    ];
    MAP.iter()
        .find(|(prefix, _)| name.starts_with(prefix))
        .map(|(_, token)| *token)
}

/// How many bytes the hex view shows.
pub const HEX_LIMIT: usize = 4096;

/// `bytes` as hex dump lines: offset, 16 bytes in hex, and their ASCII.
pub fn hex_lines(bytes: &[u8]) -> Vec<String> {
    bytes
        .chunks(16)
        .enumerate()
        .map(|(row, chunk)| {
            let hex: Vec<String> = chunk.iter().map(|b| format!("{b:02x}")).collect();
            let ascii: String = chunk
                .iter()
                .map(|&b| {
                    if b.is_ascii_graphic() || b == b' ' {
                        char::from(b)
                    } else {
                        '.'
                    }
                })
                .collect();
            format!("{:08x}  {:<47}  |{ascii}|", row * 16, hex.join(" "))
        })
        .collect()
}

/// `1.2 KiB`, `300 KiB`, `80.0 MiB`, `512 B`.
pub fn size_text(bytes: u64) -> String {
    const UNITS: [&str; 4] = ["KiB", "MiB", "GiB", "TiB"];
    if bytes < 1024 {
        return format!("{bytes} B");
    }
    #[allow(clippy::cast_precision_loss)]
    let mut value = bytes as f64 / 1024.0;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    if value >= 100.0 || value.fract() == 0.0 {
        format!("{value:.0} {}", UNITS[unit])
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lines_are_indexed() {
        let lines = TextLines::new("a\r\nb\n\nc\n".into());
        let all: Vec<&str> = (0..lines.len()).map(|i| lines.line(i)).collect();
        assert_eq!(all, ["a", "b", "", "c"]);
        assert!(TextLines::new(String::new()).is_empty());
    }

    #[test]
    fn content_types_pick_a_language() {
        assert_eq!(language("application/xml; charset=utf-8"), Some("xml"));
        assert_eq!(language("application/atom+xml"), Some("xml"));
        assert_eq!(language("text/html"), Some("html"));
        assert_eq!(language("application/x-yaml"), Some("yaml"));
        assert_eq!(language("text/javascript"), Some("js"));
        assert_eq!(language("text/css"), Some("css"));
        assert_eq!(language("text/plain"), None);
        assert_eq!(language("nonsense"), None);
    }

    #[test]
    fn xml_is_highlighted_by_scope() {
        // Scenario "XML response".
        let lines = highlight("<user id=\"42\"><!-- hi --></user>\n", "xml").unwrap();
        let tokens: Vec<(Token, &str)> = lines[0].iter().map(|(t, s)| (*t, s.as_str())).collect();
        assert!(tokens.contains(&(Token::Accent, "user")), "{tokens:?}");
        assert!(tokens.contains(&(Token::Info, "id")), "{tokens:?}");
        assert!(
            tokens
                .iter()
                .any(|(t, s)| *t == Token::Ok && s.contains("42"))
        );
        assert!(
            tokens
                .iter()
                .any(|(t, s)| *t == Token::Muted && s.contains("hi"))
        );
        for language in ["html", "yaml", "js", "css"] {
            assert!(highlight("a: 1\n", language).is_some(), "{language}");
        }
        assert!(highlight(&"x".repeat(HIGHLIGHT_LIMIT + 1), "xml").is_none());
    }

    #[test]
    fn a_very_long_line_is_not_highlighted() {
        // Regression: a 400 KB one-line HTML body took seconds to highlight.
        let started = std::time::Instant::now();
        let minified = "<div class=\"a\"><span>x</span></div>".repeat(400 * 1024 / 34);
        assert!(minified.len() < HIGHLIGHT_LIMIT);
        assert!(highlight(&minified, "html").is_none());
        assert!(started.elapsed() < std::time::Duration::from_secs(1));
        let line = "x".repeat(HIGHLIGHT_LINE_LIMIT);
        let fits = format!("<p>\n{line}\n</p>\n");
        assert!(highlight(&fits, "html").is_some(), "lines at the limit are");
    }

    #[test]
    fn hex_dump_rows() {
        let rows = hex_lines(b"\x89PNG\r\n\x1a\nabcdefghijklmnop");
        assert_eq!(
            rows[0],
            "00000000  89 50 4e 47 0d 0a 1a 0a 61 62 63 64 65 66 67 68  |.PNG....abcdefgh|"
        );
        assert!(rows[1].starts_with("00000010  69 6a"));
    }

    #[test]
    fn sizes_are_human() {
        assert_eq!(size_text(512), "512 B");
        assert_eq!(size_text(1229), "1.2 KiB");
        assert_eq!(size_text(300 * 1024), "300 KiB");
        assert_eq!(size_text(80 * 1024 * 1024), "80 MiB");
    }
}
