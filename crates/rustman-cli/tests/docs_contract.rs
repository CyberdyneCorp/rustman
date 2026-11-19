//! The examples in docs/contract.md (add-contract-testing task 7.4): every
//! `rustman` line of a ```sh block, and every `run: rustman` step of the
//! GitHub Actions workflow, runs in document order in one workspace (a git
//! repository whose `origin/main` holds the previous document), against
//! the test server standing in for `http://localhost:8080`. Each must run
//! (exit 0, or 1 for a failed check), never fail on its arguments.

mod common;

use std::fs;
use std::path::Path;

use common::{Server, rustman, write};

const TOKEN: &str = "canary-docs-41ab";

const SPEC: &str = r#"openapi: 3.1.0
info: {title: Users, version: "1"}
paths:
  /users/{id}:
    get:
      parameters:
        - {name: id, in: path, required: true, schema: {type: integer}}
      responses:
        "200":
          description: A user
          content:
            application/json:
              schema:
                type: object
                required: [id, email]
                properties:
                  id: {type: integer}
                  email: {type: string}
"#;

fn doc() -> String {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/contract.md");
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

/// The documented `rustman` command lines, in document order.
fn commands(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    for (lang, lines) in blocks(text) {
        for line in lines {
            let line = line.trim();
            let command = match lang.as_str() {
                "sh" => line.starts_with("rustman ").then_some(line),
                "yaml" => line
                    .strip_prefix("run: ")
                    .filter(|c| c.starts_with("rustman ")),
                _ => None,
            };
            out.extend(command.map(str::to_owned));
        }
    }
    out
}

fn git(dir: &Path, args: &[&str]) {
    let output = std::process::Command::new("git")
        .args(["-c", "user.name=t", "-c", "user.email=t@example.com"])
        .args(["-c", "commit.gpgsign=false", "-c", "core.autocrlf=false"])
        .args(args)
        .current_dir(dir)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

/// The workspace the examples run in: `openapi.yaml` committed as
/// `origin/main` and changed since, `users/get-user.rman.yaml` with a
/// secret bearer token, and one exchange in history.
fn workspace(server: &Server) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    write(
        root,
        "rustman.toml",
        "[workspace]\nname = \"t\"\nversion = 1\n",
    );
    write(root, ".gitignore", ".rustman/\n");
    write(
        root,
        "environments/dev.yaml",
        &format!(
            "variables:\n  base_url: {}\nsecrets: [token]\n",
            server.url("")
        ),
    );
    write(
        root,
        "users/get-user.rman.yaml",
        "version: 1\nkind: http\nname: get user\nurl: \"{{base_url}}/users/42\"\n\
         headers:\n  - name: Authorization\n    value: \"Bearer {{token}}\"\n\
         assertions:\n  - status: 200\n  - json: {path: \"$.id\", equals: 42}\n  - latency: {total_lt: 3000}\n",
    );
    write(root, "openapi.yaml", SPEC);
    git(root, &["init", "-q"]);
    git(root, &["add", "."]);
    git(root, &["commit", "-q", "-m", "base"]);
    git(root, &["update-ref", "refs/remotes/origin/main", "HEAD"]);
    write(
        root,
        "openapi.yaml",
        &SPEC.replace("required: [id, email]", "required: [id]"),
    );
    let stored = rustman(root)
        .args(["secret", "set", "token", "--env", "dev"])
        .write_stdin(format!("{TOKEN}\n"))
        .output()
        .unwrap();
    assert!(stored.status.success());
    let sent = rustman(root)
        .args(["send", "get user", "--env", "dev"])
        .output()
        .unwrap();
    assert!(
        sent.status.success(),
        "{}",
        String::from_utf8_lossy(&sent.stderr)
    );
    dir
}

#[test]
fn every_documented_command_runs() {
    let server = Server::http();
    let ws = workspace(&server);
    let found = commands(&doc());
    for wanted in [
        "contract diff --base",
        "contract pact generate",
        "contract pact verify",
        "contract verify --spec",
    ] {
        assert!(
            found.iter().any(|c| c.contains(wanted)),
            "no `{wanted}` example in {found:#?}"
        );
    }
    for command in &found {
        let line = command
            .replace("${{ github.base_ref }}", "main")
            .replace("http://localhost:8080/_pact/state", &server.url("/echo"))
            .replace("http://localhost:8080", &server.url(""));
        let args = shell_words::split(&line).unwrap();
        let output = rustman(ws.path()).args(&args[1..]).output().unwrap();
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            matches!(output.status.code(), Some(0 | 1)),
            "`{command}` exited {:?}: {stderr}",
            output.status.code()
        );
        assert!(
            !stderr.contains(TOKEN) && !String::from_utf8_lossy(&output.stdout).contains(TOKEN)
        );
        if line.contains("pact generate") {
            let warning =
                "warning: latency assertion has no Pact equivalent (users/get-user.rman.yaml)";
            assert!(stderr.contains(warning), "{stderr}");
        }
    }
    for file in [
        "contracts/pacts/web-users.json",
        "out/contract.md",
        "out/contract.sarif",
        "out/run.json",
        "out/conformance.md",
    ] {
        let text =
            fs::read_to_string(ws.path().join(file)).unwrap_or_else(|e| panic!("{file}: {e}"));
        assert!(!text.contains(TOKEN), "{file}");
    }
    let contract = fs::read_to_string(ws.path().join("out/contract.md")).unwrap();
    assert!(contract.contains("**Summary:** "), "{contract}");
}

#[test]
fn the_documented_messages_are_the_engine_s() {
    let text = doc();
    for message in [
        rustman_contract::pact::verify::STATES_URL_MISSING,
        "blocked: mutations not allowed",
        "$.id: expected number 42, got string \"42\"",
        "$.email: required property missing",
        "5/12 operations observed",
    ] {
        assert!(
            text.contains(message),
            "docs/contract.md does not quote `{message}`"
        );
    }
}
