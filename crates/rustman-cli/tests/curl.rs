//! `rustman import curl` and `rustman export curl` (task 4.11).
//!
//! Secrets use `RUSTMAN_SECRET_STORE=file`, so the OS keychain is never
//! touched.

use std::fs;
use std::path::Path;

use assert_cmd::Command;
use predicates::prelude::*;

fn rustman(dir: &Path) -> Command {
    let mut cmd = Command::cargo_bin("rustman").expect("rustman binary is built");
    cmd.current_dir(dir)
        .env("RUSTMAN_SECRET_STORE", "file")
        .env_remove("RUSTMAN_ENV");
    cmd
}

fn write(root: &Path, rel: &str, text: &str) {
    let path = root.join(rel);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, text).unwrap();
}

fn workspace() -> tempfile::TempDir {
    let tmp = tempfile::tempdir().unwrap();
    write(
        tmp.path(),
        "rustman.toml",
        "[workspace]\nname = \"t\"\nversion = 1\n",
    );
    write(
        tmp.path(),
        "environments/prod.yaml",
        "variables:\n  base_url: https://api.example.com\nsecrets: [api_token]\n",
    );
    tmp
}

const POST_JSON: &str =
    r#"curl -X POST https://x.io/a -H 'Content-Type: application/json' -d '{"a":1}'"#;

const POST_JSON_YAML: &str = "version: 1
kind: http
name: POST /a
method: POST
url: https://x.io/a
body:
  type: json
  content:
    a: 1
";

#[test]
fn import_prints_the_request_file() {
    let ws = workspace();
    rustman(ws.path())
        .args(["import", "curl", POST_JSON])
        .assert()
        .success()
        .stdout(POST_JSON_YAML)
        .stderr("");
}

#[test]
fn import_reads_stdin_or_split_words() {
    let ws = workspace();
    rustman(ws.path())
        .args(["import", "curl"])
        .write_stdin(POST_JSON)
        .assert()
        .success()
        .stdout(POST_JSON_YAML);
    rustman(ws.path())
        .args([
            "import",
            "curl",
            "--",
            "curl",
            "-X",
            "DELETE",
            "https://x.io/a",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "method: DELETE\nurl: https://x.io/a\n",
        ));
}

#[test]
fn import_warns_about_unsupported_options() {
    let ws = workspace();
    rustman(ws.path())
        .args(["import", "curl", "curl --limit-rate 1k https://x.io/a"])
        .assert()
        .success()
        .stderr("warning: ignored unsupported curl option --limit-rate\n")
        .stdout(predicate::str::contains("url: https://x.io/a\n"));
}

#[test]
fn import_out_writes_a_loadable_file_and_never_overwrites() {
    let ws = workspace();
    rustman(ws.path())
        .args(["import", "curl", "--out", "api/a.rman.yaml", POST_JSON])
        .assert()
        .failure()
        .code(2);
    fs::create_dir_all(ws.path().join("api")).unwrap();
    rustman(ws.path())
        .args(["import", "curl", "--out", "api/a.rman.yaml", POST_JSON])
        .assert()
        .success()
        .stdout(format!(
            "wrote {}\n",
            Path::new("api/a.rman.yaml").display()
        ));
    let written = fs::read_to_string(ws.path().join("api/a.rman.yaml")).unwrap();
    assert_eq!(written, POST_JSON_YAML);
    rustman(ws.path())
        .args([
            "import",
            "curl",
            "--out",
            "api/a.rman.yaml",
            "curl https://x.io/b",
        ])
        .assert()
        .failure()
        .code(2);
    assert_eq!(
        fs::read_to_string(ws.path().join("api/a.rman.yaml")).unwrap(),
        POST_JSON_YAML
    );
    rustman(ws.path())
        .args(["export", "curl", "api/a.rman.yaml"])
        .assert()
        .success()
        .stdout(
            "curl 'https://x.io/a' -H 'Content-Type: application/json' --data-raw '{\"a\":1}' -L\n",
        );
}

#[test]
fn import_out_refuses_a_file_the_workspace_could_not_load() {
    let ws = workspace();
    fs::create_dir_all(ws.path().join("examples")).unwrap();
    rustman(ws.path())
        .args([
            "import",
            "curl",
            "--out",
            "examples/p.rman.yaml",
            "curl --data-binary @/etc/passwd https://x.io",
        ])
        .assert()
        .code(2)
        .stderr(predicate::str::contains(
            "file path escapes workspace: /etc/passwd",
        ));
    assert!(!ws.path().join("examples/p.rman.yaml").exists());
    rustman(ws.path())
        .args([
            "import",
            "curl",
            "--out",
            "examples/q.rman.yaml",
            "curl --data-binary @body.json https://x.io",
        ])
        .assert()
        .success();
    rustman(ws.path())
        .args(["export", "curl", "examples/q.rman.yaml"])
        .assert()
        .success();
}

#[test]
fn import_errors_exit_2() {
    let ws = workspace();
    rustman(ws.path())
        .args(["import", "curl", "curl -k"])
        .assert()
        .code(2)
        .stderr("error: curl: no URL in the command\n");
}

#[test]
fn export_redacts_secrets_unless_asked() {
    let ws = workspace();
    write(
        ws.path(),
        "api/me.rman.yaml",
        "version: 1\nkind: http\nname: Me\nurl: '{{base_url}}/me'\nauth:\n  type: bearer\n  token: '{{api_token}}'\n",
    );
    rustman(ws.path())
        .args(["secret", "set", "api_token", "--env", "prod"])
        .write_stdin("tok-s3cr3t")
        .assert()
        .success();
    let out = rustman(ws.path())
        .args(["export", "curl", "api/me", "--env", "prod"])
        .assert()
        .success()
        .get_output()
        .clone();
    let stdout = String::from_utf8(out.stdout).unwrap();
    assert_eq!(
        stdout,
        "curl 'https://api.example.com/me' -H 'Authorization: «redacted:api_token»' -L\n"
    );
    assert!(!String::from_utf8_lossy(&out.stderr).contains("tok-s3cr3t"));
    rustman(ws.path())
        .args(["export", "curl", "Me", "--env", "prod", "--show-secrets"])
        .assert()
        .success()
        .stdout("curl 'https://api.example.com/me' -H 'Authorization: Bearer tok-s3cr3t' -L\n");
}

#[test]
fn export_of_an_unknown_request_exits_2() {
    let ws = workspace();
    rustman(ws.path())
        .args(["export", "curl", "nope"])
        .assert()
        .code(2);
}
