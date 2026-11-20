//! The examples in docs/api-docs.md (add-docs-generator task 7.2): the
//! `[docs]` table and the request-file `docs` key are accepted as written,
//! an example opted in through `[docs].examples` is published, the custom
//! sample template example works, every documented `rustman docs build`
//! line and the GitHub Pages workflow's `run: rustman` step succeed, and
//! the documented `docs serve` output is what the binary prints.

mod common;

use std::fs;
use std::io::{BufRead, BufReader};
use std::path::Path;
use std::process::{Output, Stdio};
use std::sync::mpsc;
use std::time::Duration;

use common::{Server, rustman, write};

const TOKEN: &str = "CANARY-apidocs-3e81b7";

fn doc() -> String {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/api-docs.md");
    fs::read_to_string(path).unwrap().replace("\r\n", "\n")
}

/// The fenced blocks of `text` as (language, body).
fn blocks(text: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let mut lines = text.lines();
    while let Some(line) = lines.next() {
        if let Some(lang) = line.strip_prefix("```").filter(|l| !l.is_empty()) {
            let body: Vec<&str> = lines.by_ref().take_while(|l| *l != "```").collect();
            out.push((lang.to_owned(), body.join("\n") + "\n"));
        }
    }
    out
}

/// The only block of `lang` whose body contains `marker`.
fn block(text: &str, lang: &str, marker: &str) -> String {
    let found: Vec<String> = blocks(text)
        .into_iter()
        .filter(|(l, body)| l == lang && body.contains(marker))
        .map(|(_, body)| body)
        .collect();
    assert_eq!(found.len(), 1, "one ```{lang} block with {marker:?}");
    found[0].clone()
}

/// The documented `$ rustman ...` lines with the output lines after them.
fn sessions(text: &str) -> Vec<(String, Vec<String>)> {
    let mut out = Vec::new();
    for (lang, body) in blocks(text) {
        if lang != "text" {
            continue;
        }
        let mut lines = body.lines();
        if let Some(command) = lines.next().and_then(|l| l.strip_prefix("$ ")) {
            out.push((command.to_owned(), lines.map(str::to_owned).collect()));
        }
    }
    out
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn run(root: &Path, line: &str) -> Output {
    let args = shell_words::split(line).unwrap();
    assert_eq!(args[0], "rustman", "{line}");
    let output = rustman(root).args(&args[1..]).output().unwrap();
    assert!(!stdout(&output).contains(TOKEN) && !stderr(&output).contains(TOKEN));
    output
}

/// A workspace with the documented `[docs]` table and request file, a
/// `Get user` request with one recorded exchange, and the secret `token`.
fn workspace(text: &str, server: &Server) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let table = block(text, "toml", "publish_variables");
    write(
        root,
        "rustman.toml",
        &format!("[workspace]\nname = \"Shop\"\nversion = 1\n\n{table}"),
    );
    write(
        root,
        "environments/dev.yaml",
        &format!(
            "variables:\n  base_url: {}\nsecrets:\n  - token\n",
            server.url("")
        ),
    );
    fs::create_dir_all(root.join("brand")).unwrap();
    fs::write(root.join("brand/logo.png"), b"\x89PNG\r\n\x1a\n").unwrap();
    fs::create_dir_all(root.join("docs-templates")).unwrap();
    write(
        root,
        "users/list.rman.yaml",
        &block(text, "yaml", "kind: http"),
    );
    write(
        root,
        "users/get.rman.yaml",
        "version: 1\nkind: http\nname: Get user\nurl: '{{base_url}}/echo'\nheaders:\n  - name: Authorization\n    value: Bearer {{token}}\n",
    );
    let stored = rustman(root)
        .args(["secret", "set", "token", "--env", "dev"])
        .write_stdin(format!("{TOKEN}\n"))
        .output()
        .unwrap();
    assert!(stored.status.success(), "{}", stderr(&stored));
    let sent = rustman(root)
        .args(["send", "Get user", "--env", "dev"])
        .output()
        .unwrap();
    assert!(sent.status.success(), "{}", stderr(&sent));
    dir
}

/// `wrote 12 files to site/` with any count.
fn assert_wrote(documented: &str, actual: &str) {
    let shape = |line: &str| -> (String, String) {
        let rest = line.trim_end().strip_prefix("wrote ").unwrap_or_else(|| {
            panic!("not a `wrote` line: {line:?}");
        });
        let (count, tail) = rest.split_once(' ').unwrap();
        assert!(count.parse::<usize>().is_ok(), "{line}");
        ("wrote".to_owned(), tail.to_owned())
    };
    assert_eq!(shape(documented), shape(actual));
}

