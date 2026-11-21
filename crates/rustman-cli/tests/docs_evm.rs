//! Every `rustman evm` invocation shown in docs/evm.md parses (add-evm-toolkit
//! task 4.5): each is run with `--help` appended, which clap answers only
//! when the subcommand and every flag before it exist. The offline
//! `decode` examples also run for real and print what the page shows.

mod common;

use std::path::Path;

use common::rustman;

fn page() -> String {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/evm.md");
    std::fs::read_to_string(path).unwrap().replace("\r\n", "\n")
}

/// The words of a shell line, honouring double quotes, without a trailing
/// `# comment`.
fn words(line: &str) -> Vec<String> {
    let line = line.split(" # ").next().unwrap_or(line);
    let mut out = Vec::new();
    let mut current = String::new();
    let mut quoted = false;
    for c in line.chars() {
        match c {
            '"' => quoted = !quoted,
            ' ' if !quoted => {
                if !current.is_empty() {
                    out.push(std::mem::take(&mut current));
                }
            }
            other => current.push(other),
        }
    }
    if !current.is_empty() {
        out.push(current);
    }
    out
}

fn invocations() -> Vec<Vec<String>> {
    page()
        .lines()
        .map(|l| l.trim().trim_start_matches("$ "))
        .filter(|l| l.starts_with("rustman evm "))
        .map(words)
        .collect()
}

#[test]
fn every_documented_invocation_parses() {
    let all = invocations();
    let subcommands: std::collections::BTreeSet<&str> = all.iter().map(|w| w[2].as_str()).collect();
    assert_eq!(
        subcommands.into_iter().collect::<Vec<_>>(),
        ["block", "call", "decode", "ens", "fork", "logs", "tx"]
    );
    let dir = tempfile::tempdir().unwrap();
    for words in all {
        let out = rustman(dir.path())
            .args(&words[1..])
            .arg("--help")
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{}: {}",
            words.join(" "),
            String::from_utf8_lossy(&out.stderr)
        );
    }
}

#[test]
fn the_decode_examples_print_what_the_page_shows() {
    let text = page();
    let lines: Vec<&str> = text.lines().collect();
    let dir = tempfile::tempdir().unwrap();
    let mut checked = 0;
    for (i, line) in lines.iter().enumerate() {
        let Some(command) = line.strip_prefix("$ rustman evm decode ") else {
            continue;
        };
        if command.contains('…') {
            continue;
        }
        let out = rustman(dir.path())
            .args(["evm", "decode"])
            .args(words(command))
            .output()
            .unwrap();
        assert!(out.status.success(), "{line}");
        assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), lines[i + 1]);
        checked += 1;
    }
    assert_eq!(checked, 2);
}
