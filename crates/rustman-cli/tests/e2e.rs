//! End-to-end smoke test (task 8.1) and redaction audit (task 8.2): a
//! workspace goes through `init`, an environment and a secret, a run with
//! the json and junit reporters, `export curl`, a failing request and
//! history `list`, `show` and `replay`, all against the in-process server.
//! The audit then searches every file the flow left behind and everything
//! it printed for the canary secrets. A second, non-ASCII secret with a `"`
//! comes back `\u`-escaped in a failing assertion message cut mid-secret
//! and across the preview cut of a spilled body. Nothing leaves the
//! loopback interface.

mod common;

use std::fs;
use std::path::{Path, PathBuf};

use assert_cmd::Command;
use common::{Server, closed_port, rustman, valid_report, write};
use serde_json::Value;

/// The secret; plain ASCII, so its JSON- and URL-encoded forms are itself.
const CANARY: &str = "e2e-c4n4ry-9Zq7Xw-s3cr3t";

/// The second secret: non-ASCII and with a `"`, so it is echoed escaped.
const QUOTED: &str = "Kq8\u{e9}\"Vb-m0nk3y-Tz";

/// Pieces of the secrets no artifact may hold: any part of a secret left
/// by a cut, escaped or not, contains one of them.
const PIECES: [&str; 4] = [CANARY, "c4n4ry", "Kq8", "m0nk3y"];

/// The secret store itself is the one file allowed to hold the value.
const SECRETS_FILE: &str = ".rustman/secrets.yaml";

/// What one command printed.
struct Output {
    command: String,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
}

/// The finished flow: its files and everything it printed.
struct Flow {
    _server: Server,
    tmp: tempfile::TempDir,
    outputs: Vec<Output>,
    replay_id: String,
}

impl Flow {
    fn ws(&self) -> PathBuf {
        self.tmp.path().join("shop")
    }

    fn data_dir(&self) -> PathBuf {
        self.tmp.path().join("data")
    }

    fn output(&self, prefix: &str) -> &Output {
        self.outputs
            .iter()
            .find(|o| o.command.starts_with(prefix))
            .unwrap_or_else(|| panic!("no command '{prefix}'"))
    }

    /// `rustman args...` in the workspace, with every log level on.
    fn cmd(&self, args: &[&str]) -> Command {
        let mut cmd = rustman(&self.ws());
        cmd.env("RUSTMAN_DATA_DIR", self.data_dir())
            .env("NO_COLOR", "1")
            .env_remove("RUST_LOG")
            .arg("-vvv")
            .args(args);
        cmd
    }

    /// Run `args`, expect exit `code`, and keep what it printed.
    fn step(&mut self, args: &[&str], stdin: Option<&str>, code: i32) -> &Output {
        let mut cmd = self.cmd(args);
        if let Some(input) = stdin {
            cmd.write_stdin(input);
        }
        self.finish(cmd, args, code)
    }

    /// Like `step`, with `RUST_LOG=trace` logging every crate.
    fn step_traced(&mut self, args: &[&str], code: i32) -> &Output {
        let mut cmd = self.cmd(args);
        cmd.env("RUST_LOG", "trace");
        self.finish(cmd, args, code)
    }

    fn finish(&mut self, mut cmd: Command, args: &[&str], code: i32) -> &Output {
        let out = cmd.output().unwrap();
        let output = Output {
            command: args.join(" "),
            stdout: out.stdout,
            stderr: out.stderr,
        };
        assert_eq!(
            out.status.code(),
            Some(code),
            "rustman {}\nstdout:\n{}\nstderr:\n{}",
            output.command,
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr),
        );
        self.outputs.push(output);
        self.outputs.last().unwrap()
    }
}

/// The environment `e2e`: the server's address, and two secrets: the
/// stored token and a session the run captures.
fn environment(server: &Server) -> String {
    format!(
        "variables:\n  base_url: {}\nsecrets:\n  - api_token\n  - session\n  - quoted\n",
        server.url("")
    )
}

/// Sends the token in a header, the query, a credential header and a JSON
/// body to the echo route, checks the server saw the real value, and
/// captures it back from the echo as the secret `session`, and as the
/// plain variable `echoed`, which must not reach the state file.
const SECURE: &str = r#"version: 1
kind: http
name: Secure
method: POST
url: '{{base_url}}/echo?key={{api_token}}'
headers:
  - name: Authorization
    value: 'Bearer {{api_token}}'
  - name: X-Api-Key
    value: '{{api_token}}'
