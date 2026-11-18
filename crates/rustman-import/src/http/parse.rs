//! The `.http` / `.rest` grammar (design D4: hand-written, with line
//! numbers): requests separated by `###` lines, each a request line,
//! headers, a blank line and a body; `@name = value` file variables;
//! `# @name x` request names; JetBrains `> {% ... %}` response handlers.

use crate::common::{self, header, path_of, split_url, text_body};
use crate::ir::{ImportedBody, ImportedRequest, Location, Note, NoteKind, Pair, Script, Variable};

const METHODS: &[&str] = &[
    "GET", "POST", "PUT", "PATCH", "DELETE", "HEAD", "OPTIONS", "TRACE", "CONNECT",
];

/// The requests, file variables and notes of one file.
#[derive(Debug, Default)]
pub(super) struct Parsed {
    pub requests: Vec<ImportedRequest>,
    pub variables: Vec<Variable>,
    pub notes: Vec<Note>,
}

/// Whether `line` is a request line with a standard method.
pub(super) fn is_request_line(line: &str) -> bool {
    let mut words = line.split_whitespace();
    let method = words.next().unwrap_or_default();
    METHODS.contains(&method) && words.next().is_some()
}

/// A run of lines between `###` separators.
struct Segment<'a> {
    /// The 0-based index of the first line.
    start: usize,
    name: Option<String>,
    lines: &'a [&'a str],
}

pub(super) fn parse(text: &str, file: &str) -> Parsed {
    let lines: Vec<&str> = text.lines().map(|l| l.trim_end_matches('\r')).collect();
    let mut out = Parsed::default();
    for segment in segments(&lines) {
        let mut parser = RequestParser {
            file,
            out: &mut out,
            segment: &segment,
            index: 0,
        };
        if let Some(request) = parser.request() {
            out.requests.push(request);
        }
    }
    out
}

fn segments<'a>(lines: &'a [&'a str]) -> Vec<Segment<'a>> {
    let mut out = Vec::new();
    let mut start = 0;
    let mut name = None;
    for (i, line) in lines.iter().enumerate() {
        if let Some(rest) = line.strip_prefix("###") {
            out.push(Segment {
                start,
                name: name.take(),
                lines: &lines[start..i],
            });
            start = i + 1;
            name = Some(rest.trim().to_owned()).filter(|n| !n.is_empty());
        }
    }
    out.push(Segment {
        start,
        name,
        lines: &lines[start..],
    });
    out
}

fn is_comment(line: &str) -> bool {
    line.starts_with('#') || line.starts_with("//")
}

/// The name of a `# @name x` (or `// @name=x`) line.
fn meta_name(line: &str) -> Option<String> {
    let rest = line
        .strip_prefix('#')
        .or_else(|| line.strip_prefix("//"))?
        .trim_start();
    let name = rest.strip_prefix("@name")?;
    let name = name.trim_start().trim_start_matches('=').trim();
    (!name.is_empty()).then(|| name.to_owned())
}

/// A `@name = value` file variable.
fn file_variable(line: &str) -> Option<Variable> {
    let (name, value) = line.strip_prefix('@')?.split_once('=')?;
    let name = name.trim();
    let valid = !name.is_empty()
        && name
            .chars()
            .all(|c| c.is_alphanumeric() || "_-.".contains(c));
    valid.then(|| Variable {
        name: name.to_owned(),
        value: value.trim().to_owned(),
        secret: false,
        path: false,
    })
}

struct RequestParser<'p, 'a> {
    file: &'p str,
    out: &'p mut Parsed,
    segment: &'p Segment<'a>,
    index: usize,
}

impl<'a> RequestParser<'_, 'a> {
    fn lines(&self) -> &'a [&'a str] {
        self.segment.lines
    }

    fn line_no(&self, index: usize) -> u32 {
        u32::try_from(self.segment.start + index + 1).unwrap_or(u32::MAX)
    }

    fn at(&self, index: usize) -> Location {
        Location {
            file: self.file.to_owned(),
            item: None,
            line: Some(self.line_no(index)),
        }
    }

    /// Skip blank lines, comments and file variables up to the request
    /// line; returns the request name given by `# @name`, if any.
    fn preamble(&mut self) -> Option<String> {
        let mut name = None;
        while let Some(line) = self.lines().get(self.index).map(|l| l.trim()) {
            if let Some(n) = meta_name(line) {
                name = Some(n);
            } else if let Some(var) = file_variable(line) {
                self.out.variables.push(var);
            } else if !line.is_empty() && !is_comment(line) {
                break;
            }
            self.index += 1;
        }
        name
    }

    fn request(&mut self) -> Option<ImportedRequest> {
        let meta = self.preamble();
        let request_index = self.index;
        let line = self.lines().get(request_index)?.trim();
        let (method, mut url) = request_line(line, &self.at(request_index), &mut self.out.notes);
        self.index += 1;
        let mut headers = Vec::new();
        while let Some(line) = self.lines().get(self.index).map(|l| l.trim()) {
            if line.is_empty() {
                break;
            }
            if line.starts_with('?') || line.starts_with('&') {
                url.push_str(line);
            } else if let Some((name, value)) = line.split_once(':').filter(|_| !is_comment(line)) {
                headers.push(Pair::new(name.trim(), value.trim()));
            }
            self.index += 1;
        }
        let (body_text, scripts) = self.body();
        let (base, query) = split_url(&url);
        let name = self
            .segment
            .name
            .clone()
            .or(meta)
            .unwrap_or_else(|| format!("{method} {}", path_of(&base)));
        let body = body(&body_text, &headers);
        Some(ImportedRequest {
            name,
            method,
            url: base,
            query,
            headers,
            body,
            scripts,
            location: self.at(request_index),
            ..ImportedRequest::default()
        })
    }

    /// The body text after the blank line, and the response handlers.
    fn body(&mut self) -> (String, Vec<Script>) {
        let mut body = Vec::new();
        let mut scripts = Vec::new();
        while let Some(line) = self.lines().get(self.index).copied() {
            let start = self.index;
            self.index += 1;
            if let Some(rest) = line.strip_prefix("> {%") {
                let source = self.handler(rest);
                scripts.push(Script {
                    event: "response-handler".to_owned(),
                    source,
                    location: self.at(start),
                });
            } else if line.starts_with("> ") {
                let message = "response handler file unsupported";
                self.out
                    .notes
                    .push(Note::new(NoteKind::Unsupported, message, &self.at(start)));
            } else if !line.starts_with("<> ") {
                body.push(line);
            }
        }
        let text = body.join("\n").trim_matches(['\n', ' ']).to_owned();
        (text, scripts)
    }

    /// The source of a `> {% ... %}` handler that began with `first`.
    fn handler(&mut self, first: &str) -> String {
        let mut source = Vec::new();
        let mut line = first;
        loop {
            if let Some((code, _)) = line.split_once("%}") {
                source.push(code);
                break;
            }
            source.push(line);
            let Some(next) = self.lines().get(self.index).copied() else {
                break;
            };
            self.index += 1;
            line = next;
        }
        source.join("\n").trim().to_owned()
    }
}

