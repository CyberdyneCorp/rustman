//! `rustman secret set|rm|list` (task 3.4) and the canary checks for the
//! sinks that exist so far (task 3.5).
//!
//! Every test uses `RUSTMAN_SECRET_STORE=file`, so the real OS keychain is
//! never touched; the keychain path is covered by `#[ignore]`d tests in
//! rustman-core.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command as Process, Stdio};

use assert_cmd::Command;
use predicates::prelude::*;

const CANARY: &str = "canary-7f3a9c-s3cr3t";

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

/// A workspace whose `prod` environment declares `api_token` and `other`.
fn workspace(default_env: Option<&str>) -> tempfile::TempDir {
    let tmp = tempfile::tempdir().unwrap();
    let defaults = default_env
        .map(|e| format!("\n[defaults]\ndefault_environment = \"{e}\"\n"))
        .unwrap_or_default();
    write(
        tmp.path(),
        "rustman.toml",
        &format!("[workspace]\nname = \"t\"\nversion = 1\n{defaults}"),
    );
    write(
        tmp.path(),
        "environments/prod.yaml",
        "variables:\n  base_url: https://api.example.com\nsecrets: [api_token, other]\n",
    );
    write(
        tmp.path(),
        "environments/local.yaml",
        "variables:\n  base_url: http://localhost:8080\n",
    );
    write(
        tmp.path(),
        "api/me.rman.yaml",
        "version: 1\nkind: http\nname: Me\nmethod: GET\nurl: '{{base_url}}/me'\nheaders:\n  - name: Authorization\n    value: Bearer {{api_token}}\n",
    );
    tmp
}

fn set(dir: &Path, name: &str, env: &str, value: &str) -> assert_cmd::assert::Assert {
    rustman(dir)
        .args(["secret", "set", name, "--env", env])
        .write_stdin(value)
        .assert()
}

/// Every file under `root`, relative path → bytes.
fn files(root: &Path) -> Vec<(PathBuf, Vec<u8>)> {
    let mut out = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for entry in fs::read_dir(&dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                stack.push(path);
            } else {
                let rel = path.strip_prefix(root).unwrap().to_path_buf();
                out.push((rel, fs::read(&path).unwrap()));
            }
        }
    }
    out.sort();
    out
}

fn workspace_files(root: &Path) -> Vec<(PathBuf, Vec<u8>)> {
    files(root)
        .into_iter()
        .filter(|(rel, _)| !rel.starts_with(".rustman"))
        .collect()
}

fn contains(haystack: &[u8], needle: &str) -> bool {
    haystack
        .windows(needle.len())
        .any(|w| w == needle.as_bytes())
}

#[test]
fn set_stores_the_value_outside_workspace_files_without_echo() {
    let ws = workspace(None);
    let before = workspace_files(ws.path());
    let out = set(ws.path(), "api_token", "prod", &format!("{CANARY}\n"))
        .success()
        .stdout("stored secret 'api_token' for environment 'prod' in .rustman/secrets.yaml\n")
        .stderr("")
        .get_output()
        .clone();
    assert!(!contains(&out.stdout, CANARY) && !contains(&out.stderr, CANARY));
    assert_eq!(
        workspace_files(ws.path()),
        before,
        "no workspace file changed"
    );
    let stored = fs::read_to_string(ws.path().join(".rustman/secrets.yaml")).unwrap();
    assert_eq!(stored, format!("prod:\n  api_token: {CANARY}\n"));
}

#[cfg(unix)]
#[test]
fn secrets_file_is_mode_0600() {
    use std::os::unix::fs::PermissionsExt;
    let ws = workspace(None);
    set(ws.path(), "api_token", "prod", CANARY).success();
    let mode = fs::metadata(ws.path().join(".rustman/secrets.yaml"))
        .unwrap()
        .permissions()
        .mode();
    assert_eq!(mode & 0o777, 0o600);
}

#[test]
fn list_shows_state_never_values() {
    let ws = workspace(None);
    set(ws.path(), "api_token", "prod", CANARY).success();
    rustman(ws.path())
        .args(["secret", "list"])
        .assert()
        .success()
        .stdout("prod/api_token  set\nprod/other  not set\n")
        .stdout(predicate::str::contains(CANARY).not());
    rustman(ws.path())
        .args(["secret", "list", "--env", "local"])
        .assert()
        .success()
        .stdout("")
        .stderr("no secrets declared\n");
}

#[test]
fn rm_deletes_the_value() {
    let ws = workspace(None);
    set(ws.path(), "api_token", "prod", CANARY).success();
    set(ws.path(), "other", "prod", "x").success();
    rustman(ws.path())
        .args(["secret", "rm", "api_token", "--env", "prod"])
        .assert()
        .success()
        .stdout("removed secret 'api_token' from environment 'prod'\n");
    rustman(ws.path())
        .args(["secret", "rm", "api_token", "--env", "prod"])
        .assert()
        .success()
        .stdout("secret 'api_token' was not set in environment 'prod'\n");
    let stored = fs::read_to_string(ws.path().join(".rustman/secrets.yaml")).unwrap();
    assert!(!stored.contains(CANARY), "{stored}");
    rustman(ws.path())
        .args(["secret", "list", "--env", "prod"])
        .assert()
        .success()
        .stdout("prod/api_token  not set\nprod/other  set\n");
}

#[test]
fn active_environment_is_used_without_env() {
    let ws = workspace(Some("prod"));
    rustman(ws.path())
        .args(["secret", "set", "api_token"])
        .write_stdin(CANARY)
        .assert()
        .success()
        .stdout(predicate::str::contains("environment 'prod'"));
    let ws = workspace(None);
    rustman(ws.path())
        .env("RUSTMAN_ENV", "prod")
        .args(["secret", "set", "api_token"])
        .write_stdin(CANARY)
        .assert()
        .success();
}

