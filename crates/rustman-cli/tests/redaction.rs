//! Echoed credentials that never came from a secret variable (environments
//! spec "Secret redaction invariant"): a literal `Authorization` header, an
//! ad-hoc `-H 'Authorization: ...'` and a cookie the jar sends are redacted
//! in history, the JSON report and `-vvv` logs when a server echoes them.

mod common;

use std::fs;
use std::path::Path;

use common::{Server, request, rustman, workspace, write};
use rustman_test_support::routes::{SESSION, TOKEN};

const LITERAL: &str = "LITERALabc123-tok";

fn history_text(ws: &Path) -> String {
    let dir = ws.join(".rustman/history");
    let mut text = String::new();
    for entry in fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.extension().is_some_and(|x| x == "json") {
            text.push_str(&fs::read_to_string(path).unwrap());
        }
    }
    text
}

fn assert_redacted(what: &str, text: &str) {
    assert!(!text.contains(LITERAL), "{what}: {text}");
    assert!(!text.contains(SESSION), "{what}: {text}");
    assert!(text.contains("«redacted:Authorization»"), "{what}: {text}");
}

/// A workspace whose jar holds the `/session` cookie, with an echo request
/// sending a literal `Authorization` header.
fn echo_workspace(server: &Server) -> tempfile::TempDir {
    let ws = workspace(server);
    let headers = format!("headers:\n  - name: Authorization\n    value: Bearer {LITERAL}\n");
    write(
        ws.path(),
        "echo.rman.yaml",
        &request("echo", "{{base_url}}/echo", &headers),
    );
    write(
        ws.path(),
        "session/start.rman.yaml",
        &request("start", "{{base_url}}/session", ""),
    );
    rustman(ws.path())
        .args(["send", "start", "--env", "dev", "--no-history"])
        .assert()
        .success();
    ws
}

#[test]
fn echoed_literal_authorization_and_jar_cookie_are_redacted() {
    let server = Server::http();
    let ws = echo_workspace(&server);
    let out = rustman(ws.path())
        .args(["-vvv", "send", "echo", "--env", "dev"])
        .assert()
        .success()
        .get_output()
        .clone();
    let logs = String::from_utf8_lossy(&out.stderr).into_owned();
    let terminal = String::from_utf8_lossy(&out.stdout).into_owned();
    assert!(logs.contains("x-echo") || logs.contains("< "), "{logs}");
    assert_redacted("logs", &logs);
    assert!(
        !terminal.contains(LITERAL) && !terminal.contains(SESSION),
        "{terminal}"
    );
    rustman(ws.path())
        .args(["run", "echo.rman.yaml", "--env", "dev"])
        .args(["--reporter", "json:out/report.json"])
        .assert()
        .success();
    let report = fs::read_to_string(ws.path().join("out/report.json")).unwrap();
    assert_redacted("report", &report);
    assert!(report.contains("«redacted:Cookie»"), "{report}");
    let history = history_text(ws.path());
    assert_redacted("history", &history);
    assert!(history.contains("«redacted:Cookie»"), "{history}");
}

#[test]
fn echoed_ad_hoc_authorization_is_redacted_in_history() {
    let server = Server::http();
    let ws = workspace(&server);
    let url = server.url("/echo");
    rustman(ws.path())
        .args(["send", "POST", &url])
        .args(["-H", &format!("Authorization: Bearer {LITERAL}")])
        .assert()
        .success();
    let history = history_text(ws.path());
    assert!(!history.contains(LITERAL), "{history}");
    assert!(history.contains("«redacted:Authorization»"), "{history}");
}

#[test]
fn capture_messages_naming_a_credential_are_redacted() {
    let server = Server::http();
    let ws = workspace(&server);
    let extra = format!(
        "headers:\n  - name: Authorization\n    value: Bearer {LITERAL}\n\
         captures:\n  - {{var: a, from: json, path: '$[\"{LITERAL}\"]', required: true}}\n  \
         - {{var: b, from: header, name: x-{LITERAL}}}\n"
    );
    write(
        ws.path(),
        "echo.rman.yaml",
        &request("echo", "{{base_url}}/echo", &extra),
    );
    rustman(ws.path())
        .args(["run", "echo.rman.yaml", "--env", "dev", "--no-history"])
        .args(["--reporter", "json:out/report.json"])
        .assert()
        .code(1);
    let report = fs::read_to_string(ws.path().join("out/report.json")).unwrap();
    assert!(!report.contains(LITERAL), "{report}");
    assert!(
        report.contains("no match for $[\\\"«redacted:Authorization»\\\"]"),
        "{report}"
    );
}