/// The method and URL of a request line (`GET url HTTP/1.1`, or a bare URL).
fn request_line(line: &str, at: &Location, notes: &mut Vec<Note>) -> (String, String) {
    let words: Vec<&str> = line.split_whitespace().collect();
    let (method, rest) = match words.split_first() {
        Some((first, rest)) if METHODS.contains(first) => {
            (common::method(Some(first), at, notes), rest)
        }
        _ => ("GET".to_owned(), words.as_slice()),
    };
    let url = rest
        .iter()
        .take_while(|w| !w.starts_with("HTTP/"))
        .copied()
        .collect::<Vec<_>>()
        .join(" ");
    (method, url)
}

/// `< ./file` is a file body; a form `Content-Type` makes a form; JSON by
/// `Content-Type`, or by a body that starts like JSON when there is none.
fn body(text: &str, headers: &[Pair]) -> Option<ImportedBody> {
    if let Some(path) = text.strip_prefix("< ") {
        return Some(ImportedBody::File {
            path: path.trim().to_owned(),
        });
    }
    let content_type = header(headers, "content-type").unwrap_or_default();
    if content_type.contains("x-www-form-urlencoded") && !text.is_empty() {
        let joined: String = text.lines().map(str::trim).collect();
        return Some(ImportedBody::Form {
            fields: common::query_pairs(&joined),
        });
    }
    let looks_json = content_type.is_empty() && (text.starts_with('{') || text.starts_with('['));
    let mime = if looks_json {
        "application/json"
    } else {
        content_type
    };
    text_body(mime, text)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn requests_split_on_separators() {
        let text = "@host = https://api.test\n\nGET {{host}}/a\nAccept: application/json\n\n###\n\nPOST {{host}}/b HTTP/1.1\nContent-Type: application/json\n\n{\"x\": 1}\n";
        let parsed = parse(text, "a.http");
        assert_eq!(parsed.variables[0].name, "host");
        let [a, b] = parsed.requests.as_slice() else {
            panic!("{parsed:?}");
        };
        assert_eq!((a.method.as_str(), a.url.as_str()), ("GET", "{{host}}/a"));
        assert_eq!(a.name, "GET /a");
        assert_eq!(a.body, None);
        assert_eq!(b.name, "POST /b");
        assert_eq!(b.location.line, Some(8));
        assert_eq!(
            b.body,
            Some(ImportedBody::Json {
                text: "{\"x\": 1}".into()
            })
        );
    }

    #[test]
    fn names_come_from_separators_or_name_comments() {
        let text = "### First\nGET https://x/a\n\n###\n# @name second\nGET https://x/b\n\n###\n// @name=third\nhttps://x/c\n";
        let names: Vec<String> = parse(text, "a.http")
            .requests
            .into_iter()
            .map(|r| r.name)
            .collect();
        assert_eq!(names, ["First", "second", "third"]);
    }

    #[test]
    fn query_continuations_forms_files_and_handlers() {
        let text = "POST https://x/a\n  ?page=1\n  &size=2\nContent-Type: application/x-www-form-urlencoded\n\na=1&\nb=2\n\n> {%\n  client.global.set(\"t\", response.body.t);\n%}\n###\nPUT https://x/b\n\n< ./body.json\n";
        let parsed = parse(text, "a.http");
        let a = &parsed.requests[0];
        assert_eq!(a.query, [Pair::new("page", "1"), Pair::new("size", "2")]);
        assert_eq!(
            a.body,
            Some(ImportedBody::Form {
                fields: vec![Pair::new("a", "1"), Pair::new("b", "2")]
            })
        );
        assert_eq!(
            a.scripts[0].source,
            "client.global.set(\"t\", response.body.t);"
        );
        assert_eq!(
            parsed.requests[1].body,
            Some(ImportedBody::File {
                path: "./body.json".into()
            })
        );
    }

    #[test]
    fn segments_without_a_request_are_skipped() {
        let parsed = parse("# only a comment\n###\n\n", "a.http");
        assert!(parsed.requests.is_empty());
    }
}
