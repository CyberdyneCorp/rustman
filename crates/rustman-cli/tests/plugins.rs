//! Plugins in headless runs (add-plugin-system tasks 2.2, 4.2 and section
//! 5): a tampered plugin and an untrusted plugin set stop `rustman run`
//! with exit code 2 before any request is sent; `--trust-plugins` loads
//! them with their grants; auth, assertion and generator plugins act on
//! the requests that use them.

#![cfg(feature = "plugins")]

mod common;

use std::path::Path;

use common::{Server, request, rustman, unique, workspace, write};
use hmac::{Hmac, KeyInit, Mac};
use predicates::prelude::*;
use rustman_plugin::fixtures;
use rustman_plugin::pin::sha256_hex as sha256;
use rustman_test_support::routes::timed_hits;

/// A server of fixed files, on its own runtime.
struct Files {
    server: rustman_test_support::FileServer,
    _runtime: tokio::runtime::Runtime,
}

impl Files {
    fn new() -> Self {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .enable_all()
            .build()
            .unwrap();
        let server = runtime.block_on(rustman_test_support::FileServer::http());
        Self {
            server,
            _runtime: runtime,
        }
    }
}

const UNTRUSTED: &str =
    "workspace plugins are not trusted (run interactively or pass --trust-plugins)";

/// Write `wasm` (with a manifest for `points` and `caps`) as
/// `plugins/<name>.wasm` and declare it with `grants`.
fn add_plugin(
    root: &Path,
    name: &str,
    wasm: &[u8],
    points: &[&str],
    caps: &[&str],
    grants: &[&str],
) {
    let manifest = fixtures::manifest(name, points, caps);
    let wasm = fixtures::with_manifest(wasm, &manifest);
    let path = root.join(format!("plugins/{name}.wasm"));
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, &wasm).unwrap();
    let grants: Vec<String> = grants.iter().map(|g| format!("\"{g}\"")).collect();
    let table = format!(
        "\n[plugins.{name}]\nsource = \"plugins/{name}.wasm\"\nsha256 = \"{}\"\ngrants = [{}]\n",
        sha256(&wasm),
        grants.join(", ")
    );
    let settings = root.join("rustman.toml");
    let mut text = std::fs::read_to_string(&settings).unwrap();
    text.push_str(&table);
    std::fs::write(settings, text).unwrap();
}

/// A workspace with one `/timed` request and the `faker` plugin.
fn timed_workspace(server: &Server) -> (tempfile::TempDir, String) {
    let dir = workspace(server);
    let prefix = unique(dir.path());
    let url = format!("{{{{base_url}}}}/timed/0?id={prefix}one");
    write(dir.path(), "one.rman.yaml", &request("one", &url, ""));
    add_plugin(
        dir.path(),
        "faker",
        &fixtures::faker(),
        &["generator"],
        &["random"],
        &["random"],
    );
    (dir, prefix)
}

#[test]
fn tampered_plugin() {
    let server = Server::http();
    let (dir, prefix) = timed_workspace(&server);
    let root = dir.path();
    let pinned = std::fs::read(root.join("plugins/faker.wasm")).unwrap();
    write(root, "plugins/faker.wasm", "tampered plugin bytes");
    let expected = format!(
        "plugin faker: sha256 mismatch (expected {}, got {})",
        sha256(&pinned),
        sha256(b"tampered plugin bytes")
    );
    rustman(root)
        .args(["run", ".", "--env", "dev", "--trust-plugins"])
        .assert()
        .code(2)
        .stderr(predicate::str::contains(expected));
    assert!(timed_hits(&prefix).is_empty(), "a request was sent");

    // The pinned bytes pass the check and the run goes on.
    std::fs::write(root.join("plugins/faker.wasm"), pinned).unwrap();
    rustman(root)
        .args(["run", ".", "--env", "dev", "--trust-plugins"])
        .assert()
        .code(0);
    assert_eq!(timed_hits(&prefix).len(), 1);
}

