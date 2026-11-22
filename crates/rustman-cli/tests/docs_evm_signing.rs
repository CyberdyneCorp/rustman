//! The examples of docs/evm-signing.md (add-evm-toolkit task 5.7): every
//! `rustman evm send` and `rustman evm signer` invocation parses, the "CI
//! on Anvil" files load and their first command prints the preview and
//! refuses without `--sign` (no node needed), and, against Anvil, the CI
//! steps sign and send three transactions without a terminal (two
//! `rustman evm send`, one `rustman run --sign`). The Anvil part
//! only runs with `RUSTMAN_ANVIL=1` or `RUSTMAN_REQUIRE_ANVIL=1` (the
//! `jsonrpc-anvil` CI job). Nothing leaves loopback.

mod common;

mod anvil_node;

use std::path::Path;

use anvil_node::{Node, PASSWORD, anvil_program, signing_fixtures};
use common::{rustman, write};

/// Where the docs' first deployment lands on a fresh Anvil.
const COUNTER: &str = "0x5FbDB2315678afecb367f032d93F642f64180aa3";
const DOCS_RPC: &str = "http://127.0.0.1:8545";

fn page() -> String {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/evm-signing.md");
    std::fs::read_to_string(path).unwrap().replace("\r\n", "\n")
}

/// The text of the `## <title>` section.
fn section(page: &str, title: &str) -> String {
    let start = page
        .find(&format!("\n## {title}\n"))
        .unwrap_or_else(|| panic!("no section {title}"));
    let body = &page[start + 1..];
    let end = body[3..].find("\n## ").map_or(body.len(), |i| i + 3);
    body[..end].to_owned()
}

/// The fenced code blocks of `text`: (language, body).
fn blocks(text: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let mut lines = text.lines();
    while let Some(line) = lines.next() {
        let Some(lang) = line.trim().strip_prefix("```") else {
            continue;
        };
        let body: Vec<&str> = lines.by_ref().take_while(|l| l.trim() != "```").collect();
        out.push((lang.to_owned(), body.join("\n") + "\n"));
    }
    out
}

/// The files of a section: blocks whose first line is `# <path>`.
fn files(text: &str) -> Vec<(String, String)> {
    blocks(text)
        .into_iter()
        .filter_map(|(_, body)| {
            let (first, rest) = body.split_once('\n')?;
            let path = first.strip_prefix("# ")?;
            Some((path.to_owned(), rest.to_owned()))
        })
        .collect()
}

/// The words of a shell line, without a `# comment` or a `< input`.
fn words(line: &str) -> Vec<String> {
    let line = line.split(" #").next().unwrap_or(line);
    let line = line.split(" <").next().unwrap_or(line);
    line.split_whitespace().map(str::to_owned).collect()
}

/// The `rustman` lines of the section's `sh` blocks.
fn commands(text: &str) -> Vec<Vec<String>> {
    blocks(text)
        .into_iter()
        .filter(|(lang, _)| lang == "sh")
        .flat_map(|(_, body)| {
            body.lines()
                .filter(|l| l.starts_with("rustman "))
                .map(words)
                .collect::<Vec<_>>()
        })
        .collect()
}

/// The CI workspace of the docs in a temporary directory, its `rpc`
/// pointed at `rpc`, with the test keystore and the Counter ABI.
fn ci_workspace(rpc: &str) -> tempfile::TempDir {
    let ci = section(&page(), "CI on Anvil");
    let dir = tempfile::tempdir().unwrap();
    let written = files(&ci);
    let paths: Vec<&str> = written.iter().map(|(p, _)| p.as_str()).collect();
    assert_eq!(
        paths,
        ["rustman.toml", "environments/anvil.yaml", "add.rman.yaml"]
    );
    for (path, text) in &written {
        write(dir.path(), path, &text.replace(DOCS_RPC, rpc));
    }
    let fixtures = signing_fixtures();
    let read = |rel: &str| std::fs::read_to_string(fixtures.join(rel)).unwrap();
    write(
        dir.path(),
        "keys/anvil0.json",
        &read("keystores/anvil0.json"),
    );
    write(
        dir.path(),
        "Counter.abi.json",
        &read("counter/Counter.abi.json"),
    );
    write(dir.path(), ".gitignore", ".rustman/\n");
    dir
}

