//! Snapshot testing in `rustman run` (snapshot-testing tasks 4.1-4.3):
//! recording, update mode, CI mode and the conflicting-flags error, the
//! snapshot outcomes in reports, and secrets that never reach a snapshot
//! file. Plus `rustman snapshot list|prune` (5.1, 5.2).

mod common;

use std::fs;
use std::path::Path;

use assert_cmd::Command;
use common::{Server, request, valid_report, workspace, write};
use predicates::prelude::*;

const SNAPSHOT: &str = "users/__snapshots__/get-user.default.snap.yaml";
const SECRET: &str = "Sn4pCANARY-tok-91";

/// `rustman` without the caller's `CI` variable, which turns CI mode on.
fn rustman(dir: &Path) -> Command {
    let mut cmd = common::rustman(dir);
    cmd.env_remove("CI");
    cmd
}

/// A workspace whose `users/get-user` request snapshots `/echo`, which
/// answers with the request's path (`?role={{role}}`) and headers.
fn snapshot_workspace(server: &Server) -> tempfile::TempDir {
    let ws = workspace(server);
    let env = format!(
        "variables:\n  base_url: {}\n  role: admin\nsecrets: [tok]\n",
        server.url("")
    );
    write(ws.path(), "environments/dev.yaml", &env);
    let extra = "headers:\n  - name: X-Token\n    value: '{{tok}}'\n\
                 assertions:\n  - status: 200\n  - snapshot\n";
    write(
        ws.path(),
        "users/get-user.rman.yaml",
        &request("get-user", "{{base_url}}/echo?role={{role}}", extra),
    );
    rustman(ws.path())
        .args(["secret", "set", "tok", "--env", "dev"])
        .write_stdin(SECRET)
        .assert()
        .success();
    ws
}

fn run(ws: &Path, args: &[&str]) -> (Option<i32>, String, String) {
    let output = rustman(ws)
        .args(["run", "users", "--env", "dev"])
        .args(args)
        .output()
        .unwrap();
    let text = |bytes: &[u8]| String::from_utf8_lossy(bytes).into_owned();
    (
        output.status.code(),
        text(&output.stdout),
        text(&output.stderr),
    )
}

fn snapshot_text(ws: &Path) -> String {
    fs::read_to_string(ws.join(SNAPSHOT)).unwrap()
}

/// Run with `args`, expect the exit `code` and every one of `needles` on
/// stdout.
fn expect(ws: &Path, args: &[&str], code: i32, needles: &[&str]) {
    let (actual, out, _) = run(ws, args);
    assert_eq!(actual, Some(code), "{out}");
    for needle in needles {
        assert!(out.contains(needle), "missing {needle:?}: {out}");
    }
}

#[test]
fn record_then_match_then_fail_then_update() {
    let server = Server::http();
    let ws = snapshot_workspace(&server);
    let note = format!("snapshot default recorded: {SNAPSHOT}");
    expect(ws.path(), &[], 0, &["1 snapshot recorded\n", &note]);
    let recorded = snapshot_text(ws.path());
    assert!(recorded.contains("path: /echo?role=admin"), "{recorded}");

    expect(ws.path(), &[], 0, &["1 snapshot matched\n"]);

    let mismatch = format!("snapshot mismatch: {SNAPSHOT}");
    let change = r#"changed $.path: "/echo?role=admin" → "/echo?role=viewer""#;
    let failed = ["1 snapshot failed\n", &mismatch, change];
    expect(ws.path(), &["--var", "role=viewer"], 1, &failed);
    assert_eq!(
        snapshot_text(ws.path()),
        recorded,
        "a failure writes nothing"
    );

    let viewer = ["-u", "--var", "role=viewer"];
    expect(ws.path(), &viewer, 0, &["1 snapshot updated\n"]);
    let updated = snapshot_text(ws.path());
    assert!(updated.contains("path: /echo?role=viewer"), "{updated}");
}

#[test]
fn update_mode_leaves_a_matching_file_untouched() {
    let server = Server::http();
    let ws = snapshot_workspace(&server);
    expect(ws.path(), &[], 0, &["1 snapshot recorded\n"]);
    let modified = fs::metadata(ws.path().join(SNAPSHOT)).unwrap().modified();
    expect(
        ws.path(),
        &["--update-snapshots"],
        0,
        &["1 snapshot matched\n"],
    );
    let after = fs::metadata(ws.path().join(SNAPSHOT)).unwrap().modified();
    assert_eq!(
        after.unwrap(),
        modified.unwrap(),
        "a matching file is not rewritten"
    );
}

