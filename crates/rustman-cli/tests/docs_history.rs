//! The examples in docs/history.md (task 7.4).
//!
//! Files in ```yaml / ```toml blocks (first line `# <path>`) are written into
//! one workspace whose `base_url` points at the in-process test server, and
//! the secret `api_token` is set for `dev`. Every `$ rustman ...` line of a
//! ```console block runs in document order and must print the lines that
//! follow it (`error:` lines on stderr, the rest on stdout; a command with
//! an `error:` line must fail). Ids, times, durations and the server address
//! vary from run to run, so both sides are normalized before comparing, and
//! an id shown in the document stands for the id printed at that place.

mod common;

use std::collections::HashMap;
use std::fs;
use std::path::Path;

use common::{Server, rustman};
use regex::Regex;
use rustman_test_support::routes::TOKEN;

const DOC_BASE_URL: &str = "http://localhost:8080";

fn doc() -> String {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/history.md");
    fs::read_to_string(path).unwrap()
}

/// The fenced blocks of `text` as (language, lines).
fn blocks(text: &str) -> Vec<(String, Vec<String>)> {
    let mut out = Vec::new();
    let mut lines = text.lines();
    while let Some(line) = lines.next() {
        if let Some(lang) = line.strip_prefix("```").filter(|l| !l.is_empty()) {
            let body = lines
                .by_ref()
                .take_while(|l| *l != "```")
                .map(str::to_owned)
                .collect();
            out.push((lang.to_owned(), body));
        }
    }
    out
}

/// One `$ rustman ...` line and the output that follows it.
#[derive(Default)]
struct Step {
    args: Vec<String>,
    stdout: String,
    stderr: String,
}

fn steps(lines: &[String]) -> Vec<Step> {
    let mut steps: Vec<Step> = Vec::new();
    for line in lines {
        if let Some(command) = line.strip_prefix("$ ") {
            let words = shell_words::split(command).unwrap();
            assert_eq!(words[0], "rustman", "{line}");
            steps.push(Step {
                args: words[1..].to_vec(),
                ..Step::default()
            });
            continue;
        }
        let step = steps.last_mut().expect("output before any command");
        let out = if line.starts_with("error:") {
            &mut step.stderr
        } else {
            &mut step.stdout
        };
        out.push_str(line);
        out.push('\n');
    }
    steps
}

struct Normalizer {
    ulid: Regex,
    time: Regex,
    millis: Regex,
    base_url: String,
}

impl Normalizer {
    fn new(base_url: String) -> Self {
        Self {
            ulid: Regex::new(r"\b[0-9A-HJKMNP-TV-Z]{26}\b").unwrap(),
            time: Regex::new(r"\d{4}-\d\d-\d\dT\d\d:\d\d:\d\d(\.\d+)?Z").unwrap(),
            millis: Regex::new(r"\b\d+ ms\b").unwrap(),
            base_url,
        }
    }

    fn ids<'t>(&self, text: &'t str) -> Vec<&'t str> {
        self.ulid.find_iter(text).map(|m| m.as_str()).collect()
    }

    fn apply(&self, text: &str) -> String {
        let text = text.replace(&self.base_url, DOC_BASE_URL);
        let text = self.ulid.replace_all(&text, "<id>");
        let text = self.time.replace_all(&text, "<time>");
        let text = self.millis.replace_all(&text, "<n> ms");
        text.trim_end().to_owned()
    }
}

fn write_files(root: &Path, blocks: &[(String, Vec<String>)], base_url: &str) -> usize {
    let mut written = 0;
    for (lang, lines) in blocks.iter().filter(|(l, _)| l == "yaml" || l == "toml") {
        let path = lines[0]
            .strip_prefix("# ")
            .unwrap_or_else(|| panic!("{lang} block without a path comment: {lines:?}"));
        let file = root.join(path);
        fs::create_dir_all(file.parent().unwrap()).unwrap();
        let text = lines[1..].join("\n").replace(DOC_BASE_URL, base_url);
        fs::write(file, text + "\n").unwrap();
        written += 1;
    }
    written
}

#[test]
fn doc_examples_run() {
    let server = Server::http();
    let base_url = server.url("");
    let blocks = blocks(&doc());
    let ws = tempfile::tempdir().unwrap();
    assert_eq!(write_files(ws.path(), &blocks, &base_url), 3);
    rustman(ws.path())
        .args(["secret", "set", "api_token", "--env", "dev"])
        .write_stdin(TOKEN)
        .assert()
        .success();
    let normalize = Normalizer::new(base_url);
    let mut ids: HashMap<String, String> = HashMap::new();
    let mut ran = 0;
    for (_, lines) in blocks.iter().filter(|(l, _)| l == "console") {
        for step in steps(lines) {
            let args: Vec<String> = step
                .args
                .iter()
                .map(|a| ids.get(a).cloned().unwrap_or_else(|| a.clone()))
                .collect();
            let output = rustman(ws.path()).args(&args).output().unwrap();
            let stdout = String::from_utf8(output.stdout).unwrap();
            let stderr = String::from_utf8(output.stderr).unwrap();
            assert_eq!(
                output.status.success(),
                step.stderr.is_empty(),
                "{args:?}: {stderr}"
            );
            assert_eq!(
                normalize.apply(&stdout),
                normalize.apply(&step.stdout),
                "{args:?}"
            );
            assert_eq!(
                normalize.apply(&stderr),
                normalize.apply(&step.stderr),
                "{args:?}"
            );
            for (shown, actual) in normalize
                .ids(&step.stdout)
                .into_iter()
                .zip(normalize.ids(&stdout))
            {
                ids.entry(shown.to_owned())
                    .or_insert_with(|| actual.to_owned());
            }
            ran += 1;
        }
    }
    assert_eq!(ran, 7);
    let entries = fs::read_dir(ws.path().join(".rustman/history")).unwrap();
    assert_eq!(
        entries
            .filter(|e| e.as_ref().unwrap().path().extension().is_some())
            .count(),
        0
    );
}