/// "CI run": a fresh checkout of a workspace with plugins.
#[test]
fn ci_run() {
    let server = Server::http();
    let (dir, prefix) = timed_workspace(&server);
    rustman(dir.path())
        .args(["run", ".", "--env", "dev"])
        .assert()
        .code(2)
        .stderr(predicate::str::contains(UNTRUSTED));
    assert!(timed_hits(&prefix).is_empty(), "a request was sent");
    rustman(dir.path())
        .args(["send", "one", "--env", "dev"])
        .assert()
        .code(2)
        .stderr(predicate::str::contains(UNTRUSTED));
    assert!(timed_hits(&prefix).is_empty(), "a request was sent");
}

/// "CI run with opt-in": the plugins load with exactly their grants, and
/// the approval is recorded until the set changes.
#[test]
fn ci_run_with_opt_in() {
    let server = Server::http();
    let (dir, prefix) = timed_workspace(&server);
    let root = dir.path();
    // The generator runs with its `random` grant.
    let body = "method: POST\nbody:\n  type: text\n  content: '{{$faker.email}}'\n";
    write(
        root,
        "two.rman.yaml",
        &request("two", "{{base_url}}/echo", body),
    );
    rustman(root)
        .args(["run", ".", "--env", "dev", "--trust-plugins"])
        .assert()
        .code(0);
    assert_eq!(timed_hits(&prefix).len(), 1);
    let trust = std::fs::read_to_string(root.join(".rustman/trust.yaml")).unwrap();
    assert!(trust.contains("plugins:"), "{trust}");

    // Trusted now: no flag needed.
    rustman(root)
        .args(["run", ".", "--env", "dev"])
        .assert()
        .code(0);
    assert_eq!(timed_hits(&prefix).len(), 2);

    // The grants changed: the set must be trusted again.
    let settings = std::fs::read_to_string(root.join("rustman.toml")).unwrap();
    let changed = settings.replace("grants = [\"random\"]", "grants = []");
    write(root, "rustman.toml", &changed);
    rustman(root)
        .args(["run", ".", "--env", "dev"])
        .assert()
        .code(2)
        .stderr(predicate::str::contains(UNTRUSTED));
    assert_eq!(timed_hits(&prefix).len(), 2);

    // With the opt-in, the plugin loads without `random`: the generator is
    // refused, so the request that uses it errors and the other passes.
    rustman(root)
        .args(["run", ".", "--env", "dev", "--trust-plugins"])
        .assert()
        .code(3)
        .stdout(predicate::str::contains(
            "$faker.email (plugin faker: random)",
        ));
    assert_eq!(timed_hits(&prefix).len(), 3);
}

/// "Custom generator": each occurrence is replaced by a value the plugin
/// produced.
#[test]
fn custom_generator() {
    let server = Server::http();
    let (dir, _) = timed_workspace(&server);
    let body =
        "method: POST\nbody:\n  type: text\n  content: '{{$faker.email}} {{$faker.email}}'\n";
    write(
        dir.path(),
        "two.rman.yaml",
        &request("two", "{{base_url}}/echo", body),
    );
    let output = rustman(dir.path())
        .args([
            "send",
            "two",
            "--env",
            "dev",
            "--trust-plugins",
            "--body-only",
        ])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(0), "{output:?}");
    let echo: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    let sent = echo["body"].as_str().unwrap();
    let emails: Vec<&str> = sent.split(' ').collect();
    assert_eq!(emails.len(), 2, "{sent}");
    for email in &emails {
        assert!(
            email.starts_with("user-") && email.ends_with("@example.test"),
            "{sent}"
        );
    }
    assert_ne!(emails[0], emails[1]);
}

const KEY: &str = "hmac-key-CANARY-7f3a";

/// A workspace whose `dev` environment holds `hmac_key`, with an auth
/// plugin `name` built from `wasm`.
fn auth_workspace(server: &Server, name: &str, wasm: &[u8], grants: &[&str]) -> tempfile::TempDir {
    let dir = workspace(server);
    let env = format!(
        "variables:\n  base_url: {}\nsecrets: [hmac_key]\n",
        server.url("")
    );
    write(dir.path(), "environments/dev.yaml", &env);
    rustman(dir.path())
        .args(["secret", "set", "hmac_key", "--env", "dev"])
        .write_stdin(KEY)
        .assert()
        .success();
    let caps = ["secrets:read", "clock"];
    add_plugin(dir.path(), name, wasm, &["auth"], &caps, grants);
    dir
}