const CANARY: &str = "CANARYz9Xq7-tok";

/// A workspace whose `dev` environment holds the secret `api_token`, with
/// `api/1-echo` sending it and capturing the server's echo of it into the
/// non-secret `echoed`, and `api/2-use` putting `echoed` in its URL.
fn derived_secret_workspace(server: &Server) -> tempfile::TempDir {
    let ws = workspace(server);
    let env = format!(
        "variables:\n  base_url: {}\nsecrets: [api_token]\n",
        server.url("")
    );
    write(ws.path(), "environments/dev.yaml", &env);
    rustman(ws.path())
        .args(["secret", "set", "api_token", "--env", "dev"])
        .write_stdin(CANARY)
        .assert()
        .success();
    let echo = "headers:\n  - name: X-Token\n    value: '{{api_token}}'\n\
                captures:\n  - var: echoed\n    from: body_regex\n    \
                pattern: '\"x-token\",\"([^\"]+)\"'\n    required: true\n";
    write(
        ws.path(),
        "api/1-echo.rman.yaml",
        &request("echo", "{{base_url}}/echo", echo),
    );
    write(
        ws.path(),
        "api/2-use.rman.yaml",
        &request(
            "use",
            "{{base_url}}/use/{{echoed}}",
            "settings:\n  cookies: false\n",
        ),
    );
    ws
}

