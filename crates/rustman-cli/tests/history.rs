//! Request history (tasks 7.1-7.3): recording, `--no-history` and
//! `[history] enabled`, the ad-hoc store, redaction at rest, retention with
//! spill cleanup, `history clear`, concurrent writers, the list filters,
//! `show` and `replay` with secret re-resolution.

mod common;

use std::fs;
use std::path::{Path, PathBuf};

use common::{Server, request, rustman, unique, workspace, write};
use predicates::prelude::*;
use rustman_test_support::routes::{TOKEN, timed_hits};
use serde_json::Value;

const CANARY: &str = "canary-7Hq2-s3cr3t";

fn history_dir(ws: &Path) -> PathBuf {
    ws.join(".rustman/history")
}

fn entry_files(dir: &Path) -> Vec<PathBuf> {
    let Ok(read) = fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut files: Vec<PathBuf> = read
        .map(|e| e.unwrap().path())
        .filter(|p| p.extension().is_some_and(|x| x == "json"))
        .collect();
    files.sort();
    files
}

/// `rustman history list --json` in `dir`, plus `args`.
fn list(dir: &Path, args: &[&str]) -> Vec<Value> {
    let out = rustman(dir)
        .args(["history", "list", "--json"])
        .args(args)
        .assert()
        .success()
        .stderr("")
        .get_output()
        .stdout
        .clone();
    let rows: Value = serde_json::from_slice(&out).unwrap();
    rows.as_array().unwrap().clone()
}

fn statuses(rows: &[Value]) -> Vec<u64> {
    rows.iter().map(|r| r["status"].as_u64().unwrap()).collect()
}

fn set_secret(dir: &Path, name: &str, value: &str) {
    rustman(dir)
        .args(["secret", "set", name, "--env", "dev"])
        .write_stdin(value)
        .assert()
        .success();
}

/// A workspace whose `dev` environment also declares the secret `token`.
fn secret_workspace(server: &Server) -> tempfile::TempDir {
    let ws = workspace(server);
    let env = format!(
        "variables:\n  base_url: {}\nsecrets:\n  - token\n",
        server.url("")
    );
    write(ws.path(), "environments/dev.yaml", &env);
    ws
}

