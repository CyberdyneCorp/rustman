//! The examples in docs/environments.md (task 3.6).
//!
//! Files in ```yaml / ```toml blocks (first line `# <path>`) are written into
//! one workspace. Every `$ ...` line of a ```console block is run against it,
//! in document order, and must print exactly the lines that follow it (lines
//! starting with `error:` or `warning:` are expected on stderr, the rest on
//! stdout). The error texts quoted in the page are checked against the engine.

use std::fs;
use std::path::{Path, PathBuf};

use assert_cmd::Command;
use rustman_core::secrets::{FALLBACK_WARNING, SecretBackend};
use rustman_core::vars::{Resolver, resolve_request};
use rustman_core::{Redactor, Secret, SecretError, SecretStore, Workspace};

const CANARY: &str = "canary-7f3a9c-s3cr3t";

fn doc() -> String {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/environments.md");
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

/// One `$ ...` line and the output that follows it.
#[derive(Debug, Default)]
struct Step {
    line: String,
    stdin_var: Option<String>,
    env: Vec<(String, String)>,
    args: Vec<String>,
    stdout: String,
    stderr: String,
}

impl Step {
    fn parse(line: &str) -> Self {
        let mut step = Step {
            line: line.to_owned(),
            ..Step::default()
        };
        let command = match line.split_once(" | ") {
            Some((source, command)) => {
                let var = source
                    .strip_prefix("printf '%s' \"$")
                    .and_then(|v| v.strip_suffix('"'))
                    .unwrap_or_else(|| panic!("unsupported pipe source in `{line}`"));
                step.stdin_var = Some(var.to_owned());
                command
            }
            None => line,
        };
        let mut words = command.split_whitespace();
        for word in words.by_ref() {
            if word == "rustman" {
                break;
            }
            let (k, v) = word
                .split_once('=')
                .unwrap_or_else(|| panic!("expected NAME=value or rustman in `{line}`"));
            step.env.push((k.to_owned(), v.to_owned()));
        }
        step.args = words.map(str::to_owned).collect();
        step
    }

    fn expect(&mut self, line: &str) {
        let sink = if line.starts_with("error:") || line.starts_with("warning:") {
            &mut self.stderr
        } else {
            &mut self.stdout
        };
        sink.push_str(line);
        sink.push('\n');
    }

    fn succeeds(&self) -> bool {
        !self.stderr.lines().any(|l| l.starts_with("error:"))
    }
}

fn steps(lines: &[String]) -> Vec<Step> {
    let mut steps: Vec<Step> = Vec::new();
    for line in lines {
        match line.strip_prefix("$ ") {
            Some(command) => steps.push(Step::parse(command)),
            None => steps
                .last_mut()
                .expect("console block starts with a command")
                .expect(line),
        }
    }
    steps
}

/// Write every file block under `root`; returns the paths written.
fn write_files(root: &Path, blocks: &[(String, Vec<String>)]) -> Vec<PathBuf> {
    let mut written = Vec::new();
    for (lang, lines) in blocks {
        if lang != "yaml" && lang != "toml" {
            continue;
        }
        let rel = lines[0]
            .strip_prefix("# ")
            .expect("file block starts with '# <path>'");
        let path = root.join(rel);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        let body: String = lines[1..].iter().map(|l| format!("{l}\n")).collect();
        fs::write(&path, body).unwrap();
        written.push(path);
    }
    written
}

fn run(root: &Path, step: &Step) -> (Vec<u8>, Vec<u8>) {
    let mut cmd = Command::cargo_bin("rustman").unwrap();
    cmd.current_dir(root)
        .env_remove("RUSTMAN_ENV")
        .env("RUSTMAN_SECRET_STORE", "file")
        .args(&step.args);
    for (k, v) in &step.env {
        cmd.env(k, v);
    }
    let stdin = match step.stdin_var.as_deref() {
        Some("API_TOKEN") => CANARY,
        Some(other) => panic!("no test value for ${other}"),
        None => "",
    };
    let assert = cmd.write_stdin(stdin).assert();
    let assert = if step.succeeds() {
        assert.success()
    } else {
        assert.failure()
    };
    let assert = assert
        .stdout(step.stdout.clone())
        .stderr(step.stderr.clone());
    let out = assert.get_output();
    (out.stdout.clone(), out.stderr.clone())
}

fn contains(haystack: &[u8], needle: &str) -> bool {
    haystack
        .windows(needle.len())
        .any(|w| w == needle.as_bytes())
}

#[test]
fn console_examples_run_as_documented() {
    let blocks = blocks(&doc());
    let tmp = tempfile::tempdir().unwrap();
    let files = write_files(tmp.path(), &blocks);
    assert!(files.len() >= 5, "{files:?}");
    let mut ran = 0;
    for (lang, lines) in &blocks {
        if lang != "console" {
            continue;
        }
        for step in steps(lines) {
            let (stdout, stderr) = run(tmp.path(), &step);
            assert!(
                !contains(&stdout, CANARY) && !contains(&stderr, CANARY),
                "`{}` printed the secret",
                step.line
            );
            ran += 1;
        }
    }
    assert!(ran >= 7, "only {ran} commands found");
    for path in files {
        let text = fs::read_to_string(&path).unwrap();
        assert!(!text.contains(CANARY), "{}", path.display());
    }
}

#[test]
fn quoted_messages_match_the_engine() {
    let text = doc();
    let blocks = blocks(&text);
    let tmp = tempfile::tempdir().unwrap();
    write_files(tmp.path(), &blocks);
    let ws = Workspace::load(tmp.path()).unwrap_or_else(|e| panic!("{e}"));
    assert!(ws.warnings.is_empty(), "{:?}", ws.warnings);
    let store = SecretStore::open(tmp.path(), SecretBackend::File);
    let request = ws.find_by_name("Me").remove(0);

    let prod = ws.active_environment(Some("prod"), None).unwrap();
    let resolver = Resolver::for_request(&ws.settings, &request, prod, &store);
    let err = resolve_request(&request, &resolver).unwrap_err();
    let quoted = format!("error: {err}");
    assert!(text.contains(&quoted), "doc should quote `{quoted}`");

    for message in [
        FALLBACK_WARNING.to_owned(),
        format!("error: {}", SecretError::NotIgnored),
        format!(
            "Authorization: {}",
            Redactor::new().header_value("Authorization", "Bearer abc123")
        ),
    ] {
        assert!(text.contains(&message), "doc should quote `{message}`");
    }

    store
        .set("ci", &Secret::new("api_token", CANARY.to_owned()))
        .unwrap();
    let ci = ws.active_environment(Some("ci"), None).unwrap();
    let resolver = Resolver::for_request(&ws.settings, &request, ci, &store);
    let resolved = resolve_request(&request, &resolver).unwrap();
    assert_eq!(resolved.url.text, "http://127.0.0.1:9000/v1/me");
    assert_eq!(resolved.query[0].value.text, "10");
    let auth = &resolved.headers[0];
    assert_eq!(auth.value.text, format!("Bearer {CANARY}"));
    assert_eq!(
        Redactor::for_request(&resolved).resolved(&auth.value),
        "Bearer «redacted:api_token»"
    );
}

#[test]
fn invalid_environment_name_warning_is_as_documented() {
    let text = doc();
    let tmp = tempfile::tempdir().unwrap();
    write_files(tmp.path(), &blocks(&text));
    fs::write(tmp.path().join("environments/my env.yaml"), "").unwrap();
    let warning = "warning: ignoring environment 'my env': invalid name\n";
    assert!(text.contains(warning.trim_end()));
    Command::cargo_bin("rustman")
        .unwrap()
        .current_dir(tmp.path())
        .env_remove("RUSTMAN_ENV")
        .args(["env", "list"])
        .assert()
        .success()
        .stderr(warning);
}