/// "HMAC signing": the sent request carries the headers the plugin
/// returned, signed with the granted secret.
#[test]
fn hmac_signing() {
    let server = Server::http();
    let grants = ["secrets:read:hmac_key", "clock"];
    let dir = auth_workspace(&server, "hmac", &fixtures::hmac_signer(), &grants);
    let auth = "auth: {type: plugin, scheme: hmac/v1, key_var: hmac_key}\n";
    write(
        dir.path(),
        "signed.rman.yaml",
        &request("signed", "{{base_url}}/echo?a=1", auth),
    );
    let output = rustman(dir.path())
        .args([
            "send",
            "signed",
            "--env",
            "dev",
            "--trust-plugins",
            "--body-only",
        ])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(0), "{output:?}");
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(!stdout.contains(KEY), "{stdout}");
    let echo: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    let header = |name: &str| {
        echo["headers"]
            .as_array()
            .unwrap()
            .iter()
            .find(|h| h[0] == name)
            .map(|h| h[1].as_str().unwrap().to_owned())
            .unwrap_or_else(|| panic!("no {name}: {echo}"))
    };
    let timestamp = header("x-timestamp");
    let message = format!("{timestamp}\nGET\n{}", server.url("/echo?a=1"));
    let mut mac = <Hmac<sha2::Sha256> as KeyInit>::new_from_slice(KEY.as_bytes()).unwrap();
    mac.update(message.as_bytes());
    let expected: String = mac
        .finalize()
        .into_bytes()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    assert_eq!(header("x-signature"), expected);
}

/// "Plugin tries to change the host": the request fails and nothing is
/// sent.
#[test]
fn plugin_tries_to_change_the_host() {
    let server = Server::http();
    let dir = auth_workspace(&server, "evil", &fixtures::host_rewriter(), &[]);
    let prefix = unique(dir.path());
    let url = format!("{{{{base_url}}}}/timed/0?id={prefix}one");
    let auth = "auth: {type: plugin, scheme: evil/v1}\n";
    write(dir.path(), "one.rman.yaml", &request("one", &url, auth));
    rustman(dir.path())
        .args(["send", "one", "--env", "dev", "--trust-plugins"])
        .assert()
        .code(3)
        .stderr(predicate::str::contains(
            "error: plugin evil: auth plugins may not change method, host or body",
        ));
    assert!(timed_hits(&prefix).is_empty(), "a request was sent");
}

/// "Trap during auth": the request is not sent and counts as a request
/// error.
#[test]
fn trap_during_auth() {
    let server = Server::http();
    let dir = auth_workspace(&server, "aws-sigv4", &fixtures::trapping_auth(), &[]);
    let prefix = unique(dir.path());
    let url = format!("{{{{base_url}}}}/timed/0?id={prefix}one");
    let auth = "auth: {type: plugin, scheme: aws-sigv4/v1}\n";
    write(dir.path(), "one.rman.yaml", &request("one", &url, auth));
    rustman(dir.path())
        .args(["send", "one", "--env", "dev", "--trust-plugins"])
        .assert()
        .code(3)
        .stderr(predicate::str::contains(
            "error: plugin aws-sigv4 trapped: ",
        ));
    rustman(dir.path())
        .args(["run", ".", "--env", "dev"])
        .assert()
        .code(3)
        .stdout(predicate::str::contains("plugin aws-sigv4 trapped: "));
    assert!(timed_hits(&prefix).is_empty(), "a request was sent");
}

/// "Custom business rule": the assertion is reported like a built-in one.
#[test]
fn custom_business_rule() {
    let server = Server::http();
    let files = Files::new();
    files.server.serve(
        "/order",
        r#"{"total": 99.0, "lines": [{"amount": 49.0}, {"amount": 49.5}]}"#,
    );
    let dir = workspace(&server);
    add_plugin(
        dir.path(),
        "shop",
        &fixtures::price_check(),
        &["assertion"],
        &[],
        &[],
    );
    let assertions = "assertions:\n  - status: 200\n  - plugin: {fn: shop/price-consistent}\n";
    write(
        dir.path(),
        "order.rman.yaml",
        &request("order", &files.server.url("/order"), assertions),
    );
    rustman(dir.path())
        .args(["send", "order", "--env", "dev", "--trust-plugins"])
        .assert()
        .code(1)
        .stderr(predicate::str::contains(
            "fail: shop/price-consistent: total 99.0 != sum 98.5",
        ));
}