#[test]
fn send_is_recorded_as_the_newest_entry() {
    let server = Server::http();
    let ws = workspace(&server);
    let get_user = request("get-user", "{{base_url}}/users/42", "");
    write(ws.path(), "users/get-user.rman.yaml", &get_user);
    write(
        ws.path(),
        "status.rman.yaml",
        &request("status", "{{base_url}}/status/503", ""),
    );
    for target in ["status", "get-user"] {
        rustman(ws.path())
            .args(["send", target, "--env", "dev"])
            .output()
            .unwrap();
    }
    let rows = list(ws.path(), &[]);
    assert_eq!(statuses(&rows), [200, 503]);
    assert_eq!(rows[0]["request"], "users/get-user.rman.yaml");
    assert_eq!(rows[0]["environment"], "dev");
    assert_eq!(rows[0]["url"], server.url("/users/42"));
    let text = rustman(ws.path())
        .args(["history", "list"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let text = String::from_utf8(text).unwrap();
    let newest = text.lines().next().unwrap();
    let url = server.url("/users/42");
    assert!(newest.contains(&format!("  GET {url}  200  ")), "{newest}");
    assert!(newest.ends_with(" ms"), "{newest}");
    assert_eq!(entry_files(&history_dir(ws.path())).len(), 2);
}

#[test]
fn no_history_and_disabled_history_record_nothing() {
    let server = Server::http();
    let ws = workspace(&server);
    write(
        ws.path(),
        "a.rman.yaml",
        &request("a", "{{base_url}}/users/42", ""),
    );
    rustman(ws.path())
        .args(["run", ".", "--env", "dev", "--no-history"])
        .assert()
        .success();
    rustman(ws.path())
        .args(["send", "a", "--env", "dev", "--no-history"])
        .assert()
        .success();
    assert!(entry_files(&history_dir(ws.path())).is_empty());
    write(
        ws.path(),
        "rustman.toml",
        "[workspace]\nname = \"t\"\nversion = 1\n[history]\nenabled = false\n",
    );
    rustman(ws.path())
        .args(["run", ".", "--env", "dev"])
        .assert()
        .success();
    assert!(entry_files(&history_dir(ws.path())).is_empty());
    assert!(list(ws.path(), &[]).is_empty());
}

#[test]
fn ad_hoc_requests_outside_a_workspace_go_to_the_data_dir() {
    let server = Server::http();
    let cwd = tempfile::tempdir().unwrap();
    let data = tempfile::tempdir().unwrap();
    let url = server.url("/users/42");
    rustman(cwd.path())
        .env("RUSTMAN_DATA_DIR", data.path())
        .args(["send", "GET", &url])
        .assert()
        .success();
    assert_eq!(entry_files(&data.path().join("history")).len(), 1);
    assert!(!cwd.path().join(".rustman").exists());
    let out = rustman(cwd.path())
        .env("RUSTMAN_DATA_DIR", data.path())
        .args(["history", "list", "--json"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let rows: Value = serde_json::from_slice(&out).unwrap();
    assert_eq!(rows[0]["request"], "adhoc");
    assert_eq!(rows[0]["url"], url);
}

#[test]
fn ad_hoc_requests_in_a_workspace_are_recorded_as_adhoc() {
    let server = Server::http();
    let ws = workspace(&server);
    rustman(ws.path())
        .args(["send", "POST", &server.url("/echo"), "-d", "hi"])
        .assert()
        .success();
    let rows = list(ws.path(), &[]);
    assert_eq!(rows[0]["request"], "adhoc");
    assert_eq!(rows[0]["method"], "POST");
}

#[test]
fn the_canary_secret_never_reaches_history() {
    let server = Server::http();
    let ws = secret_workspace(&server);
    set_secret(ws.path(), "token", CANARY);
    let echo = request(
        "echo",
        "{{base_url}}/echo?key={{token}}",
        "method: POST\nheaders:\n  - name: Authorization\n    value: 'Bearer {{token}}'\n  \
         - name: X-Token\n    value: '{{token}}'\nbody:\n  type: json\n  content:\n    token: '{{token}}'\n",
    );
    write(ws.path(), "echo.rman.yaml", &echo);
    rustman(ws.path())
        .args(["send", "echo", "--env", "dev"])
        .assert()
        .success();
    let files = entry_files(&history_dir(ws.path()));
    assert_eq!(files.len(), 1);
    let stored = fs::read_to_string(&files[0]).unwrap();
    assert!(!stored.contains(CANARY), "{stored}");
    assert!(stored.contains("«redacted:token»"), "{stored}");
    assert!(stored.contains("«redacted:Authorization»"), "{stored}");
    // The replayable request holds the placeholder, not the value.
    assert!(stored.contains("Bearer {{token}}"), "{stored}");
    for args in [&["history", "list"][..], &["history", "show", "0"][..]] {
        rustman(ws.path())
            .args(args)
            .assert()
            .success()
            .stdout(predicate::str::contains(CANARY).not());
    }
}

#[test]
fn retention_prunes_the_oldest_entries_and_their_spill_files() {
    let server = Server::http();
    let ws = workspace(&server);
    write(
        ws.path(),
        "rustman.toml",
        "[workspace]\nname = \"t\"\nversion = 1\n[http]\nmax_body_bytes = 1024\n\
         [history]\nmax_entries = 2\n",
    );
    write(
        ws.path(),
        "big.rman.yaml",
        &request("big", "{{base_url}}/bytes/5000", ""),
    );
    let bodies = ws.path().join(".rustman/bodies");
    let send = || {
        rustman(ws.path())
            .args(["send", "big", "--env", "dev", "-o", "out.bin"])
            .assert()
            .success();
    };
    send();
    let first = entry_files(&history_dir(ws.path()));
    let entry: Value = serde_json::from_str(&fs::read_to_string(&first[0]).unwrap()).unwrap();
    let spill = bodies.join(entry["spill"].as_str().unwrap());
    assert!(spill.is_file());
    send();
    send();
    let files = entry_files(&history_dir(ws.path()));
    assert_eq!(files.len(), 2);
    assert!(!files.contains(&first[0]));
    assert!(!spill.exists(), "the pruned entry's spill file is deleted");
    assert_eq!(fs::read_dir(&bodies).unwrap().count(), 2);
}

#[test]
fn clear_deletes_every_entry() {
    let server = Server::http();
    let ws = workspace(&server);
    write(
        ws.path(),
        "a.rman.yaml",
        &request("a", "{{base_url}}/users/42", ""),
    );
    for _ in 0..3 {
        rustman(ws.path())
            .args(["send", "a", "--env", "dev"])
            .output()
            .unwrap();
    }
    rustman(ws.path())
        .args(["history", "clear"])
        .assert()
        .success()
        .stdout("deleted 3 exchanges from history\n");
    assert!(list(ws.path(), &[]).is_empty());
}

#[test]
fn two_processes_writing_at_once_keep_every_entry_readable() {
    let server = Server::http();
    let ws = workspace(&server);
    for i in 0..10 {
        let file = format!("f{i}/r.rman.yaml");
        write(
            ws.path(),
            &file,
            &request(&format!("r{i}"), "{{base_url}}/users/42", ""),
        );
    }
    let run = || {
        rustman(ws.path())
            .args([
                "run",
                ".",
                "--env",
                "dev",
                "--parallel",
                "4",
                "--iterations",
                "5",
            ])
            .args(["--reporter", "tap"])
            .assert()
            .success();
    };
    std::thread::scope(|scope| {
        scope.spawn(run);
        scope.spawn(run);
    });
    let rows = list(ws.path(), &[]);
    assert_eq!(rows.len(), 100);
    assert!(rows.iter().all(|r| r["status"] == 200));
}

/// Move entry `file` back in time: a new id from an old timestamp.
fn age_entry(file: &Path) {
    let mut entry: Value = serde_json::from_str(&fs::read_to_string(file).unwrap()).unwrap();
    // 2016-07-30: the ULID spec's example timestamp.
    let id = "01ARZ3NDEKTSV4RRFFQ69G5FAV";
    entry["id"] = Value::from(id);
    fs::write(file.with_file_name(format!("{id}.json")), entry.to_string()).unwrap();
    fs::remove_file(file).unwrap();
}

#[test]
fn list_filters_by_status_method_text_age_and_count() {
    let server = Server::http();
    let ws = workspace(&server);
    let send = |args: &[&str]| {
        rustman(ws.path()).arg("send").args(args).output().unwrap();
    };
    send(&["GET", &server.url("/status/500")]);
    age_entry(&entry_files(&history_dir(ws.path()))[0]);
    send(&["GET", &server.url("/users/42")]);
    send(&["GET", &server.url("/status/503")]);
    send(&["POST", &server.url("/echo"), "-d", "needle-in-body"]);
    send(&["GET", &server.url("/status/404")]);

    assert_eq!(statuses(&list(ws.path(), &[])), [404, 200, 503, 200, 500]);
    let failed = list(ws.path(), &["--status", "5xx", "--since", "1h"]);
    assert_eq!(statuses(&failed), [503]);
    assert_eq!(statuses(&list(ws.path(), &["--status", "5xx"])), [503, 500]);
    assert_eq!(statuses(&list(ws.path(), &["--status", "404"])), [404]);
    let posts = list(ws.path(), &["--method", "post"]);
    assert_eq!(posts.len(), 1);
    assert_eq!(posts[0]["method"], "POST");
    let found = list(ws.path(), &["--grep", "NEEDLE-IN"]);
    assert_eq!(found.len(), 1);
    assert_eq!(found[0]["url"], server.url("/echo"));
    assert_eq!(list(ws.path(), &["--grep", "/users/42"]).len(), 1);
    assert_eq!(statuses(&list(ws.path(), &["--limit", "2"])), [404, 200]);
    rustman(ws.path())
        .args(["history", "list", "--status", "6xx"])
        .assert()
        .code(2)
        .stderr(predicate::str::contains("invalid status '6xx'"));
}

#[test]
fn show_prints_the_whole_exchange() {
    let server = Server::http();
    let ws = workspace(&server);
    let get_user = request(
        "get-user",
        "{{base_url}}/users/42",
        "assertions:\n  - status: 200\n",
    );
    write(ws.path(), "get-user.rman.yaml", &get_user);
    rustman(ws.path())
        .args(["send", "get-user", "--env", "dev"])
        .assert()
        .success();
    let id = list(ws.path(), &[])[0]["id"].as_str().unwrap().to_owned();
    rustman(ws.path())
        .args(["history", "show", &id[..12]])
        .assert()
        .success()
        .stdout(predicate::str::contains(format!("id: {id}\n")))
        .stdout(predicate::str::contains(format!(
            "> GET {}\n",
            server.url("/users/42")
        )))
        .stdout(predicate::str::contains("< HTTP/1.1 200 ("))
        .stdout(predicate::str::contains("< x-request-id: req-1\n"))
        .stdout(predicate::str::contains("pass: status 200"));
    let out = rustman(ws.path())
        .args(["history", "show", &id, "--json"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let entry: Value = serde_json::from_slice(&out).unwrap();
    assert_eq!(entry["schema"], "rustman.history/v1");
    assert_eq!(entry["id"], id.as_str());
    rustman(ws.path())
        .args(["history", "show", "NOSUCHID"])
        .assert()
        .code(2)
        .stderr("error: no history entry 'NOSUCHID' (see 'rustman history list')\n");
}

#[test]
fn replay_sends_the_current_secret_value() {
    let server = Server::http();
    let ws = secret_workspace(&server);
    set_secret(ws.path(), "token", TOKEN);
    let me = request(
        "me",
        "{{base_url}}/me",
        "headers:\n  - name: Authorization\n    value: 'Bearer {{token}}'\n",
    );
    write(ws.path(), "me.rman.yaml", &me);
    rustman(ws.path())
        .args(["send", "me", "--env", "dev"])
        .assert()
        .success();
    let id = list(ws.path(), &[])[0]["id"].as_str().unwrap().to_owned();

    rustman(ws.path())
        .args(["history", "replay", &id, "--body-only"])
        .assert()
        .success()
        .stdout("{\"id\":42}");
    let rows = list(ws.path(), &[]);
    assert_eq!(statuses(&rows), [200, 200]);
    assert_eq!(rows[0]["request"], "me.rman.yaml");
    assert_ne!(rows[0]["id"], id.as_str());

    // A rotated secret is what the replay sends.
    set_secret(ws.path(), "token", "rotated");
    rustman(ws.path())
        .args(["history", "replay", &id])
        .assert()
        .success()
        .stdout(predicate::str::starts_with("HTTP/1.1 401\n"));
    assert_eq!(statuses(&list(ws.path(), &[]))[0], 401);
}

#[test]
fn replay_with_a_deleted_secret_sends_nothing() {
    let server = Server::http();
    let ws = secret_workspace(&server);
    set_secret(ws.path(), "token", CANARY);
    let hit = unique(ws.path());
    let timed = request(
        "timed",
        &format!("{{{{base_url}}}}/timed/0?id={hit}"),
        "headers:\n  - name: Authorization\n    value: 'Bearer {{token}}'\n",
    );
    write(ws.path(), "timed.rman.yaml", &timed);
    rustman(ws.path())
        .args(["send", "timed", "--env", "dev"])
        .assert()
        .success();
    assert_eq!(timed_hits(&hit).len(), 1);
    let id = list(ws.path(), &[])[0]["id"].as_str().unwrap().to_owned();
    rustman(ws.path())
        .args(["secret", "rm", "token", "--env", "dev"])
        .assert()
        .success();
    rustman(ws.path())
        .args(["history", "replay", &id])
        .assert()
        .code(3)
        .stdout("")
        .stderr(predicate::str::starts_with(
            "error: unresolved variable(s): token (secret not set; \
             run 'rustman secret set token --env dev')",
        ));
    assert_eq!(timed_hits(&hit).len(), 1, "nothing was sent");
    assert_eq!(list(ws.path(), &[]).len(), 1, "nothing was recorded");
}

#[test]
fn a_literal_authorization_header_cannot_be_replayed() {
    let server = Server::http();
    let ws = workspace(&server);
    let me = request(
        "me",
        "{{base_url}}/me",
        "headers:\n  - name: Authorization\n    value: Bearer abc123\n",
    );
    write(ws.path(), "me.rman.yaml", &me);
    rustman(ws.path())
        .args(["send", "me", "--env", "dev"])
        .output()
        .unwrap();
    let files = entry_files(&history_dir(ws.path()));
    assert!(!fs::read_to_string(&files[0]).unwrap().contains("abc123"));
    let id = list(ws.path(), &[])[0]["id"].as_str().unwrap().to_owned();
    rustman(ws.path())
        .args(["history", "replay", &id])
        .assert()
        .code(3)
        .stderr(predicate::str::contains(
            "this exchange cannot be replayed: the Authorization header was not kept in history",
        ));
}

#[test]
fn show_rejects_an_empty_id_and_reports_an_unreadable_entry() {
    let server = Server::http();
    let ws = workspace(&server);
    write(
        ws.path(),
        "a.rman.yaml",
        &request("a", "{{base_url}}/users/42", ""),
    );
    rustman(ws.path())
        .args(["send", "a", "--env", "dev"])
        .assert()
        .success();
    rustman(ws.path())
        .args(["history", "show", ""])
        .assert()
        .code(2)
        .stdout("")
        .stderr("error: a history id cannot be empty (see 'rustman history list')\n");
    let file = entry_files(&history_dir(ws.path())).remove(0);
    fs::write(&file, b"{\"trunc").unwrap();
    let id = file.file_stem().unwrap().to_string_lossy().into_owned();
    let out = rustman(ws.path())
        .args(["history", "show", &id])
        .assert()
        .code(2)
        .get_output()
        .stderr
        .clone();
    let stderr = String::from_utf8(out).unwrap();
    assert!(
        stderr.starts_with("error: cannot read history entry "),
        "{stderr}"
    );
    assert!(stderr.contains(&format!("{id}.json: ")), "{stderr}");
    assert_eq!(stderr.matches(&id).count(), 1, "{stderr}");
    assert!(!stderr.contains("skipping"), "{stderr}");
}

#[test]
fn a_zero_limit_is_a_usage_error() {
    let dir = tempfile::tempdir().unwrap();
    rustman(dir.path())
        .args(["history", "list", "--limit", "0"])
        .assert()
        .code(2)
        .stdout("")
        .stderr(predicate::str::contains(
            "invalid value '0' for '--limit <N>'",
        ));
}