/// `rustman send users/get-user` with `args` and `CI` set to `ci`: the exit
/// code and stderr.
fn send(ws: &Path, args: &[&str], ci: Option<&str>) -> (Option<i32>, String) {
    let mut cmd = rustman(ws);
    cmd.args(["send", "users/get-user", "--env", "dev"])
        .args(args);
    if let Some(value) = ci {
        cmd.env("CI", value);
    }
    let output = cmd.output().unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(!stdout.contains(SECRET), "{stdout}");
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    assert!(!stderr.contains(SECRET), "{stderr}");
    (output.status.code(), stderr)
}

#[test]
fn send_never_writes_snapshots_in_ci_mode() {
    let server = Server::http();
    let ws = snapshot_workspace(&server);
    let missing = format!("snapshot missing: {SNAPSHOT} (run without --ci to record)");
    for (args, ci) in [
        (&["--ci"][..], None),
        (&[][..], Some("true")),
        (&[][..], Some("1")),
    ] {
        let (code, err) = send(ws.path(), args, ci);
        assert_eq!(code, Some(1), "{args:?} {ci:?}: {err}");
        assert!(err.contains(&missing), "{err}");
    }
    assert!(!ws.path().join("users/__snapshots__").exists());

    let (code, err) = send(ws.path(), &["-u"], Some("true"));
    assert_eq!(code, Some(2), "{err}");
    assert!(err.contains("--update-snapshots cannot be used in CI mode"));
    assert!(!ws.path().join("users/__snapshots__").exists());
}

#[test]
fn send_records_updates_and_notes_snapshots() {
    let server = Server::http();
    let ws = snapshot_workspace(&server);
    let (code, err) = send(ws.path(), &[], None);
    assert_eq!(code, Some(0), "{err}");
    let recorded = format!("snapshot default recorded: {SNAPSHOT}");
    assert!(err.contains(&recorded), "{err}");
    let stored = snapshot_text(ws.path());
    assert!(!stored.contains(SECRET), "{stored}");

    let viewer = ["--var", "role=viewer"];
    let (code, err) = send(ws.path(), &viewer, Some("1"));
    assert_eq!(code, Some(1), "{err}");
    assert!(
        err.contains(&format!("snapshot mismatch: {SNAPSHOT}")),
        "{err}"
    );
    assert_eq!(snapshot_text(ws.path()), stored, "CI mode writes nothing");

    let (code, err) = send(ws.path(), &["-u", "--var", "role=viewer"], None);
    assert_eq!(code, Some(0), "{err}");
    let updated = format!("snapshot default updated: {SNAPSHOT}");
    assert!(err.contains(&updated), "{err}");
    assert!(snapshot_text(ws.path()).contains("path: /echo?role=viewer"));
}

#[test]
fn ci_mode_fails_on_a_missing_snapshot_and_writes_nothing() {
    let server = Server::http();
    let ws = snapshot_workspace(&server);
    let missing = format!("snapshot missing: {SNAPSHOT} (run without --ci to record)");
    let (code, out, _) = run(ws.path(), &["--ci"]);
    assert_eq!(code, Some(1), "{out}");
    assert!(out.contains(&missing), "{out}");
    assert!(out.contains("1 snapshot missing\n"), "{out}");
    for value in ["true", "1", "TRUE"] {
        rustman(ws.path())
            .args(["run", "users", "--env", "dev"])
            .env("CI", value)
            .assert()
            .code(1)
            .stdout(predicate::str::contains(missing.as_str()));
    }
    assert!(!ws.path().join("users/__snapshots__").exists());
    rustman(ws.path())
        .args(["run", "users", "--env", "dev"])
        .env("CI", "false")
        .assert()
        .success()
        .stdout(predicate::str::contains("1 snapshot recorded"));
    assert!(ws.path().join(SNAPSHOT).is_file());
}