/// Run `rustman mcp serve` in `root`: initialize, list the tools, and
/// return the tool names and stderr.
fn mcp_tools(root: &Path) -> (Vec<String>, String) {
    use std::io::{BufRead, BufReader, Read, Write};
    use std::process::{Command, Stdio};

    let mut child = Command::new(assert_cmd::cargo::cargo_bin("rustman"))
        .current_dir(root)
        .args(["mcp", "serve", "--env", "dev"])
        .env("RUSTMAN_SECRET_STORE", "file")
        .env("RUSTMAN_DATA_DIR", common::test_data_dir())
        .env_remove("RUSTMAN_ENV")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut stdin = child.stdin.take().unwrap();
    let mut stdout = BufReader::new(child.stdout.take().unwrap());
    let mut stderr_pipe = child.stderr.take().unwrap();
    let stderr = std::thread::spawn(move || {
        let mut text = String::new();
        let _ = stderr_pipe.read_to_string(&mut text);
        text
    });
    let requests = [
        serde_json::json!({
            "jsonrpc": "2.0", "id": 1, "method": "initialize",
            "params": {
                "protocolVersion": "2025-06-18",
                "capabilities": {},
                "clientInfo": {"name": "cli-test", "version": "0"}
            }
        }),
        serde_json::json!({"jsonrpc": "2.0", "method": "notifications/initialized"}),
        serde_json::json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list"}),
    ];
    let mut last = String::new();
    for request in &requests {
        stdin.write_all(format!("{request}\n").as_bytes()).unwrap();
        stdin.flush().unwrap();
        if request.get("id").is_some() {
            last.clear();
            stdout.read_line(&mut last).unwrap();
        }
    }
    drop(stdin);
    child.wait().unwrap();
    let tools: serde_json::Value = serde_json::from_str(&last).unwrap();
    let names = tools["result"]["tools"]
        .as_array()
        .unwrap_or_else(|| panic!("{tools}"))
        .iter()
        .map(|t| t["name"].as_str().unwrap().to_owned())
        .collect();
    (names, stderr.join().unwrap())
}

/// "Untrusted plugins in agent mode": the server starts without plugins and
/// says so; no tool can add, remove, trust or grant plugins, trusted or
/// not.
#[test]
fn untrusted_plugins_in_agent_mode() {
    let server = Server::http();
    let (dir, _) = timed_workspace(&server);
    let root = dir.path();
    let (tools, stderr) = mcp_tools(root);
    assert!(
        stderr.contains("plugins not loaded: workspace not trusted"),
        "{stderr}"
    );
    assert!(!tools.is_empty());
    for tool in &tools {
        let lower = tool.to_ascii_lowercase();
        assert!(
            !lower.contains("plugin") && !lower.contains("trust") && !lower.contains("grant"),
            "{tool}"
        );
    }
    // The agent session recorded nothing.
    assert!(!root.join(".rustman/trust.yaml").exists());

    // Trusted by a human: the server loads them, still with no tool for
    // plugins.
    rustman(root)
        .args(["run", ".", "--env", "dev", "--trust-plugins"])
        .assert()
        .code(0);
    let (trusted_tools, stderr) = mcp_tools(root);
    assert!(!stderr.contains("plugins not loaded"), "{stderr}");
    assert_eq!(trusted_tools, tools);
}

