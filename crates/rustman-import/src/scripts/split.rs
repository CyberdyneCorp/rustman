//! Splitting a script into statements without a JavaScript parser: a small
//! scanner that knows strings, comments, parentheses and braces. A brace
//! after `)` or `=>` opens a block whose statements are split; any other
//! brace is an object literal and stays inside its statement.

/// The statements of `source`, trimmed, without comments, in order.
/// Fragments with no letter or digit (such as the `})` closing a
/// `pm.test` callback) are left out.
pub fn split(source: &str) -> Vec<String> {
    let mut scanner = Scanner::default();
    let chars: Vec<char> = source.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        i = scanner.step(&chars, i);
    }
    scanner.flush();
    scanner.out
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Brace {
    Block,
    Object,
}

#[derive(Default)]
struct Scanner {
    out: Vec<String>,
    current: String,
    /// Open parentheses and brackets in the current block.
    depth: usize,
    /// Open braces, with the parenthesis depth to restore for blocks.
    braces: Vec<(Brace, usize)>,
}

impl Scanner {
    /// Consume the token at `i`; returns the index after it.
    fn step(&mut self, chars: &[char], i: usize) -> usize {
        let c = chars[i];
        let next = chars.get(i + 1).copied();
        match c {
            '"' | '\'' | '`' => return self.string(chars, i),
            '/' if next == Some('/') => return skip_line(chars, i),
            '/' if next == Some('*') => return skip_block_comment(chars, i),
            '(' | '[' => self.open_paren(c),
            ')' | ']' => self.close_paren(c),
            '{' => self.open_brace(empty_braces(chars, i)),
            '}' => self.close_brace(),
            ';' if self.at_statement_level() => self.flush(),
            '\n' if self.at_statement_level() && !continues(&self.current, chars, i + 1) => {
                self.flush();
            }
            _ => self.current.push(c),
        }
        i + 1
    }

    fn at_statement_level(&self) -> bool {
        self.depth == 0 && !matches!(self.braces.last(), Some((Brace::Object, _)))
    }

    fn open_paren(&mut self, c: char) {
        self.depth += 1;
        self.current.push(c);
    }

    fn close_paren(&mut self, c: char) {
        self.depth = self.depth.saturating_sub(1);
        self.current.push(c);
    }

    /// Open a block, or (for `{}` and braces not after `)`, `=>`, `else`
    /// or `try`) an object literal that stays in the statement.
    fn open_brace(&mut self, empty: bool) {
        let head = self.current.trim_end();
        let block = head.is_empty() || head.ends_with(')') || head.ends_with("=>");
        if !empty && (block || head.ends_with("else") || head.ends_with("try")) {
            self.flush();
            self.braces.push((Brace::Block, self.depth));
            self.depth = 0;
        } else {
            self.braces.push((Brace::Object, self.depth));
            self.current.push('{');
        }
    }

    fn close_brace(&mut self) {
        match self.braces.pop() {
            Some((Brace::Block, depth)) => {
                self.flush();
                self.depth = depth;
            }
            _ => self.current.push('}'),
        }
    }

    /// Copy the string literal starting at `i`; returns the index after it.
    fn string(&mut self, chars: &[char], i: usize) -> usize {
        let quote = chars[i];
        self.current.push(quote);
        let mut j = i + 1;
        while j < chars.len() {
            let c = chars[j];
            self.current.push(c);
            j += 1;
            if c == '\\' {
                if let Some(escaped) = chars.get(j) {
                    self.current.push(*escaped);
                }
                j += 1;
            } else if c == quote {
                break;
            }
        }
        j
    }

    fn flush(&mut self) {
        let text = self.current.trim();
        if text.chars().any(char::is_alphanumeric) {
            self.out.push(text.to_owned());
        }
        self.current.clear();
    }
}

/// Whether the statement in `current` goes on past the line break: it ends
/// with an operator, or the next line starts with one (method chains).
fn continues(current: &str, chars: &[char], from: usize) -> bool {
    let ends_open = current.trim_end().ends_with([
        '.', ',', '=', '+', '-', '*', '/', '&', '|', '?', ':', '!', '<', '>',
    ]);
    let next = chars[from.min(chars.len())..]
        .iter()
        .find(|c| !c.is_whitespace());
    ends_open || matches!(next, Some('.' | '?' | ':' | '+' | '&' | '|' | ')' | ']'))
}

