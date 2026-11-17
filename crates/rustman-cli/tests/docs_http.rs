//! The examples in docs/http.md (task 4.12).
//!
//! Files in ```yaml / ```toml blocks (first line `# <path>`) are written into
//! one workspace, which must load. The secret `api_token` is set for `prod`
//! first. Every `$ rustman ...` line of a ```console block is split like a
//! shell would, run against the workspace in document order, and must print
//! exactly the lines that follow it (`warning:`/`error:` lines on stderr, the
//! rest on stdout). The engine messages are checked in
//! `crates/rustman-http/tests/docs_http.rs`.

use std::fs;
use std::path::Path;

use assert_cmd::Command;
use rustman_core::Workspace;

fn doc() -> String {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/http.md");
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

fn rustman(dir: &Path) -> Command {
    let mut cmd = Command::cargo_bin("rustman").expect("rustman binary is built");
    cmd.current_dir(dir)
        .env("RUSTMAN_SECRET_STORE", "file")
        .env_remove("RUSTMAN_ENV");
    cmd
}

/// Write every file block into `root`.
fn write_files(root: &Path, blocks: &[(String, Vec<String>)]) -> usize {
    let mut written = 0;
    for (lang, lines) in blocks.iter().filter(|(l, _)| l == "yaml" || l == "toml") {
        let path = lines[0]
            .strip_prefix("# ")
            .unwrap_or_else(|| panic!("{lang} block without a path comment: {lines:?}"));
        let file = root.join(path);
        fs::create_dir_all(file.parent().unwrap()).unwrap();
        fs::write(file, lines[1..].join("\n") + "\n").unwrap();
        written += 1;
    }
    written
}

/// One `$ rustman ...` line and the output that follows it.
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
                stdout: String::new(),
                stderr: String::new(),
            });
            continue;
        }
        let step = steps.last_mut().expect("output before any command");
        let out = if line.starts_with("warning:") || line.starts_with("error:") {
            &mut step.stderr
        } else {
            &mut step.stdout
        };
        out.push_str(line);
        out.push('\n');
    }
    steps
}

#[test]
fn doc_examples_load_and_run() {
    let text = doc();
    let blocks = blocks(&text);
    let ws = tempfile::tempdir().unwrap();
    assert!(write_files(ws.path(), &blocks) >= 8);
    let loaded = Workspace::load(ws.path()).unwrap();
    assert!(loaded.warnings.is_empty(), "{:?}", loaded.warnings);
    rustman(ws.path())
        .args(["secret", "set", "api_token", "--env", "prod"])
        .write_stdin("tok-1234")
        .assert()
        .success();
    let mut ran = 0;
    for (_, lines) in blocks.iter().filter(|(l, _)| l == "console") {
        for step in steps(lines) {
            rustman(ws.path())
                .args(&step.args)
                .assert()
                .success()
                .stdout(step.stdout)
                .stderr(step.stderr);
            ran += 1;
        }
    }
    assert!(ran >= 5, "only {ran} commands ran");
}
