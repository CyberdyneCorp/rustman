//! Reading and writing workspace YAML (design D4).
//!
//! Every YAML file Rustman reads goes through [`from_yaml`], so the parser
//! options from D4 are applied in one place: only `true`/`false` are booleans,
//! numbers with a leading zero or an underscore stay strings, duplicate keys
//! are errors, and the default resource budget stays on. The parser crate is not visible outside
//! this module, so swapping it is a one-module change.

mod json;
mod located;
pub mod write;

use std::fmt::Display;
use std::path::Path;

use serde::de::DeserializeOwned;
use serde_saphyr::options::DuplicateKeyPolicy;

use crate::error::{LoadError, Position};

pub use json::JsonValue;
pub(crate) use json::{MaybeFloat, exact_located};
pub use located::Located;

fn options() -> serde_saphyr::Options {
    serde_saphyr::options! {
        strict_booleans: true,
        leading_zero_numbers_as_strings: true,
        underscored_numbers_as_strings: true,
        duplicate_keys: DuplicateKeyPolicy::Error,
        with_snippet: false,
    }
}

/// Parse `text` (the contents of `file`) into `T`.
///
/// Errors carry the exact line and column reported by the parser.
pub fn from_yaml<T: DeserializeOwned>(file: &Path, text: &str) -> Result<T, LoadError> {
    let _source = json::SourceGuard::set(text);
    serde_saphyr::from_str_with_options(text, options()).map_err(|err| to_load_error(file, &err))
}

fn to_load_error(file: &Path, err: &serde_saphyr::Error) -> LoadError {
    let rendered = err.to_string();
    let Some(position) = err.location().and_then(position_of) else {
        let (position, message) = split_marked(&rendered);
        return LoadError::new(file, position, message);
    };
    let suffix = format!(" at line {}, column {}", position.line, position.column);
    let message = rendered.strip_suffix(&suffix).unwrap_or(&rendered);
    match split_marked(message) {
        (Some(marked), message) => LoadError::new(file, Some(marked), message),
        (None, message) => LoadError::new(file, Some(position), message),
    }
}

/// Delimits a position embedded by [`error_at`]. Printable, because the
/// parser escapes control characters in error messages; a user-written look-alike
/// can at worst move the reported position.
const MARK: &str = "\u{2063}@@";

/// The message of an error raised while converting a value (a serde
/// `try_from`), reported at `position` instead of where the parser stopped,
/// which for a mapping is its start rather than the offending key.
pub(crate) fn error_at(position: Option<Position>, message: impl Display) -> String {
    match position {
        Some(p) => format!("{MARK}{}:{}{MARK}{message}", p.line, p.column),
        None => message.to_string(),
    }
}

/// Take the position embedded by [`error_at`] out of `message`.
fn split_marked(message: &str) -> (Option<Position>, String) {
    let parsed = message.split_once(MARK).and_then(|(before, rest)| {
        let (at, after) = rest.split_once(MARK)?;
        let (line, column) = at.split_once(':')?;
        let position = Position {
            line: line.parse().ok()?,
            column: column.parse().ok()?,
        };
        Some((position, format!("{before}{after}")))
    });
    match parsed {
        Some((position, text)) => (Some(position), text),
        None => (None, message.to_owned()),
    }
}

fn position_of(location: serde_saphyr::Location) -> Option<Position> {
    let line = usize::try_from(location.line()).ok()?;
    let column = usize::try_from(location.column()).ok()?;
    (line > 0).then_some(Position { line, column })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::Deserialize;

    #[derive(Debug, Deserialize, PartialEq)]
    #[serde(deny_unknown_fields)]
    struct Doc {
        a: serde_json::Value,
    }

    fn parse(text: &str) -> Result<Doc, LoadError> {
        from_yaml(Path::new("f.yaml"), text)
    }

    #[test]
    fn yaml_1_1_scalars_stay_strings() {
        let doc = parse("a: [no, on, yes, off, 0755, true, false, 1.5, 7]\n").unwrap();
        assert_eq!(
            doc.a,
            serde_json::json!(["no", "on", "yes", "off", "0755", true, false, 1.5, 7])
        );
    }

    #[test]
    fn unknown_field_has_exact_position() {
        let err = parse("a: 1\nb: 2\n").unwrap_err();
        assert_eq!(err.position, Some(Position { line: 2, column: 1 }));
        assert!(err.message.starts_with("unknown field `b`"), "{err}");
        assert_eq!(err.to_string(), format!("f.yaml:2: {}", err.message));
    }

    #[test]
    fn duplicate_keys_are_rejected() {
        let err = parse("a: 1\na: 2\n").unwrap_err();
        assert_eq!(err.position, Some(Position { line: 2, column: 1 }));
        assert!(err.message.starts_with("duplicate mapping key"), "{err}");
    }

    #[test]
    fn syntax_errors_have_exact_position() {
        let err = parse("a: 'open\n").unwrap_err();
        assert_eq!(err.position, Some(Position { line: 1, column: 4 }));
        assert_eq!(err.message, "unclosed quote");
    }

    #[test]
    fn marked_positions_are_extracted() {
        let at = Some(Position { line: 3, column: 7 });
        assert_eq!(split_marked(&error_at(at, "bad")), (at, "bad".to_owned()));
        assert_eq!(
            split_marked(&error_at(None, "bad")),
            (None, "bad".to_owned())
        );
        assert_eq!(split_marked("plain"), (None, "plain".to_owned()));
    }

    #[test]
    fn alias_bomb_is_rejected_by_the_budget() {
        let mut text = String::from("x0: &a0 [lol, lol, lol, lol, lol, lol, lol, lol, lol]\n");
        for i in 1..10 {
            let prev = i - 1;
            let refs = vec![format!("*a{prev}"); 9].join(", ");
            text.push_str(&format!("x{i}: &a{i} [{refs}]\n"));
        }
        text.push_str("a: *a9\n");
        assert!(from_yaml::<serde_json::Value>(Path::new("f.yaml"), &text).is_err());
    }
}