body:
  type: json
  content:
    token: '{{api_token}}'
assertions:
  - status: 200
  - body:
      contains: '"x-api-key","{{api_token}}"'
  - json:
      path: $.method
      equals: POST
captures:
  - var: session
    from: body_regex
    pattern: '"x-api-key","([^"]+)"'
    required: true
    persist: true
  - var: echoed
    from: body_regex
    pattern: '"x-api-key","([^"]+)"'
    persist: true
"#;

/// Uses the captured secret in the next request of the folder.
const SESSION: &str = r#"version: 1
kind: http
name: Session
url: '{{base_url}}/echo'
headers:
  - name: X-Session
    value: '{{session}}'
assertions:
  - status: 200
  - body:
      contains: '"x-session","{{api_token}}"'
"#;

/// Sends the quoted secret to `/mirror`, which echoes it `\u`-escaped after
/// 189 `A`s: the failing assertion's message is cut 200 characters in,
/// mid-secret. The piece `Vb-m0nk3y` it captures must not be persisted.
const ESCAPED: &str = r#"version: 1
kind: http
name: Escaped
method: POST
url: '{{base_url}}/mirror?pad=189&tail=20'
body:
  type: text
  content: '{{quoted}}'
assertions:
  - body:
      contains: zzz
captures:
  - var: piece
    from: body_regex
    pattern: 'Vb-m0n[k]3y'
    persist: true
"#;

/// The quoted secret straddles the 64 KiB preview cut of a spilled body.
const SPILLED: &str = r#"version: 1
kind: http
name: Spilled
method: POST
url: '{{base_url}}/mirror?pad=65528&tail=100000'
body:
  type: text
  content: '{{quoted}}'
assertions:
  - status: 200
"#;

/// Fails to connect with the token in its URL, for the error message.
fn broken(port: u16) -> String {
    format!(
        "version: 1\nkind: http\nname: Broken\n\
         url: 'http://127.0.0.1:{port}/down?key={{{{api_token}}}}'\n"
    )
}

fn flow() -> Flow {
    let server = Server::http();
    let tmp = tempfile::tempdir().unwrap();
    let mut flow = Flow {
        _server: server,
        tmp,
        outputs: Vec::new(),
        replay_id: String::new(),
    };
    fs::create_dir_all(flow.ws()).unwrap();
    flow.step(&["init", "."], None, 0);

    write(
        &flow.ws(),
        "environments/e2e.yaml",
        &environment(&flow._server),
    );
    write(&flow.ws(), "examples/secure.rman.yaml", SECURE);
    write(&flow.ws(), "examples/session.rman.yaml", SESSION);
    write(
        &flow.ws(),
        "broken/broken.rman.yaml",
        &broken(closed_port()),
    );
    write(
        &flow.ws(),
        "examples/_collection.yaml",
        "order:\n  - ping\n  - secure\n  - session\n",
    );
    flow.step(&["env", "list"], None, 0);
    flow.step(
        &["secret", "set", "api_token", "--env", "e2e"],
        Some(CANARY),
        0,
    );
    flow.step(
        &["secret", "set", "quoted", "--env", "e2e"],
        Some(QUOTED),
        0,
    );
    flow.step(&["secret", "list", "--env", "e2e"], None, 0);
    leaks(&mut flow);

    flow.step(
        &[
            "run",
            "examples",
            "--env",
            "e2e",
            "--reporter",
            "json:out/report.json",
            "--reporter",
            "junit:out/junit.xml",
            "--reporter",
            "pretty",
        ],
        None,
        0,
    );
    flow.step(
        &["run", "broken", "--env", "e2e", "--reporter", "json"],
        None,
        3,
    );
    flow.step_traced(&["send", "examples/secure", "--env", "e2e"], 0);
    flow.step(
        &["export", "curl", "examples/secure", "--env", "e2e"],
        None,
        0,
    );

    let list = flow.step(&["history", "list", "--json"], None, 0);
    let rows: Vec<Value> = serde_json::from_slice(&list.stdout).unwrap();
    let secure = rows
        .iter()
        .find(|row| row["request"] == "examples/secure.rman.yaml")
        .expect("the secure request is in history");
    let id = secure["id"].as_str().unwrap().to_owned();
    flow.step(&["history", "list"], None, 0);
    flow.step(&["history", "show", &id], None, 0);
    flow.step(&["history", "replay", &id], None, 0);
    flow.replay_id = id;
    flow
}