/// "Company report format": `--reporter <plugin>/<format>:path` and
/// `report render --format <plugin>/<format>` write exactly the bytes the
/// plugin returned for the redacted report.
#[test]
fn company_report_format() {
    let server = Server::http();
    let (dir, _) = timed_workspace(&server);
    let root = dir.path();
    add_plugin(
        root,
        "acme",
        &fixtures::report_renderer(),
        &["report"],
        &[],
        &[],
    );
    rustman(root)
        .args(["run", ".", "--env", "dev", "--trust-plugins"])
        .args([
            "--reporter",
            "json:out/it.json",
            "--reporter",
            "acme/xlsx:out/run.xlsx",
        ])
        .assert()
        .code(0);
    let json = std::fs::read(root.join("out/it.json")).unwrap();
    let mut expected = fixtures::REPORT_PREFIX.to_vec();
    expected.extend_from_slice(&json);
    assert_eq!(std::fs::read(root.join("out/run.xlsx")).unwrap(), expected);

    rustman(root)
        .args([
            "report",
            "render",
            "out/it.json",
            "--format",
            "acme/xlsx",
            "-o",
            "r.xlsx",
        ])
        .assert()
        .code(0);
    assert_eq!(std::fs::read(root.join("r.xlsx")).unwrap(), expected);

    rustman(root)
        .args(["report", "render", "out/it.json", "--format", "acme/xlsx"])
        .assert()
        .code(2)
        .stderr(predicate::str::contains(
            "--format acme/xlsx writes a file: pass -o <FILE>",
        ));
    rustman(root)
        .args(["run", ".", "--env", "dev", "--reporter", "acme/xlsx"])
        .assert()
        .code(2)
        .stderr(predicate::str::contains(
            "plugin reporter 'acme/xlsx' needs a file",
        ));
}

/// Canary: the `Authorization` header an auth plugin sets from a granted
/// secret reaches the server, and never stdout, the report or history,
/// even when the server echoes it.
#[test]
fn plugin_set_credentials_are_redacted_everywhere() {
    canary_through_auth_header("Authorization");
}

/// Canary: a granted secret an auth plugin puts in a header whose name says
/// nothing about credentials, and that no request references, is redacted
/// from every sink too.
#[test]
fn secrets_a_plugin_echoes_in_any_header_are_redacted() {
    canary_through_auth_header("X-Plain");
}

fn canary_through_auth_header(header: &str) {
    const SECRET: &str = "aws-secret-CANARY-91c4e";
    let server = Server::http();
    let dir = workspace(&server);
    let root = dir.path();
    let env = format!(
        "variables:\n  base_url: {}\nsecrets: [aws_secret_key]\n",
        server.url("")
    );
    write(root, "environments/dev.yaml", &env);
    rustman(root)
        .args(["secret", "set", "aws_secret_key", "--env", "dev"])
        .write_stdin(SECRET)
        .assert()
        .success();
    let grants = ["secrets:read:aws_secret_key"];
    add_plugin(
        root,
        "sig",
        &fixtures::signer_header(header),
        &["auth"],
        &["secrets:read"],
        &grants,
    );
    let extra = "auth: {type: plugin, scheme: sig/v1}\nassertions:\n  - body: {contains: CANARY}\n";
    write(
        root,
        "echo.rman.yaml",
        &request("echo", "{{base_url}}/echo", extra),
    );
    let output = rustman(root)
        .args(["run", ".", "--env", "dev", "--trust-plugins", "-vv"])
        .args(["--reporter", "json:out/report.json", "--reporter", "pretty"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(0), "{output:?}");
    let report = std::fs::read_to_string(root.join("out/report.json")).unwrap();
    let mut history = String::new();
    for entry in std::fs::read_dir(root.join(".rustman/history")).unwrap() {
        let path = entry.unwrap().path();
        if path.extension().is_some_and(|x| x == "json") {
            history.push_str(&std::fs::read_to_string(path).unwrap());
        }
    }
    for (what, text) in [
        (
            "stdout",
            String::from_utf8_lossy(&output.stdout).into_owned(),
        ),
        (
            "stderr",
            String::from_utf8_lossy(&output.stderr).into_owned(),
        ),
        ("report", report),
        ("history", history.clone()),
    ] {
        assert!(!text.contains(SECRET), "{what}: {text}");
    }
    // The server echoed the signed header: history shows the value
    // redacted, and records no header the plugin set.
    assert!(history.contains("«redacted:aws_secret_key»"), "{history}");
    assert!(!history.contains(&format!("\"{header}\"")), "{history}");
}

/// A workspace with a `fetch` generator granted `net:<grant>`, and a
/// request whose body is what the plugin fetched from `url`.
fn fetch_workspace(server: &Server, grant: &str, url: &str) -> tempfile::TempDir {
    let dir = workspace(server);
    add_plugin(
        dir.path(),
        "fetch",
        &fixtures::fetcher(),
        &["generator"],
        &["net"],
        &[&format!("net:{grant}")],
    );
    let body =
        format!("method: POST\nbody:\n  type: text\n  content: '{{{{$fetch.fetch {url}}}}}'\n");
    write(
        dir.path(),
        "fetched.rman.yaml",
        &request("fetched", "{{base_url}}/echo", &body),
    );
    dir
}

fn history_text(root: &Path) -> String {
    let Ok(entries) = std::fs::read_dir(root.join(".rustman/history")) else {
        return String::new();
    };
    let mut text = String::new();
    for entry in entries {
        let path = entry.unwrap().path();
        if path.extension().is_some_and(|x| x == "json") {
            text.push_str(&std::fs::read_to_string(path).unwrap());
        }
    }
    text
}

/// "Granted host", in a real run: the plugin's request is sent through the
/// session's network and recorded in history like any other exchange.
#[test]
fn granted_host_in_a_run() {
    let server = Server::http();
    let authority = server
        .url("")
        .trim_start_matches("http://")
        .trim_end_matches('/')
        .to_owned();
    let url = server.url("/echo?from=plugin");
    let dir = fetch_workspace(&server, &authority, &url);
    let output = rustman(dir.path())
        .args([
            "send",
            "fetched",
            "--env",
            "dev",
            "--trust-plugins",
            "--body-only",
        ])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(0), "{output:?}");
    let echo: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    let fetched = echo["body"].as_str().unwrap();
    assert!(fetched.contains("from=plugin"), "{fetched}");
    let history = history_text(dir.path());
    assert!(history.contains("plugin:fetch"), "{history}");
    assert!(history.contains("from=plugin"), "{history}");
}