#[test]
fn the_documented_builds_run_as_shown() {
    let text = doc();
    let server = Server::http();
    let ws = workspace(&text, &server);
    let root = ws.path();
    let builds: Vec<_> = sessions(&text)
        .into_iter()
        .filter(|(command, _)| command.starts_with("rustman docs build"))
        .collect();
    assert!(builds.len() >= 2, "{builds:?}");
    for (command, shown) in &builds {
        let output = run(root, command);
        assert!(output.status.success(), "{command}: {}", stderr(&output));
        assert_wrote(&shown[0], &stdout(&output));
        // The request opted in through `[docs].examples` publishes its
        // exchange; the documented request file is hidden.
        assert!(stderr(&output).contains("published example: users/get"));
    }
    assert!(root.join("site/endpoints/users-get.html").is_file());
    assert!(!root.join("site/endpoints/users-list.html").exists());
    assert!(root.join("docs/api/README.md").is_file());
    let page = fs::read_to_string(root.join("site/endpoints/users-get.html")).unwrap();
    assert!(page.contains("Example"), "{page}");

    let workflow = block(&text, "yaml", "deploy-pages");
    let steps: Vec<&str> = workflow
        .lines()
        .filter_map(|l| l.trim().strip_prefix("- run: "))
        .filter(|c| c.starts_with("rustman "))
        .collect();
    assert_eq!(steps, ["rustman docs build --out site"]);
    for step in steps {
        let output = run(root, step);
        assert!(output.status.success(), "{step}: {}", stderr(&output));
        assert!(stdout(&output).ends_with(" files to site/\n"));
    }
}

#[test]
fn the_documented_custom_sample_template_is_used() {
    let text = doc();
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let table = block(&text, "toml", "\"go\"");
    write(
        root,
        "rustman.toml",
        &format!("[workspace]\nname = \"Shop\"\nversion = 1\n\n{table}"),
    );
    write(
        root,
        "templates/go.tmpl",
        &block(&text, "jinja", "docs_context_version"),
    );
    write(
        root,
        "users/list.rman.yaml",
        "version: 1\nkind: http\nname: List users\nurl: '{{base_url}}/users'\n",
    );
    let output = run(root, "rustman docs build");
    assert!(output.status.success(), "{}", stderr(&output));
    assert!(!stderr(&output).contains("warning"), "{}", stderr(&output));
    let page = fs::read_to_string(root.join("site/endpoints/users-list.html")).unwrap();
    assert!(page.contains("http.NewRequest("), "{page}");
    assert!(!page.contains("httpie"), "only curl and go");
}

/// The first line of `reader`, or a panic after a minute.
fn first_line(reader: impl std::io::Read + Send + 'static) -> String {
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let mut line = String::new();
        let _ = BufReader::new(reader).read_line(&mut line);
        let _ = tx.send(line);
    });
    rx.recv_timeout(Duration::from_secs(60)).expect("a line")
}

#[test]
fn the_documented_preview_output_is_printed() {
    let text = doc();
    let serves: Vec<_> = sessions(&text)
        .into_iter()
        .filter(|(command, _)| command.starts_with("rustman docs serve"))
        .collect();
    let [(_, started), (busy_command, busy)] = serves.as_slice() else {
        panic!("two `docs serve` examples: {serves:?}");
    };
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    write(
        root,
        "rustman.toml",
        "[workspace]\nname = \"Shop\"\nversion = 1\n",
    );

    let mut child = std::process::Command::new(assert_cmd::cargo::cargo_bin("rustman"))
        .current_dir(root)
        .env("RUSTMAN_SECRET_STORE", "file")
        .args(["docs", "serve", "--port", "0"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let built = first_line(child.stderr.take().unwrap());
    let banner = first_line(child.stdout.take().unwrap());
    child.kill().unwrap();
    let _ = child.wait();
    let port = banner.trim_end().rsplit(':').next().unwrap().to_owned();
    assert_eq!(
        banner.trim_end(),
        started[1].replace("4000", &port),
        "{banner}"
    );
    assert_eq!(
        built.split_whitespace().next(),
        started[0].split_whitespace().next()
    );
    assert!(built.starts_with("docs: built "), "{built}");

    let held = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = held.local_addr().unwrap().port().to_string();
    let output = run(root, &busy_command.replace("4000", &port));
    assert_eq!(output.status.code(), Some(2));
    assert_eq!(stderr(&output).trim_end(), busy[0].replace("4000", &port));
}