fn run(dir: &Path, words: &[String]) -> (Option<i32>, String, String) {
    let out = rustman(dir)
        .args(&words[1..])
        .env("DEPLOYER_PW", PASSWORD)
        .env_remove("RUSTMAN_EVM_SIGNER")
        .output()
        .unwrap();
    (
        out.status.code(),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

#[test]
fn every_documented_invocation_parses() {
    let page = page();
    let all: Vec<Vec<String>> = page
        .lines()
        .map(str::trim)
        .filter(|l| l.starts_with("rustman evm "))
        .map(words)
        .collect();
    let subcommands: std::collections::BTreeSet<&str> = all.iter().map(|w| w[2].as_str()).collect();
    assert_eq!(
        subcommands.into_iter().collect::<Vec<_>>(),
        ["send", "signer"]
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
            "{words:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
}

#[test]
fn the_ci_files_load_and_refuse_without_sign() {
    let ci = section(&page(), "CI on Anvil");
    let steps = commands(&ci);
    assert_eq!(steps.len(), 4, "{steps:?}");
    assert!(!steps[0].contains(&"--sign".to_owned()));
    assert_eq!(steps[3][1], "run");
    assert!(steps[3].contains(&"--sign".to_owned()), "{steps:?}");
    // Nothing listens on port 9: the preview needs no node.
    let ws = ci_workspace("http://127.0.0.1:9");
    let (code, stdout, stderr) = run(ws.path(), &steps[0]);
    assert_eq!(code, Some(4), "{stdout}{stderr}");
    assert!(stdout.contains("evm-tx add"), "{stdout}");
    assert!(stdout.contains(COUNTER), "{stdout}");
    assert!(stdout.contains("signer  deployer"), "{stdout}");
    assert!(
        stderr.contains("error: add --sign to sign and send"),
        "{stderr}"
    );
    assert!(!format!("{stdout}{stderr}").contains(PASSWORD));
}

#[test]
fn the_ci_steps_sign_and_send_on_anvil() {
    let Some(program) = anvil_program() else {
        return;
    };
    let node = Node::start(&program, COUNTER);
    let ws = ci_workspace(&node.url());
    let steps = commands(&section(&page(), "CI on Anvil"));
    let outputs: Vec<_> = steps.iter().map(|w| run(ws.path(), w)).collect();
    assert_eq!(outputs[0].0, Some(4), "{:?}", outputs[0]);
    for ((code, stdout, stderr), nonce) in outputs[1..3].iter().zip([0, 1]) {
        assert_eq!(*code, Some(0), "{stdout}{stderr}");
        assert!(
            stderr.contains("confirmed by --yes-local on Anvil/Hardhat local (31337)"),
            "{stderr}"
        );
        assert!(
            stdout.contains(&format!("nonce        {nonce}")),
            "{stdout}"
        );
        assert!(stdout.contains("status     success"), "{stdout}");
        assert!(!format!("{stdout}{stderr}").contains(PASSWORD));
    }
    // `rustman run --sign --yes-local` signs the workspace's evm-tx file.
    let (code, stdout, stderr) = &outputs[3];
    assert_eq!(*code, Some(0), "{stdout}{stderr}");
    assert!(stderr.contains("nonce        2"), "{stderr}");
    assert!(stdout.contains("PASS  add"), "{stdout}");
    assert!(!format!("{stdout}{stderr}").contains(PASSWORD));
    let number = node.call(
        "eth_call",
        serde_json::json!([{"to": COUNTER, "data": "0x8381f58a"}, "latest"]),
    );
    assert_eq!(number, serde_json::json!(format!("0x{:0>64}", "6")));
    let history = std::fs::read_dir(ws.path().join(".rustman/history")).unwrap();
    let mut audits = 0;
    for entry in history.flatten() {
        let text = std::fs::read_to_string(entry.path()).unwrap();
        assert!(!text.contains(PASSWORD), "{text}");
        audits += usize::from(text.contains("\"evm-tx\""));
    }
    assert_eq!(audits, 3);
}