#[test]
fn updating_in_ci_mode_is_a_usage_error() {
    let server = Server::http();
    let ws = snapshot_workspace(&server);
    let error = "error: --update-snapshots cannot be used in CI mode\n";
    rustman(ws.path())
        .args(["run", ".", "--ci", "--update-snapshots"])
        .assert()
        .code(2)
        .stdout("")
        .stderr(error);
    rustman(ws.path())
        .args(["run", ".", "-u"])
        .env("CI", "true")
        .assert()
        .code(2)
        .stderr(error);
    assert!(!ws.path().join("users/__snapshots__").exists());
}

#[test]
fn reports_carry_the_snapshot_outcome_and_diff() {
    let server = Server::http();
    let ws = snapshot_workspace(&server);
    run(ws.path(), &[]);
    let (code, out, _) = run(
        ws.path(),
        &[
            "--var",
            "role=viewer",
            "--reporter",
            "json",
            "--reporter",
            "junit:out/junit.xml",
        ],
    );
    assert_eq!(code, Some(1));
    let report = valid_report(out.as_bytes());
    assert_eq!(report["summary"]["snapshots"]["failed"], 1);
    let snapshot = &report["requests"][0]["assertions"][1];
    assert_eq!(snapshot["kind"], "snapshot");
    assert_eq!(snapshot["outcome"], "fail");
    assert_eq!(snapshot["snapshot"]["state"], "failed");
    assert_eq!(snapshot["snapshot"]["path"], SNAPSHOT);
    let change = &snapshot["snapshot"]["diff"]["changes"][0];
    assert_eq!(change["path"], "$.path");
    assert_eq!(change["kind"], "changed");
    assert_eq!(change["new"], "/echo?role=viewer");
    let junit = fs::read_to_string(ws.path().join("out/junit.xml")).unwrap();
    let doc = roxmltree::Document::parse(&junit).unwrap();
    let failure = doc
        .descendants()
        .find(|n| n.has_tag_name("failure"))
        .unwrap();
    let message = failure.attribute("message").unwrap();
    assert!(
        message.contains(r#"changed $.path: "/echo?role=admin" → "/echo?role=viewer""#),
        "{message}"
    );
}

/// Task 4.3: a server echoing a secret, recorded, failed and updated; the
/// snapshot file and every report hold only the placeholder.
#[test]
fn an_echoed_secret_never_reaches_a_snapshot_or_report() {
    let server = Server::http();
    let ws = snapshot_workspace(&server);
    let (code, out, _) = run(ws.path(), &[]);
    assert_eq!(code, Some(0), "{out}");
    let recorded = snapshot_text(ws.path());
    assert!(!recorded.contains(SECRET), "{recorded}");
    assert!(recorded.contains("«redacted:tok»"), "{recorded}");

    let reporters = [
        "--reporter",
        "pretty:out/pretty.txt",
        "--reporter",
        "json:out/report.json",
        "--reporter",
        "junit:out/junit.xml",
        "--reporter",
        "tap:out/report.tap",
    ];
    let mut failing = vec!["--var", "role=viewer"];
    failing.extend(reporters);
    let (code, out, err) = run(ws.path(), &failing);
    assert_eq!(code, Some(1), "{out}");
    let mut outputs = vec![out, err];
    for file in ["pretty.txt", "report.json", "junit.xml", "report.tap"] {
        outputs.push(fs::read_to_string(ws.path().join("out").join(file)).unwrap());
    }
    assert!(outputs[2].contains("snapshot mismatch"), "{}", outputs[2]);
    for text in &outputs {
        assert!(!text.contains(SECRET), "{text}");
    }

    let (code, out, _) = run(ws.path(), &["-u", "--var", "role=viewer"]);
    assert_eq!(code, Some(0), "{out}");
    assert!(!out.contains(SECRET), "{out}");
    let updated = snapshot_text(ws.path());
    assert!(updated.contains("role=viewer"), "{updated}");
    assert!(!updated.contains(SECRET), "{updated}");
    assert!(updated.contains("«redacted:tok»"), "{updated}");
}

/// A workspace (no server needed) whose `users/get-user` declares the
/// snapshots `default` and `v2`, with a stale name and an orphan beside them.
fn listing_workspace() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    write(
        root,
        "rustman.toml",
        "[workspace]\nname = \"t\"\nversion = 1\n",
    );
    let extra = "assertions:\n  - snapshot\n  - snapshot: {name: v2}\n";
    write(
        root,
        "users/get-user.rman.yaml",
        &request("get-user", "http://localhost:1/u", extra),
    );
    for name in [
        "get-user.default",
        "get-user.v2",
        "get-user.gone",
        "old.default",
    ] {
        write(
            root,
            &format!("users/__snapshots__/{name}.snap.yaml"),
            "version: 1\n",
        );
    }
    dir
}

