//! The `.bru` grammar (design D4: hand-written, with line numbers).
//!
//! A file is a list of top-level blocks. `name {` opens a block closed by a
//! `}` line at column 0; `name [` opens a list closed by `]`. Text blocks
//! (bodies, scripts, tests, docs) keep their content with the two-space
//! indent removed; every other block holds `key: value` lines, where a `~`
//! prefix marks a disabled entry.

use crate::error::ImportError;

/// One top-level block.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Block {
    pub name: String,
    /// The 1-based line of the block header.
    pub line: u32,
    pub content: Content,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Content {
    Dict(Vec<Entry>),
    Text(String),
    List(Vec<String>),
}

/// A `key: value` line of a dictionary block.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    pub key: String,
    pub value: String,
    pub enabled: bool,
    pub line: u32,
}

impl Block {
    /// The enabled entries of a dictionary block (none for other blocks).
    pub fn entries(&self) -> impl Iterator<Item = &Entry> {
        let all: &[Entry] = match &self.content {
            Content::Dict(entries) => entries,
            _ => &[],
        };
        all.iter()
    }

    /// The value of the enabled entry `key`.
    pub fn get(&self, key: &str) -> Option<&str> {
        self.entries()
            .find(|e| e.enabled && e.key == key)
            .map(|e| e.value.as_str())
    }

    /// The text of a text block (empty for other blocks).
    pub fn text(&self) -> &str {
        match &self.content {
            Content::Text(text) => text,
            _ => "",
        }
    }
}

/// Blocks whose content is free text rather than `key: value` lines.
fn is_text_block(name: &str) -> bool {
    matches!(
        name,
        "body:json"
            | "body:text"
            | "body:xml"
            | "body:sparql"
            | "body:graphql"
            | "body:graphql:vars"
            | "script:pre-request"
            | "script:post-response"
            | "tests"
            | "docs"
    )
}

/// Parse the `.bru` text of `file` into its blocks.
pub fn parse(text: &str, file: &str) -> Result<Vec<Block>, ImportError> {
    let lines: Vec<&str> = text.lines().map(|l| l.trim_end_matches('\r')).collect();
    let mut blocks = Vec::new();
    let mut i = 0;
    while i < lines.len() {
        let line = lines[i].trim_end();
        if line.trim().is_empty() {
            i += 1;
            continue;
        }
        let header = line_no(i);
        let (name, close) = open(line).ok_or_else(|| {
            error(
                file,
                header,
                format!(
                    "expected a block such as `meta {{`, found `{}`",
                    line.trim()
                ),
            )
        })?;
        let end = (i + 1..lines.len())
            .find(|&j| lines[j].trim_end() == close)
            .ok_or_else(|| error(file, header, format!("block `{name}` is not closed")))?;
        let body = &lines[i + 1..end];
        let content = match close {
            "]" => Content::List(list(body)),
            _ if is_text_block(name) => Content::Text(text_content(body)),
            _ => Content::Dict(dict(body, i + 1, file)?),
        };
        blocks.push(Block {
            name: name.to_owned(),
            line: header,
            content,
        });
        i = end + 1;
    }
    Ok(blocks)
}

fn line_no(index: usize) -> u32 {
    u32::try_from(index + 1).unwrap_or(u32::MAX)
}

fn error(file: &str, line: u32, message: String) -> ImportError {
    ImportError::invalid(file, format!("line {line}: {message}"))
}

/// The name of a block opened by `line`, and the line that closes it.
fn open(line: &str) -> Option<(&str, &'static str)> {
    if line.starts_with(char::is_whitespace) {
        return None;
    }
    let (name, close) = if let Some(name) = line.strip_suffix('{') {
        (name, "}")
    } else {
        (line.strip_suffix('[')?, "]")
    };
    let name = name.trim();
    let valid = !name.is_empty() && !name.contains(char::is_whitespace);
    valid.then_some((name, close))
}

/// Text block content: the lines with their two-space indent removed.
fn text_content(body: &[&str]) -> String {
    let lines: Vec<&str> = body
        .iter()
        .map(|l| l.strip_prefix("  ").unwrap_or(l.trim_start()))
        .collect();
    lines.join("\n").trim_matches('\n').to_owned()
}

fn list(body: &[&str]) -> Vec<String> {
    body.iter()
        .flat_map(|l| l.split(','))
        .map(str::trim)
        .filter(|item| !item.is_empty())
        .map(str::to_owned)
        .collect()
}

fn dict(body: &[&str], first: usize, file: &str) -> Result<Vec<Entry>, ImportError> {
    let mut out = Vec::new();
    for (offset, raw) in body.iter().enumerate() {
        let line = raw.trim();
        if line.is_empty() {
            continue;
        }
        let number = line_no(first + offset);
        let (enabled, line) = match line.strip_prefix('~') {
            Some(rest) => (false, rest),
            None => (true, line),
        };
        let (key, value) = line.split_once(':').ok_or_else(|| {
            error(
                file,
                number,
                format!("expected `key: value`, found `{line}`"),
            )
        })?;
        out.push(Entry {
            key: key.trim().to_owned(),
            value: value.trim().to_owned(),
            enabled,
            line: number,
        });
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "meta {\n  name: Get user\n  seq: 2\n}\n\nget {\n  url: {{base}}/users/1\n  body: json\n}\r\n\r\nheaders {\n  Accept: application/json\n  ~X-Debug: 1\n}\n\nbody:json {\n  {\n    \"a\": 1\n  }\n}\n\nvars:secret [\n  token,\n  password\n]\n";

    #[test]
    fn blocks_keep_entries_text_and_lines() {
        let blocks = parse(SAMPLE, "a.bru").unwrap();
        let names: Vec<&str> = blocks.iter().map(|b| b.name.as_str()).collect();
        assert_eq!(
            names,
            ["meta", "get", "headers", "body:json", "vars:secret"]
        );
        assert_eq!(blocks[0].get("seq"), Some("2"));
        assert_eq!(blocks[1].get("url"), Some("{{base}}/users/1"));
        assert_eq!(blocks[1].line, 6);
        let headers: Vec<_> = blocks[2].entries().collect();
        assert_eq!(headers.len(), 2);
        assert!(!headers[1].enabled);
        assert_eq!(headers[1].line, 13);
        assert_eq!(blocks[3].text(), "{\n  \"a\": 1\n}");
        assert_eq!(
            blocks[4].content,
            Content::List(vec!["token".into(), "password".into()])
        );
    }

    #[test]
    fn an_unclosed_block_names_its_line() {
        let err = parse("meta {\n  name: x\n}\n\nheaders {\n  A: b\n", "bad.bru").unwrap_err();
        assert_eq!(
            err.to_string(),
            "import: bad.bru: line 5: block `headers` is not closed"
        );
    }

    #[test]
    fn stray_text_and_entries_without_a_colon_are_errors() {
        let err = parse("meta {\n}\noops\n", "bad.bru").unwrap_err();
        assert!(
            err.to_string().contains("line 3: expected a block"),
            "{err}"
        );
        let err = parse("headers {\n  no colon here\n}\n", "bad.bru").unwrap_err();
        assert!(
            err.to_string().contains("line 2: expected `key: value`"),
            "{err}"
        );
    }
}