/// Whether the brace at `i` is closed right away (`{}`).
fn empty_braces(chars: &[char], i: usize) -> bool {
    chars[i + 1..].iter().find(|c| !c.is_whitespace()) == Some(&'}')
}

fn skip_line(chars: &[char], i: usize) -> usize {
    chars[i..]
        .iter()
        .position(|c| *c == '\n')
        .map_or(chars.len(), |n| i + n)
}

fn skip_block_comment(chars: &[char], i: usize) -> usize {
    (i + 2..chars.len().saturating_sub(1))
        .find(|&j| chars[j] == '*' && chars[j + 1] == '/')
        .map_or(chars.len(), |j| j + 2)
}

/// `statement` with whitespace collapsed outside string literals: a space
/// is kept only between two word characters (`const x`), and single-quoted
/// strings become double-quoted, so patterns match one spelling.
pub fn normalize(statement: &str) -> String {
    let chars: Vec<char> = statement.chars().collect();
    let mut out = String::with_capacity(statement.len());
    let mut i = 0;
    let mut pending_space = false;
    while i < chars.len() {
        let c = chars[i];
        if c.is_whitespace() {
            pending_space = true;
            i += 1;
            continue;
        }
        if pending_space && is_word(out.chars().last()) && is_word(Some(c)) {
            out.push(' ');
        }
        pending_space = false;
        if matches!(c, '"' | '\'') {
            i = quoted(&chars, i, &mut out);
        } else {
            out.push(c);
            i += 1;
        }
    }
    out
}

fn is_word(c: Option<char>) -> bool {
    c.is_some_and(|c| c.is_alphanumeric() || c == '_' || c == '$')
}

/// Copy the string literal at `i` into `out` as a double-quoted string;
/// returns the index after it.
fn quoted(chars: &[char], i: usize, out: &mut String) -> usize {
    let quote = chars[i];
    out.push('"');
    let mut j = i + 1;
    while j < chars.len() && chars[j] != quote {
        match chars[j] {
            '\\' if chars.get(j + 1) == Some(&'\'') => {
                out.push('\'');
                j += 1;
            }
            '\\' => {
                out.push('\\');
                if let Some(escaped) = chars.get(j + 1) {
                    out.push(*escaped);
                }
                j += 1;
            }
            '"' => out.push_str("\\\""),
            c => out.push(c),
        }
        j += 1;
    }
    out.push('"');
    j + 1
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_callbacks_are_split_into_their_statements() {
        let source = "pm.test(\"ok\", function () {\n    pm.response.to.have.status(200);\n    pm.expect(x).to.eql({a: 1});\n});\n// done\npm.test('one', () => pm.response.to.be.ok);";
        assert_eq!(
            split(source),
            [
                "pm.test(\"ok\", function ()",
                "pm.response.to.have.status(200)",
                "pm.expect(x).to.eql({a: 1})",
                "pm.test('one', () => pm.response.to.be.ok)",
            ]
        );
    }

    #[test]
    fn lines_without_semicolons_and_chains_split_correctly() {
        let source = "const d = pm.response.json()\npm.expect(d.id)\n  .to.eql(1)\nif (d.ok) { postman.setNextRequest(\"x\") }";
        assert_eq!(
            split(source),
            [
                "const d = pm.response.json()",
                "pm.expect(d.id)\n  .to.eql(1)",
                "if (d.ok)",
                "postman.setNextRequest(\"x\")",
            ]
        );
    }

    #[test]
    fn strings_and_comments_do_not_split() {
        let source = "pm.expect(t).to.include(\"a;b // c {\"); /* x; y */ z = '}'";
        assert_eq!(
            split(source),
            ["pm.expect(t).to.include(\"a;b // c {\")", "z = '}'"]
        );
        assert_eq!(
            split("pm.sendRequest(u, () => {});"),
            ["pm.sendRequest(u, () => {})"]
        );
    }

    #[test]
    fn normalizing_collapses_space_and_quotes() {
        assert_eq!(
            normalize("const  body = pm.response.json( )"),
            "const body=pm.response.json()"
        );
        assert_eq!(
            normalize("pm.expect(t).to.include( 'it\\'s \"x\" a b' )"),
            "pm.expect(t).to.include(\"it's \\\"x\\\" a b\")"
        );
    }
}