const LISTING: &str = "\
ok                 users/__snapshots__/get-user.default.snap.yaml  users/get-user.rman.yaml default
unreferenced name  users/__snapshots__/get-user.gone.snap.yaml  users/get-user.rman.yaml gone
ok                 users/__snapshots__/get-user.v2.snap.yaml  users/get-user.rman.yaml v2
orphaned           users/__snapshots__/old.default.snap.yaml  users/old.rman.yaml default
";

#[test]
fn list_shows_every_snapshot_with_its_state() {
    let ws = listing_workspace();
    rustman(ws.path())
        .args(["snapshot", "list"])
        .assert()
        .success()
        .stdout(LISTING);
    let out = rustman(ws.path())
        .args(["snapshot", "list", "--json"])
        .output()
        .unwrap();
    assert!(out.status.success());
    let list: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    let states: Vec<&str> = list
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["state"].as_str().unwrap())
        .collect();
    assert_eq!(states, ["ok", "unreferenced_name", "ok", "orphaned"]);
    assert_eq!(list[3]["request"], "users/old.rman.yaml");
    assert_eq!(list[3]["name"], "default");
    assert_eq!(list[3]["path"], "users/__snapshots__/old.default.snap.yaml");
    // A request file as the path lists only its own snapshots.
    let out = rustman(&ws.path().join("users"))
        .args(["snapshot", "list", "get-user.rman.yaml"])
        .output()
        .unwrap();
    assert_eq!(String::from_utf8_lossy(&out.stdout).lines().count(), 3);
    rustman(ws.path())
        .args(["snapshot", "list", "missing"])
        .assert()
        .code(2)
        .stderr("error: missing: no such file or folder\n");
}

#[test]
fn prune_without_yes_only_lists_when_not_on_a_terminal() {
    let ws = listing_workspace();
    let candidates = "\
obsolete users/__snapshots__/get-user.gone.snap.yaml (unreferenced name)
obsolete users/__snapshots__/old.default.snap.yaml (orphaned)
";
    rustman(ws.path())
        .args(["snapshot", "prune"])
        .write_stdin("y\n")
        .assert()
        .success()
        .stdout(candidates)
        .stderr("pass --yes to delete them\n");
    rustman(ws.path())
        .args(["snapshot", "prune", "--dry-run", "--yes"])
        .assert()
        .success()
        .stdout(candidates)
        .stderr("");
    rustman(ws.path())
        .args(["snapshot", "list"])
        .assert()
        .stdout(LISTING);
}

#[test]
fn prune_with_yes_deletes_the_obsolete_snapshots() {
    let ws = listing_workspace();
    rustman(ws.path())
        .args(["snapshot", "prune", "--yes"])
        .assert()
        .success()
        .stdout(
            "deleted users/__snapshots__/get-user.gone.snap.yaml (unreferenced name)\n\
             deleted users/__snapshots__/old.default.snap.yaml (orphaned)\n",
        );
    let snapshots = ws.path().join("users/__snapshots__");
    assert!(!snapshots.join("get-user.gone.snap.yaml").exists());
    assert!(!snapshots.join("old.default.snap.yaml").exists());
    assert!(snapshots.join("get-user.default.snap.yaml").exists());
    assert!(snapshots.join("get-user.v2.snap.yaml").exists());
    rustman(ws.path())
        .args(["snapshot", "prune", "--yes"])
        .assert()
        .success()
        .stdout("no obsolete snapshots\n");
}

#[test]
fn an_empty_workspace_has_no_snapshots() {
    let dir = tempfile::tempdir().unwrap();
    write(
        dir.path(),
        "rustman.toml",
        "[workspace]\nname = \"t\"\nversion = 1\n",
    );
    rustman(dir.path())
        .args(["snapshot", "list"])
        .assert()
        .success()
        .stdout("no snapshots\n");
    rustman(dir.path())
        .args(["snapshot", "list", "--json"])
        .assert()
        .success()
        .stdout("[]\n");
}