/// "Other host", in a real run: a host the plugin holds no grant for is
/// refused, and nothing is sent to it.
#[test]
fn other_host_in_a_run() {
    let server = Server::http();
    let url = server.url("/echo?from=plugin");
    let dir = fetch_workspace(&server, "auth.example.com", &url);
    let output = rustman(dir.path())
        .args(["send", "fetched", "--env", "dev", "--trust-plugins"])
        .output()
        .unwrap();
    assert_ne!(output.status.code(), Some(0), "{output:?}");
    let stderr = String::from_utf8_lossy(&output.stderr);
    // The fetcher returns the host error's message: the refused host.
    assert!(stderr.contains("plugin fetch: net "), "{stderr}");
    assert!(!history_text(dir.path()).contains("plugin:fetch"));
}

/// `rustman bench` loads the workspace plugins under the same trust rule
/// as `run`: refused (exit 2, nothing sent) while untrusted, and with
/// `--trust-plugins` each iteration resolves the plugin generator.
#[test]
fn bench_loads_the_trusted_plugins() {
    let server = Server::http();
    let dir = workspace(&server);
    let root = dir.path();
    let prefix = unique(root);
    let url = format!("{{{{base_url}}}}/timed/0?id={prefix}-{{{{$faker.email}}}}");
    write(root, "one.rman.yaml", &request("one", &url, ""));
    let caps = ["random"];
    add_plugin(
        root,
        "faker",
        &fixtures::faker(),
        &["generator"],
        &caps,
        &caps,
    );
    let target = server.url("");
    let bench = [
        "bench",
        ".",
        "--env",
        "dev",
        "--target",
        &target,
        "--iterations",
        "1",
    ];
    rustman(root)
        .args(bench)
        .assert()
        .code(2)
        .stderr(predicate::str::contains(UNTRUSTED));
    assert!(timed_hits(&prefix).is_empty(), "a request was sent");

    let output = rustman(root)
        .args(bench)
        .arg("--trust-plugins")
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(0), "{output:?}");
    let hits = timed_hits(&prefix);
    assert_eq!(hits.len(), 1, "{output:?}");
    assert!(hits[0].id.contains("-user-"), "{}", hits[0].id);
}