#[test]
fn no_active_environment_exits_2() {
    let ws = workspace(None);
    rustman(ws.path())
        .args(["secret", "set", "api_token"])
        .write_stdin(CANARY)
        .assert()
        .code(2)
        .stderr("error: no environment selected (pass --env <name> or set RUSTMAN_ENV)\n");
}

#[test]
fn undeclared_secret_exits_2() {
    let ws = workspace(None);
    set(ws.path(), "base_url", "prod", CANARY)
        .code(2)
        .stderr("error: 'base_url' is not a secret of environment 'prod' (list it under 'secrets' in environments/prod.yaml)\n");
    assert!(!ws.path().join(".rustman").exists());
}

#[test]
fn missing_environment_exits_2() {
    let ws = workspace(None);
    set(ws.path(), "api_token", "qa", CANARY)
        .code(2)
        .stderr("error: environment 'qa' not found (available: local, prod)\n");
}

#[test]
fn empty_value_exits_2() {
    let ws = workspace(None);
    set(ws.path(), "api_token", "prod", "\n")
        .code(2)
        .stderr("error: secret value is empty; nothing was stored\n");
}

#[test]
fn invalid_backend_exits_2() {
    let ws = workspace(None);
    rustman(ws.path())
        .env("RUSTMAN_SECRET_STORE", "vault")
        .args(["secret", "list"])
        .assert()
        .code(2)
        .stderr("error: invalid RUSTMAN_SECRET_STORE 'vault': expected 'keychain' or 'file'\n");
}

fn git(dir: &Path, args: &[&str]) {
    let ok = Process::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .expect("git is installed")
        .success();
    assert!(ok, "git {args:?} failed");
}

#[test]
fn refuses_to_write_when_not_git_ignored() {
    let ws = workspace(None);
    git(ws.path(), &["init", "-q"]);
    write(ws.path(), ".gitignore", "target/\n");
    set(ws.path(), "api_token", "prod", CANARY)
        .code(2)
        .stdout("")
        .stderr("error: refusing to write secrets: .rustman/ is not git-ignored\n");
    assert!(!ws.path().join(".rustman/secrets.yaml").exists());
    write(ws.path(), ".gitignore", "target/\n.rustman/\n");
    set(ws.path(), "api_token", "prod", CANARY).success();
}

#[cfg(unix)]
#[test]
fn refuses_to_write_through_a_symlinked_state_folder() {
    let ws = workspace(None);
    git(ws.path(), &["init", "-q"]);
    write(ws.path(), ".gitignore", "target/\n.rustman/\n");
    fs::create_dir_all(ws.path().join("public")).unwrap();
    std::os::unix::fs::symlink("public", ws.path().join(".rustman")).unwrap();
    set(ws.path(), "api_token", "prod", CANARY)
        .code(2)
        .stdout("")
        .stderr("error: refusing to write secrets: .rustman is a symbolic link\n");
    assert_eq!(fs::read_dir(ws.path().join("public")).unwrap().count(), 0);
}

#[cfg(unix)]
#[test]
fn reading_a_world_readable_secrets_file_warns() {
    use std::os::unix::fs::PermissionsExt;
    let ws = workspace(None);
    set(ws.path(), "api_token", "prod", CANARY).success();
    let path = ws.path().join(".rustman/secrets.yaml");
    fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
    rustman(ws.path())
        .args(["secret", "list", "--env", "prod"])
        .assert()
        .success()
        .stdout(predicate::str::contains(CANARY).not())
        .stderr(predicate::str::contains(
            ".rustman/secrets.yaml is readable by other users",
        ));
}

#[test]
fn init_workspace_is_git_ignored_so_secrets_can_be_written() {
    let tmp = tempfile::tempdir().unwrap();
    git(tmp.path(), &["init", "-q"]);
    rustman(tmp.path()).arg("init").assert().success();
    write(
        tmp.path(),
        "environments/local.yaml",
        "variables:\n  base_url: http://x\nsecrets: [api_token]\n",
    );
    rustman(tmp.path())
        .args(["secret", "set", "api_token"])
        .write_stdin(CANARY)
        .assert()
        .success();
}

#[test]
fn canary_never_reaches_any_output_or_workspace_file() {
    let ws = workspace(None);
    let mut outputs = Vec::new();
    let mut run = |args: &[&str], stdin: &str| {
        let out = rustman(ws.path())
            .args(args)
            .write_stdin(stdin)
            .output()
            .unwrap();
        outputs.push(out.stdout);
        outputs.push(out.stderr);
    };
    run(&["secret", "set", "api_token", "--env", "prod"], CANARY);
    run(&["secret", "set", "api_token", "--env", "prod"], CANARY);
    run(&["secret", "set", "base_url", "--env", "prod"], CANARY);
    run(&["secret", "list"], "");
    run(&["env", "list", "--env", "prod"], "");
    run(&["secret", "rm", "other", "--env", "prod"], "");
    for output in &outputs {
        assert!(
            !contains(output, CANARY),
            "{}",
            String::from_utf8_lossy(output)
        );
    }
    for (rel, bytes) in files(ws.path()) {
        if rel != Path::new(".rustman/secrets.yaml") {
            assert!(!contains(&bytes, CANARY), "{}", rel.display());
        }
    }
}

#[test]
fn a_secret_shorter_than_four_characters_is_set_with_a_warning() {
    let ws = workspace(None);
    set(ws.path(), "api_token", "prod", "t0k\n")
        .success()
        .stderr(
            "warning: secret 'api_token' is shorter than 4 characters: every occurrence \
             of it is redacted, even in unrelated text\n",
        );
    set(ws.path(), "other", "prod", "t0ke\n")
        .success()
        .stderr("");
}
