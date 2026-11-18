//! Syntax classes of a document's tokens, for editors that colour it.
//!
//! The lexer never fails, so a half-typed document still gets classes up
//! to the cursor; a token the lexer rejects (an unterminated string) is
//! left uncoloured.

use apollo_parser::{Lexer, TokenKind};

/// How a token is drawn.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TokenClass {
    /// `query`, `mutation`, `subscription`, `fragment`, `on`, `true`...
    Keyword,
    /// A field, argument, type or alias name.
    Name,
    /// `$id`.
    Variable,
    /// `@include`.
    Directive,
    String,
    Number,
    Comment,
    /// Braces, parentheses, `:`, `!`, `...`.
    Punctuation,
}

/// One classified token: a byte range of the document.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Highlight {
    pub start: usize,
    pub end: usize,
    pub class: TokenClass,
}

const KEYWORDS: [&str; 11] = [
    "query",
    "mutation",
    "subscription",
    "fragment",
    "on",
    "true",
    "false",
    "null",
    "schema",
    "type",
    "input",
];

/// The classes of every significant token of `document`, in order.
pub fn highlight(document: &str) -> Vec<Highlight> {
    let mut out: Vec<Highlight> = Vec::new();
    let mut previous: Option<TokenKind> = None;
    for token in Lexer::new(document).filter_map(Result::ok) {
        let kind = token.kind();
        let text = token.data();
        let start = token.index();
        let class = match kind {
            TokenKind::Whitespace | TokenKind::Comma | TokenKind::Eof => continue,
            TokenKind::Name => name_class(text, previous),
            other => other_class(other),
        };
        // `$` and `@` join the name that follows.
        let joined = matches!(class, TokenClass::Variable | TokenClass::Directive)
            && out
                .last()
                .is_some_and(|h| h.end == start && h.class == class);
        match out.last_mut() {
            Some(last) if joined => last.end = start + text.len(),
            _ => out.push(Highlight {
                start,
                end: start + text.len(),
                class,
            }),
        }
        previous = Some(kind);
    }
    out
}

fn name_class(text: &str, previous: Option<TokenKind>) -> TokenClass {
    match previous {
        Some(TokenKind::Dollar) => TokenClass::Variable,
        Some(TokenKind::At) => TokenClass::Directive,
        _ if KEYWORDS.contains(&text) => TokenClass::Keyword,
        _ => TokenClass::Name,
    }
}

fn other_class(kind: TokenKind) -> TokenClass {
    match kind {
        TokenKind::Dollar => TokenClass::Variable,
        TokenKind::At => TokenClass::Directive,
        TokenKind::StringValue => TokenClass::String,
        TokenKind::Int | TokenKind::Float => TokenClass::Number,
        TokenKind::Comment => TokenClass::Comment,
        _ => TokenClass::Punctuation,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn classes(document: &str) -> Vec<(&str, TokenClass)> {
        highlight(document)
            .into_iter()
            .map(|h| (&document[h.start..h.end], h.class))
            .collect()
    }

    #[test]
    fn every_token_kind_gets_its_class() {
        use TokenClass::*;
        let document =
            "query Q($id: ID!) { user(id: $id, n: 2) @skip(if: false) { name } } # c\n\"s\"";
        assert_eq!(
            classes(document),
            [
                ("query", Keyword),
                ("Q", Name),
                ("(", Punctuation),
                ("$id", Variable),
                (":", Punctuation),
                ("ID", Name),
                ("!", Punctuation),
                (")", Punctuation),
                ("{", Punctuation),
                ("user", Name),
                ("(", Punctuation),
                ("id", Name),
                (":", Punctuation),
                ("$id", Variable),
                ("n", Name),
                (":", Punctuation),
                ("2", Number),
                (")", Punctuation),
                ("@skip", Directive),
                ("(", Punctuation),
                ("if", Name),
                (":", Punctuation),
                ("false", Keyword),
                (")", Punctuation),
                ("{", Punctuation),
                ("name", Name),
                ("}", Punctuation),
                ("}", Punctuation),
                ("# c", Comment),
                ("\"s\"", String),
            ]
        );
    }

    #[test]
    fn a_half_typed_document_is_still_classified() {
        let found = classes("{ user(id: \"unterminated");
        assert_eq!(found[0], ("{", TokenClass::Punctuation));
        assert_eq!(found[1], ("user", TokenClass::Name));
        assert!(highlight("").is_empty());
    }
}
