//! The examples in docs/import-openapi.md (task 1.11), in document order:
//!
//! - a ```yaml block whose first line is `# <path>` is written into the
//!   workspace;
//! - a ```yaml block whose first line is `# <path> (generated)` must equal
//!   the file the import wrote;
//! - every `$ rustman ...` line of a ```console block is run and must print
//!   exactly the lines that follow it (lines starting with `error:` or
//!   `warning:` on stderr, the rest on stdout).

mod common;

use std::fs;
use std::path::Path;

use common::{rustman, write};

fn doc() -> String {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/import-openapi.md");
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

/// A `# <path>` block: written, or checked when marked `(generated)`.
fn file_block(ws: &Path, lines: &[String]) {
    let Some(header) = lines.first().and_then(|l| l.strip_prefix("# ")) else {
        return;
    };
    let body: String = lines[1..].iter().map(|l| format!("{l}\n")).collect();
    match header.strip_suffix(" (generated)") {
        Some(path) => {
            let written = fs::read_to_string(ws.join(path))
                .unwrap_or_else(|e| panic!("{path}: {e}"))
                .replace("\r\n", "\n");
            assert_eq!(written, body, "{path} differs from the docs");
        }
        None => write(ws, header, &body),
    }
}

/// Run each `$ rustman ...` line of a console block and compare its output.
fn console_block(ws: &Path, lines: &[String]) -> usize {
    let mut ran = 0;
    let mut index = 0;
    while index < lines.len() {
        let command = lines[index]
            .strip_prefix("$ rustman ")
            .unwrap_or_else(|| panic!("expected a command: {}", lines[index]));
        index += 1;
        let (mut stdout, mut stderr) = (String::new(), String::new());
        while index < lines.len() && !lines[index].starts_with("$ ") {
            let line = &lines[index];
            let to = if line.starts_with("warning:") || line.starts_with("error:") {
                &mut stderr
            } else {
                &mut stdout
            };
            to.push_str(line);
            to.push('\n');
            index += 1;
        }
        let output = rustman(ws)
            .args(command.split_whitespace())
            .output()
            .unwrap();
        let out = |b: &[u8]| String::from_utf8_lossy(b).replace("\r\n", "\n");
        assert_eq!(out(&output.stdout), stdout, "stdout of `{command}`");
        assert_eq!(out(&output.stderr), stderr, "stderr of `{command}`");
        assert_eq!(output.status.code(), Some(0), "`{command}`");
        ran += 1;
    }
    ran
}

#[test]
fn the_documented_commands_run_and_print_what_the_page_says() {
    let ws = tempfile::tempdir().unwrap();
    write(
        ws.path(),
        "rustman.toml",
        "[workspace]\nname = \"docs\"\nversion = 1\n",
    );
    let mut commands = 0;
    for (lang, lines) in blocks(&doc()) {
        match lang.as_str() {
            "yaml" => file_block(ws.path(), &lines),
            "console" => commands += console_block(ws.path(), &lines),
            _ => {}
        }
    }
    assert_eq!(commands, 3, "every documented command ran");
    // The walkthrough's end state.
    let shop = ws.path().join("shop");
    assert!(shop.join("_removed/delete-pet.rman.yaml").is_file());
    let body = fs::read_to_string(shop.join("pets/create-pet.rman.yaml")).unwrap();
    assert!(body.contains("name: Rex"), "{body}");
}