#[test]
fn a_secret_reaching_a_later_request_through_a_capture_is_redacted() {
    let server = Server::http();
    let ws = derived_secret_workspace(&server);
    let out = rustman(ws.path())
        .args(["run", "api", "--env", "dev"])
        .args(["--reporter", "json:out/r.json", "--reporter", "json"])
        .assert()
        .success()
        .get_output()
        .clone();
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    assert!(!stdout.contains(CANARY), "stdout: {stdout}");
    assert!(!stderr.contains(CANARY), "stderr: {stderr}");
    let report = fs::read_to_string(ws.path().join("out/r.json")).unwrap();
    assert!(!report.contains(CANARY), "report: {report}");
    assert!(report.contains("/use/«redacted:echoed»"), "{report}");

    let history = history_text(ws.path());
    assert!(!history.contains(CANARY), "history: {history}");
    assert!(history.contains("/use/«redacted:echoed»"), "{history}");
    assert!(
        history.contains("a value derived from a secret"),
        "{history}"
    );

    let list = rustman(ws.path())
        .args(["history", "list", "--json"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let rows: serde_json::Value = serde_json::from_slice(&list).unwrap();
    let newest = rows[0]["id"].as_str().unwrap().to_owned();
    let shown = rustman(ws.path())
        .args(["history", "show", &newest])
        .assert()
        .success()
        .get_output()
        .clone();
    let shown = String::from_utf8_lossy(&shown.stdout);
    assert!(!shown.contains(CANARY), "history show: {shown}");
    assert!(shown.contains("«redacted:echoed»"), "{shown}");

    let replay = rustman(ws.path())
        .args(["history", "replay", &newest, "--no-history"])
        .assert()
        .code(3)
        .get_output()
        .clone();
    let replay = String::from_utf8_lossy(&replay.stderr);
    assert!(!replay.contains(CANARY), "{replay}");
    assert!(replay.contains("cannot be replayed"), "{replay}");
}

const SPILLED: &str = "Sp1llCANARY-é\"q";

/// A workspace whose bodies over 1 KiB spill, with `big` sending the secret
/// `tok` to `/mirror`, which echoes it escaped across the 64 KiB chunk
/// boundary of the spill file.
fn spilled_workspace(server: &Server) -> tempfile::TempDir {
    let ws = workspace(server);
    let env = format!(
        "variables:\n  base_url: {}\nsecrets: [tok]\n",
        server.url("")
    );
    write(ws.path(), "environments/dev.yaml", &env);
    let toml = fs::read_to_string(ws.path().join("rustman.toml")).unwrap();
    write(
        ws.path(),
        "rustman.toml",
        &format!("{toml}[http]\nmax_body_bytes = 1024\n"),
    );
    rustman(ws.path())
        .args(["secret", "set", "tok", "--env", "dev"])
        .write_stdin(SPILLED)
        .assert()
        .success();
    let big = "method: POST\nbody:\n  type: text\n  content: '{{tok}}'\n";
    write(
        ws.path(),
        "big.rman.yaml",
        &request("big", "{{base_url}}/mirror?pad=65530&tail=5000", big),
    );
    ws
}

#[test]
fn a_spilled_body_is_printed_redacted_unless_asked() {
    let server = Server::http();
    let ws = spilled_workspace(&server);
    let stdout = |args: &[&str]| {
        let out = rustman(ws.path()).args(args).output().unwrap();
        assert_eq!(out.status.code(), Some(0), "{out:?}");
        String::from_utf8_lossy(&out.stdout).into_owned()
    };
    for args in [
        &["send", "big", "--env", "dev"][..],
        &["send", "big", "--env", "dev", "--body-only"],
    ] {
        let printed = stdout(args);
        assert!(!printed.contains("CANARY"), "{args:?}: no canary");
        assert!(printed.contains(r#"AAA"«redacted:tok»"BBB"#), "{args:?}");
    }
    let shown = stdout(&[
        "send",
        "big",
        "--env",
        "dev",
        "--body-only",
        "--show-secrets",
    ]);
    assert!(
        shown.contains(r#"Sp1llCANARY-\u00e9\"q"#),
        "--show-secrets prints it"
    );
    stdout(&["send", "big", "--env", "dev", "-o", "out.bin"]);
    let saved = fs::read_to_string(ws.path().join("out.bin")).unwrap();
    assert!(
        saved.contains(r#"Sp1llCANARY-\u00e9\"q"#),
        "-o saves it as received"
    );

    let list = stdout(&["history", "list", "--json"]);
    let rows: serde_json::Value = serde_json::from_str(&list).unwrap();
    let id = rows[0]["id"].as_str().unwrap().to_owned();
    let replayed = stdout(&["history", "replay", &id]);
    assert!(!replayed.contains("CANARY"), "replay: no canary");
    assert!(replayed.contains(r#"AAA"«redacted:tok»"BBB"#));
}

/// A workspace whose `dev` environment declares `secrets` and sets each
/// to its value.
fn secret_workspace(server: &Server, secrets: &[(&str, &str)]) -> tempfile::TempDir {
    let ws = workspace(server);
    let names: Vec<&str> = secrets.iter().map(|(n, _)| *n).collect();
    let env = format!(
        "variables:\n  base_url: {}\nsecrets: [{}]\n",
        server.url(""),
        names.join(", ")
    );
    write(ws.path(), "environments/dev.yaml", &env);
    for (name, value) in secrets {
        rustman(ws.path())
            .args(["secret", "set", name, "--env", "dev"])
            .write_stdin(*value)
            .assert()
            .success();
    }
    ws
}

fn stdout_of(ws: &Path, args: &[&str]) -> Vec<u8> {
    let out = rustman(ws).args(args).output().unwrap();
    assert!(
        out.status.code().is_some_and(|c| c <= 1),
        "{args:?}: {out:?}"
    );
    out.stdout
}

fn has(haystack: &[u8], needle: &str) -> bool {
    haystack
        .windows(needle.len())
        .any(|w| w == needle.as_bytes())
}

/// Every secret the requests of a run reference is redacted from the first
/// request on: `/token` echoes `later`'s value before `2-later` uses it.
/// A secret no request of the run references is not looked up.
#[test]
fn a_run_redacts_every_secret_its_requests_reference_from_the_start() {
    let server = Server::http();
    let ws = secret_workspace(&server, &[("later", TOKEN)]);
    write(
        ws.path(),
        "api/1-token.rman.yaml",
        &request("token", "{{base_url}}/token", ""),
    );
    let later = "headers:\n  - name: X-Later\n    value: '{{later}}'\n";
    write(
        ws.path(),
        "api/2-later.rman.yaml",
        &request("later", "{{base_url}}/echo", later),
    );
    let run = stdout_of(
        ws.path(),
        &["run", "api", "--env", "dev", "--reporter", "json"],
    );
    assert!(!has(&run, TOKEN), "{}", String::from_utf8_lossy(&run));
    assert!(has(&run, r#"\"access_token\":\"«redacted:later»\""#));
    // Outside that scope (no request of this run references it), the
    // server's token is shown as sent.
    let alone = stdout_of(
        ws.path(),
        &[
            "run",
            "api/1-token.rman.yaml",
            "--env",
            "dev",
            "--reporter",
            "json",
        ],
    );
    assert!(has(&alone, TOKEN));
    let sent = stdout_of(
        ws.path(),
        &["send", "api/2-later.rman.yaml", "--env", "dev"],
    );
    assert!(!has(&sent, TOKEN));
}

/// A body that is not valid UTF-8 (Latin-1 JSON) is redacted byte-wise on
/// stdout too; only `-o` and `--show-secrets` keep it as received.
#[test]
fn a_binary_body_is_printed_redacted() {
    let server = Server::http();
    const CANARY: &str = "L4t1n-CANARY-x9";
    let ws = secret_workspace(&server, &[("tok", CANARY)]);
    let body = "method: POST\nbody:\n  type: text\n  content: '{{tok}}'\n";
    write(
        ws.path(),
        "latin.rman.yaml",
        &request("latin", "{{base_url}}/latin1", body),
    );
    for args in [
        &["send", "latin", "--env", "dev"][..],
        &["send", "latin", "--env", "dev", "--body-only"],
    ] {
        let out = stdout_of(ws.path(), args);
        assert!(!has(&out, CANARY), "{args:?}");
        assert!(has(&out, "\"echo\":\"«redacted:tok»\""), "{args:?}");
        assert!(
            out.windows(4).any(|w| w == b"Bel\xe9"),
            "the other bytes stay"
        );
    }
    let shown = stdout_of(
        ws.path(),
        &[
            "send",
            "latin",
            "--env",
            "dev",
            "--body-only",
            "--show-secrets",
        ],
    );
    assert!(has(&shown, CANARY));
    stdout_of(
        ws.path(),
        &["send", "latin", "--env", "dev", "-o", "latin.json"],
    );
    assert!(has(
        &fs::read(ws.path().join("latin.json")).unwrap(),
        CANARY
    ));
}

/// Fifty secrets of 1 KiB, all sent and echoed: `send` stays fast (a
/// generous bound for a debug build; release takes well under a second).
#[test]
fn a_send_with_fifty_long_secrets_stays_fast() {
    let server = Server::http();
    let values: Vec<(String, String)> = (0..50)
        .map(|j| {
            let value: String = (0..1024)
                .map(|i| ['a', 'b', '/', '&', '<', '~', 'Q', '9', '.', '-'][(i * 7 + j * 3) % 10])
                .collect();
            (format!("s{j}"), format!("{j:02}{value}"))
        })
        .collect();
    let pairs: Vec<(&str, &str)> = values
        .iter()
        .map(|(n, v)| (n.as_str(), v.as_str()))
        .collect();
    let ws = secret_workspace(&server, &pairs);
    let content: Vec<String> = values.iter().map(|(n, _)| format!("{{{{{n}}}}}")).collect();
    let body = format!(
        "method: POST\nbody:\n  type: text\n  content: '{}'\n",
        content.join(",")
    );
    write(
        ws.path(),
        "all.rman.yaml",
        &request("all", "{{base_url}}/echo", &body),
    );
    let started = std::time::Instant::now();
    let out = stdout_of(ws.path(), &["send", "all", "--env", "dev"]);
    let elapsed = started.elapsed();
    assert!(elapsed < std::time::Duration::from_secs(60), "{elapsed:?}");
    assert!(!has(&out, &values[7].1), "echoed secrets are redacted");
    assert!(has(&out, "«redacted:s7»"));
}