/// Run the `leaks` folder: first without history, which leaves no spill
/// file behind, then with it, into `out/leaks.json`.
fn leaks(flow: &mut Flow) {
    let toml = fs::read_to_string(flow.ws().join("rustman.toml")).unwrap();
    write(
        &flow.ws(),
        "rustman.toml",
        &format!("{toml}\n[http]\nmax_body_bytes = 100000\n"),
    );
    write(&flow.ws(), "leaks/1-escaped.rman.yaml", ESCAPED);
    write(&flow.ws(), "leaks/2-spilled.rman.yaml", SPILLED);
    flow.step(&["run", "leaks", "--env", "e2e", "--no-history"], None, 1);
    assert_eq!(spill_files(&flow.ws()), Vec::<PathBuf>::new());
    flow.step(
        &[
            "run",
            "leaks",
            "--env",
            "e2e",
            "--reporter",
            "json:out/leaks.json",
        ],
        None,
        1,
    );
    assert_eq!(spill_files(&flow.ws()).len(), 1, "history keeps the spill");
}

fn spill_files(ws: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();
    collect_files(&ws.join(".rustman/bodies"), &mut files);
    files
}

#[test]
fn init_secret_run_and_replay_end_to_end() {
    let flow = flow();
    let ws = flow.ws();

    let report = valid_report(&fs::read(ws.join("out/report.json")).unwrap());
    assert_eq!(report["kind"], "run");
    assert_eq!(report["environment"], "e2e");
    let ids: Vec<&str> = report["requests"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["id"].as_str().unwrap())
        .collect();
    assert_eq!(
        ids,
        ["examples/ping", "examples/secure", "examples/session"]
    );
    assert_eq!(report["summary"]["passed"], 3, "{report:#}");
    assert_eq!(report["summary"]["total"], 3);

    let xml = fs::read_to_string(ws.join("out/junit.xml")).unwrap();
    let doc = roxmltree::Document::parse(&xml).expect("well-formed JUnit XML");
    let cases = doc.descendants().filter(|n| n.has_tag_name("testcase"));
    assert_eq!(cases.count(), 3, "{xml}");
    assert!(
        !doc.descendants().any(|n| n.has_tag_name("failure")),
        "{xml}"
    );

    // The captured secret went to the secret store, not the state file.
    let store = fs::read_to_string(ws.join(SECRETS_FILE)).unwrap();
    assert!(store.contains("session"), "the captured secret is stored");
    let state = fs::read_to_string(ws.join(".rustman/state/e2e.yaml")).unwrap_or_default();
    assert!(!state.contains("session"), "{state}");

    // The broken request errored on the network, after resolving the secret.
    let broken = valid_report(&flow.output("run broken").stdout);
    assert_eq!(broken["summary"]["errored"], 1);

    // The replay re-read the secret from the store and sent its real value:
    // the server echoed it, and the echo scan redacted it.
    let replay = String::from_utf8_lossy(&flow.output("history replay").stdout).into_owned();
    assert!(replay.starts_with("HTTP/1.1 200\n"), "{replay}");
    assert!(
        replay.contains(r#""x-api-key","«redacted:api_token»""#),
        "{replay}"
    );
    let show = String::from_utf8_lossy(&flow.output("history show").stdout).into_owned();
    assert!(show.contains(&flow.replay_id), "{show}");
}

#[test]
fn show_secrets_reaches_the_terminal_only() {
    let mut flow = flow();
    let before = history_files(&flow.ws());
    let shown = flow.step(
        &["send", "examples/secure", "--env", "e2e", "--show-secrets"],
        None,
        0,
    );
    let stdout = String::from_utf8_lossy(&shown.stdout).into_owned();
    assert!(stdout.contains(CANARY), "{stdout}");
    // Not in the logs or the history entry the send wrote.
    assert!(!contains(&shown.stderr, CANARY));
    let after = history_files(&flow.ws());
    assert_eq!(after.len(), before.len() + 1);
    for file in after {
        let text = fs::read(&file).unwrap();
        assert!(!contains(&text, CANARY), "{}", file.display());
    }
}

/// Task 8.2: no artifact of the end-to-end flow holds the canary secret.
#[test]
fn redaction_audit_finds_no_canary_in_any_artifact() {
    let flow = flow();
    let mut files = Vec::new();
    collect_files(flow.tmp.path(), &mut files);
    let secrets_file = flow.ws().join(SECRETS_FILE);
    // Spilled bodies are working state kept as received (docs/environments.md).
    let bodies = flow.ws().join(".rustman/bodies");
    let audited: Vec<&PathBuf> = files
        .iter()
        .filter(|f| **f != secrets_file && !f.starts_with(&bodies))
        .collect();

    // The flow wrote the sinks the audit is about.
    let names: Vec<String> = audited.iter().map(|f| f.display().to_string()).collect();
    for sink in ["report.json", "junit.xml", "history", "rustman.toml"] {
        assert!(
            names.iter().any(|n| n.contains(sink)),
            "no {sink} in {names:#?}"
        );
    }
    let mut leaks: Vec<String> = audited
        .iter()
        .filter(|f| holds_a_piece(&fs::read(f).unwrap()))
        .map(|f| f.display().to_string())
        .collect();
    for output in &flow.outputs {
        for (stream, bytes) in [("stdout", &output.stdout), ("stderr", &output.stderr)] {
            if holds_a_piece(bytes) {
                leaks.push(format!("rustman {} ({stream})", output.command));
            }
        }
    }
    assert!(leaks.is_empty(), "the canary leaked into: {leaks:#?}");

    // The secret was really used: the store holds it and the outputs
    // carry its placeholder.
    assert!(contains(&fs::read(&secrets_file).unwrap(), CANARY));
    let warned = String::from_utf8_lossy(&flow.output("run examples").stdout).into_owned()
        + &String::from_utf8_lossy(&flow.output("run examples").stderr);
    assert!(
        warned.contains("capture 'echoed': not persisted: the value contains a secret"),
        "{warned}"
    );
    let report = fs::read_to_string(flow.ws().join("out/report.json")).unwrap();
    assert!(report.contains("«redacted:api_token»"), "{report}");
    // The escaped echo was cut mid-secret in the message, and the secret
    // straddled the spilled preview's cut: both show the placeholder.
    let leaks = fs::read_to_string(flow.ws().join("out/leaks.json")).unwrap();
    let cut = leaks.matches(r#"A\\\"«redacted:quoted»\"…"#).count();
    assert_eq!(cut, 2, "message and actual, cut after the secret: {leaks}");
    let preview = leaks.matches(r#"A\"«redacted:quoted»""#).count();
    assert_eq!(
        preview, 1,
        "the preview ends with the whole secret: {leaks}"
    );
    assert!(leaks.contains("\"body_truncated\": true"), "{leaks}");
    assert!(
        leaks.contains("capture 'piece': not persisted: the value contains a secret"),
        "{leaks}"
    );
    let curl = String::from_utf8_lossy(&flow.output("export curl").stdout).into_owned();
    assert!(curl.contains("«redacted:api_token»"), "{curl}");
    let logs = String::from_utf8_lossy(&flow.output("run examples").stderr).into_owned();
    assert!(logs.contains("«redacted:"), "{logs}");
    let traced = String::from_utf8_lossy(&flow.output("send").stderr).into_owned();
    assert!(traced.contains("TRACE"), "{traced}");
    let error = String::from_utf8_lossy(&flow.output("run broken").stdout).into_owned();
    assert!(error.contains("\"error\""), "{error}");
}

fn history_files(ws: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();
    collect_files(&ws.join(".rustman/history"), &mut files);
    files
}

fn collect_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries {
        let path = entry.unwrap().path();
        if path.is_dir() {
            collect_files(&path, out);
        } else {
            out.push(path);
        }
    }
}

fn holds_a_piece(haystack: &[u8]) -> bool {
    PIECES.iter().any(|piece| contains(haystack, piece))
}

fn contains(haystack: &[u8], needle: &str) -> bool {
    haystack
        .windows(needle.len())
        .any(|window| window == needle.as_bytes())
}
