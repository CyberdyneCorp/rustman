//! Pact JSON paths: the keys of body matching rules (`$.id`,
//! `$.items[*].name`, `$['a key']`) and the paths of `json` assertions they
//! are generated from. Only plain member and index steps (and `*` in rule
//! paths) are supported: a filter or a recursive descent has no Pact
//! equivalent.

use std::fmt;

/// One step of a path.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Step {
    Key(String),
    Index(usize),
    /// `*` or `[*]`: any key or index (rule paths only).
    Any,
}

/// A parsed path: the steps below the root `$`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Path(pub Vec<Step>);

impl Path {
    /// Parse `$`, `$.a.b`, `$.a[0]`, `$['a b']`, `$.a[*]` or `$.*`.
    pub fn parse(text: &str) -> Result<Path, String> {
        let unsupported = || format!("unsupported JSON path '{text}'");
        let mut rest = text.strip_prefix('$').ok_or_else(unsupported)?;
        let mut steps = Vec::new();
        while !rest.is_empty() {
            let (step, tail) = next_step(rest).ok_or_else(unsupported)?;
            steps.push(step);
            rest = tail;
        }
        Ok(Path(steps))
    }

    /// A child of this path.
    pub fn child(&self, step: Step) -> Path {
        let mut steps = self.0.clone();
        steps.push(step);
        Path(steps)
    }

    /// Whether this rule path applies to the concrete `path`: the same
    /// length, every step equal or `*`.
    pub fn matches(&self, path: &Path) -> bool {
        self.0.len() == path.0.len()
            && self
                .0
                .iter()
                .zip(&path.0)
                .all(|(rule, step)| *rule == Step::Any || rule == step)
    }

    /// How specific a rule path is: its steps that are not `*`.
    pub fn weight(&self) -> usize {
        self.0.iter().filter(|s| **s != Step::Any).count()
    }
}

fn next_step(rest: &str) -> Option<(Step, &str)> {
    if let Some(tail) = rest.strip_prefix(".*") {
        return Some((Step::Any, tail));
    }
    if let Some(tail) = rest.strip_prefix('.') {
        let end = tail.find(['.', '[']).unwrap_or(tail.len());
        let key = &tail[..end];
        return (!key.is_empty()).then(|| (Step::Key(key.to_owned()), &tail[end..]));
    }
    let tail = rest.strip_prefix('[')?;
    let end = tail.find(']')?;
    let inside = &tail[..end];
    let step = bracket_step(inside)?;
    Some((step, &tail[end + 1..]))
}

fn bracket_step(inside: &str) -> Option<Step> {
    if inside == "*" {
        return Some(Step::Any);
    }
    if let Some(key) = quoted(inside, '\'').or_else(|| quoted(inside, '"')) {
        return Some(Step::Key(key.to_owned()));
    }
    inside.parse().ok().map(Step::Index)
}

fn quoted(text: &str, quote: char) -> Option<&str> {
    text.strip_prefix(quote)?.strip_suffix(quote)
}

impl fmt::Display for Path {
    /// `$.id`, `$.items[0]`, `$['a key']`: plain keys after a dot.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("$")?;
        for step in &self.0 {
            match step {
                Step::Key(key) if is_plain(key) => write!(f, ".{key}")?,
                Step::Key(key) => write!(f, "['{key}']")?,
                Step::Index(index) => write!(f, "[{index}]")?,
                Step::Any => f.write_str("[*]")?,
            }
        }
        Ok(())
    }
}

fn is_plain(key: &str) -> bool {
    !key.is_empty()
        && key
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

/// A JSON pointer (`/a/0`) as a path (`$.a[0]`): array indices are the
/// all-digit tokens.
pub fn from_pointer(pointer: &str) -> Path {
    let steps = pointer
        .split('/')
        .skip(1)
        .map(|token| {
            let token = token.replace("~1", "/").replace("~0", "~");
            match token.parse::<usize>() {
                Ok(index) if !token.starts_with('0') || token == "0" => Step::Index(index),
                _ => Step::Key(token),
            }
        })
        .collect();
    Path(steps)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(k: &str) -> Step {
        Step::Key(k.into())
    }

    #[test]
    fn paths_parse_and_print() {
        let path = Path::parse("$.items[0]['a b'].id").unwrap();
        assert_eq!(
            path.0,
            vec![key("items"), Step::Index(0), key("a b"), key("id")]
        );
        assert_eq!(path.to_string(), "$.items[0]['a b'].id");
        assert_eq!(Path::parse("$").unwrap(), Path::default());
        assert_eq!(Path::parse("$.*").unwrap().0, vec![Step::Any]);
        assert_eq!(Path::parse("$.a[*]").unwrap().to_string(), "$.a[*]");
        for bad in ["id", "$..id", "$[?(@.a)]", "$.", "$[x"] {
            assert!(Path::parse(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn rule_paths_match_with_wildcards_and_weigh_their_literals() {
        let rule = Path::parse("$.items[*].id").unwrap();
        assert!(rule.matches(&Path::parse("$.items[3].id").unwrap()));
        assert!(!rule.matches(&Path::parse("$.items[3]").unwrap()));
        assert!(!rule.matches(&Path::parse("$.other[3].id").unwrap()));
        assert_eq!(rule.weight(), 2);
    }

    #[test]
    fn pointers_become_paths() {
        assert_eq!(from_pointer("/email").to_string(), "$.email");
        assert_eq!(
            from_pointer("/items/0/a~1b").to_string(),
            "$.items[0]['a/b']"
        );
        assert_eq!(from_pointer("").to_string(), "$");
    }
}