/// `rustman history replay` signs a plugin-auth request again with the
/// workspace plugins, under the same trust rule as `send`.
#[test]
fn replay_signs_with_the_trusted_plugins() {
    let server = Server::http();
    let grants = ["secrets:read:hmac_key", "clock"];
    let dir = auth_workspace(&server, "hmac", &fixtures::hmac_signer(), &grants);
    let root = dir.path();
    let auth = "auth: {type: plugin, scheme: hmac/v1, key_var: hmac_key}\n";
    write(
        root,
        "signed.rman.yaml",
        &request("signed", "{{base_url}}/echo?a=1", auth),
    );
    rustman(root)
        .args(["send", "signed", "--env", "dev", "--trust-plugins"])
        .assert()
        .code(0);
    let listed = rustman(root)
        .args(["history", "list", "--json"])
        .output()
        .unwrap();
    let rows: serde_json::Value = serde_json::from_slice(&listed.stdout).unwrap();
    let id = rows[0]["id"].as_str().unwrap().to_owned();

    // A new machine: no approval recorded.
    std::fs::remove_file(root.join(".rustman/trust.yaml")).unwrap();
    rustman(root)
        .args(["history", "replay", &id])
        .assert()
        .code(2)
        .stderr(predicate::str::contains(UNTRUSTED));

    let output = rustman(root)
        .args(["history", "replay", &id, "--trust-plugins", "--body-only"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(0), "{output:?}");
    let echo: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    let signed = echo["headers"]
        .as_array()
        .unwrap()
        .iter()
        .any(|h| h[0] == "x-signature");
    assert!(signed, "{echo}");
}

/// A provider value no other test uses.
const PROVIDER_CANARY: &str = "provider-canary-4d2e9b";

/// What the `param_echo` auth plugin passed on (its `p` query parameter)
/// when it signed a request whose `app_key` param is
/// `{{secret:vault/APP_JSON#key}}`, resolved by an `env` provider, under
/// `grants`. The response is written as received (`-o`), so the
/// placeholder or the value is exactly what the plugin saw.
fn param_seen_by_plugin(grants: &[&str]) -> String {
    let server = Server::http();
    let dir = workspace(&server);
    write(
        dir.path(),
        "rustman.toml",
        "[workspace]\nname = \"t\"\nversion = 1\n\n[secret_providers.vault]\ntype = \"env\"\n",
    );
    add_plugin(
        dir.path(),
        "echo",
        &fixtures::param_echo(),
        &["auth"],
        &["secrets:read"],
        grants,
    );
    let auth = "auth: {type: plugin, scheme: echo/v1, app_key: '{{secret:vault/APP_JSON#key}}'}\n";
    write(
        dir.path(),
        "signed.rman.yaml",
        &request("signed", "{{base_url}}/echo", auth),
    );
    let config = tempfile::tempdir().unwrap();
    let output = rustman(dir.path())
        .env("XDG_CONFIG_HOME", config.path())
        .env("APPDATA", config.path())
        .env("APP_JSON", format!(r#"{{"key":"{PROVIDER_CANARY}"}}"#))
        .args(["send", "signed", "--env", "dev", "--trust-plugins"])
        .args(["-o", "out.json"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(0), "{output:?}");
    let body = std::fs::read_to_string(dir.path().join("out.json")).unwrap();
    let echo: serde_json::Value = serde_json::from_str(&body).unwrap();
    let path = echo["path"].as_str().unwrap().to_owned();
    let query = path.split_once("?p=").map(|(_, q)| q.to_owned());
    query.unwrap_or_else(|| panic!("no p parameter: {path}"))
}

/// add-secret-providers task 5.3, "Auth plugin without grant": a value a
/// `{{secret:...}}` reference resolved reaches a plugin as its
/// placeholder unless the plugin holds `secrets:read:<reference>`.
#[test]
fn auth_plugin_without_grant_sees_the_provider_placeholder() {
    let seen = param_seen_by_plugin(&[]);
    assert!(!seen.contains(PROVIDER_CANARY), "{seen}");
    // `«redacted:vault/APP_JSON#key»`, form-encoded.
    assert_eq!(seen, "%C2%ABredacted%3Avault%2FAPP_JSON%23key%C2%BB");
}

#[test]
fn auth_plugin_with_the_reference_grant_sees_the_provider_value() {
    let seen = param_seen_by_plugin(&["secrets:read:vault/APP_JSON#key"]);
    assert_eq!(seen, PROVIDER_CANARY);
}
