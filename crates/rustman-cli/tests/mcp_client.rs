//! `rustman mcp-client` (add-mcp-client task 8.1): workspace servers,
//! `stdio:` command lines and URLs, text and `--json` output, exit codes,
//! stdio trust, and `logout`. The HTTP server is the in-process fake; the
//! stdio server is `rustman mcp serve` itself. Nothing leaves loopback.

mod common;

use std::path::Path;

use common::{rustman, write};
use predicates::prelude::*;
use rustman_test_support::McpHttpServer;
use serde_json::Value;

/// The fake server and the runtime it is served from (dropped after it).
struct Served {
    server: McpHttpServer,
    _runtime: tokio::runtime::Runtime,
}

fn serve() -> Served {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();
    let server = runtime.block_on(McpHttpServer::streamable());
    Served {
        server,
        _runtime: runtime,
    }
}

/// A workspace whose server `remote` is `url` and whose server `files` is
/// `rustman mcp serve` (with `args`).
fn workspace(url: &str, args: &str) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let bin = assert_cmd::cargo::cargo_bin("rustman");
    write(
        dir.path(),
        "rustman.toml",
        &format!(
            "[workspace]\nname = \"t\"\nversion = 1\n\n[mcp_servers.remote]\nurl = '{url}'\n\n[mcp_servers.files]\ncommand = '{}'\nargs = [{args}]\n",
            bin.display()
        ),
    );
    write(dir.path(), ".gitignore", ".rustman/\n");
    dir
}

fn json(output: &[u8]) -> Value {
    serde_json::from_slice(output)
        .unwrap_or_else(|e| panic!("{e}: {}", String::from_utf8_lossy(output)))
}

#[test]
fn tools_of_an_ad_hoc_url_print_as_json() {
    let served = serve();
    let dir = tempfile::tempdir().unwrap();
    let output = rustman(dir.path())
        .args(["mcp-client", "tools", &served.server.url(), "--json"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let tools = json(&output);
    assert_eq!(tools["tools"][0]["name"], "echo");
    assert_eq!(tools["tools"][1]["name"], "fail");
}

#[test]
fn tools_of_a_workspace_server_print_as_text() {
    let served = serve();
    let dir = workspace(&served.server.url(), "'mcp', 'serve'");
    rustman(dir.path())
        .args(["mcp-client", "tools", "remote"])
        .assert()
        .success()
        .stdout("echo\n    Echo the text back\nfail\n");
}

#[test]
fn a_tool_call_prints_its_content() {
    let served = serve();
    let dir = workspace(&served.server.url(), "'mcp', 'serve'");
    rustman(dir.path())
        .args([
            "mcp-client",
            "call",
            "remote",
            "echo",
            "--arg",
            "text=hello",
        ])
        .assert()
        .success()
        .stdout("hello\n");
    let output = rustman(dir.path())
        .args([
            "mcp-client",
            "call",
            "remote",
            "echo",
            "--args",
            r#"{"text": "hi"}"#,
            "--json",
        ])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    assert_eq!(json(&output)["content"][0]["text"], "hi");
}

#[test]
fn a_tool_error_prints_the_content_and_exits_1() {
    let served = serve();
    let dir = workspace(&served.server.url(), "'mcp', 'serve'");
    rustman(dir.path())
        .args([
            "mcp-client",
            "call",
            "remote",
            "fail",
            "--arg",
            "path=missing",
        ])
        .assert()
        .code(1)
        .stdout("tool error\nno such file\n");
}

#[test]
fn a_protocol_error_exits_1() {
    let served = serve();
    let dir = tempfile::tempdir().unwrap();
    rustman(dir.path())
        .args([
            "mcp-client",
            "call",
            &served.server.url(),
            "nope",
            "--no-validate",
        ])
        .assert()
        .code(1)
        .stderr("error: mcp: protocol error -32602: unknown tool nope\n");
}

#[test]
fn escape_sequences_from_the_server_are_shown_escaped() {
    let served = serve();
    let dir = workspace(&served.server.url(), "'mcp', 'serve'");
    rustman(dir.path())
        .args([
            "mcp-client",
            "call",
            "remote",
            "echo",
            "--arg",
            "text=\u{1b}[31mred\u{7}",
        ])
        .assert()
        .success()
        .stdout("\\u{1b}[31mred\\u{7}\n");
    rustman(dir.path())
        .args([
            "mcp-client",
            "call",
            &served.server.url(),
            "\u{1b}]0;owned\u{7}",
            "--no-validate",
        ])
        .assert()
        .code(1)
        .stderr("error: mcp: protocol error -32602: unknown tool \\u{1b}]0;owned\\u{7}\n");
}

#[test]
fn resources_are_listed_and_read() {
    let served = serve();
    let dir = workspace(&served.server.url(), "'mcp', 'serve'");
    rustman(dir.path())
        .args(["mcp-client", "resources", "remote"])
        .assert()
        .success()
        .stdout("file:///notes.md  notes  text/markdown\n");
    rustman(dir.path())
        .args([
            "mcp-client",
            "resources",
            "remote",
            "--read",
            "file:///notes.md",
        ])
        .assert()
        .success()
        .stdout("# Notes\n");
}

#[test]
fn prompts_are_listed_and_got() {
    let served = serve();
    let dir = workspace(&served.server.url(), "'mcp', 'serve'");
    rustman(dir.path())
        .args(["mcp-client", "prompts", "remote"])
        .assert()
        .success()
        .stdout("review  (code*)\n");
    rustman(dir.path())
        .args([
            "mcp-client",
            "prompts",
            "remote",
            "--get",
            "review",
            "--arg",
            "code=x = 1",
        ])
        .assert()
        .success()
        .stdout("user: Review x = 1\n");
}

#[test]
fn a_changed_stdio_server_needs_trust_again() {
    let dir = workspace("http://127.0.0.1:9/mcp", "'mcp', 'serve'");
    rustman(dir.path())
        .args(["mcp-client", "tools", "files"])
        .assert()
        .code(2)
        .stderr("error: mcp: server files is not trusted; review it or pass --trust\n");
    rustman(dir.path())
        .args(["mcp-client", "tools", "files", "--trust"])
        .assert()
        .success()
        .stdout(predicate::str::contains("list_requests"));
    rustman(dir.path())
        .args(["mcp-client", "tools", "files"])
        .assert()
        .success();
    let changed = workspace(
        "http://127.0.0.1:9/mcp",
        "'mcp', 'serve', '--workspace', '.'",
    );
    std::fs::copy(
        changed.path().join("rustman.toml"),
        dir.path().join("rustman.toml"),
    )
    .unwrap();
    rustman(dir.path())
        .args(["mcp-client", "tools", "files"])
        .assert()
        .code(2)
        .stderr("error: mcp: server files is not trusted; review it or pass --trust\n");
}

fn git(dir: &Path, args: &[&str]) {
    let ok = std::process::Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["-c", "user.name=t", "-c", "user.email=t@example.com"])
        .args(args)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .expect("git is installed")
        .success();
    assert!(ok, "git {args:?} failed");
}

/// A repository cannot ship approvals: a trust file Git tracks is refused,
/// so the server it approves is never started.
#[test]
fn a_committed_trust_file_is_refused() {
    let dir = workspace("http://127.0.0.1:9/mcp", "'mcp', 'serve'");
    git(dir.path(), &["init", "-q"]);
    rustman(dir.path())
        .args(["mcp-client", "tools", "files", "--trust"])
        .assert()
        .success();
    git(
        dir.path(),
        &[
            "add",
            "-f",
            ".gitignore",
            "rustman.toml",
            ".rustman/trust.yaml",
        ],
    );
    git(dir.path(), &["commit", "-q", "-m", "ship trust"]);
    rustman(dir.path())
        .args(["mcp-client", "tools", "files"])
        .assert()
        .code(2)
        .stdout("")
        .stderr(
            "error: refusing .rustman/trust.yaml: Git tracks it or does not ignore it; \
             delete it and approve the servers again\n",
        );
}

/// A trust file that comes with a workspace outside any Git work tree (a
/// downloaded archive) approves nothing: it was sealed by another machine,
/// or for another workspace, so the server is refused and never started.
#[test]
fn a_shipped_trust_file_outside_git_approves_nothing() {
    let author = workspace("http://127.0.0.1:9/mcp", "'mcp', 'serve'");
    rustman(author.path())
        .args(["mcp-client", "tools", "files", "--trust"])
        .assert()
        .success();
    let shipped = |data_dir: &Path| {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join(".rustman")).unwrap();
        for rel in ["rustman.toml", ".gitignore", ".rustman/trust.yaml"] {
            std::fs::copy(author.path().join(rel), dir.path().join(rel)).unwrap();
        }
        rustman(dir.path())
            .env("RUSTMAN_DATA_DIR", data_dir)
            .args(["mcp-client", "tools", "files"])
            .assert()
            .code(2)
            .stdout("")
            .stderr("error: mcp: server files is not trusted; review it or pass --trust\n");
    };
    // Sealed on another machine (another key).
    let elsewhere = tempfile::tempdir().unwrap();
    shipped(elsewhere.path());
    // Sealed on this machine, for another workspace.
    shipped(&common::test_data_dir());
}

#[test]
fn an_ad_hoc_stdio_server_is_the_users_own_command() {
    let dir = workspace("http://127.0.0.1:9/mcp", "'mcp', 'serve'");
    let bin = assert_cmd::cargo::cargo_bin("rustman");
    let target = format!("stdio:{} mcp serve", shell_quote(&bin));
    rustman(dir.path())
        .args(["mcp-client", "tools", &target, "--json"])
        .assert()
        .success()
        .stdout(predicate::str::contains("\"list_requests\""));
    assert!(
        !dir.path().join(".rustman/trust.yaml").exists(),
        "nothing recorded"
    );
}

#[test]
fn unknown_servers_and_bad_arguments_are_usage_errors() {
    let dir = workspace("http://127.0.0.1:9/mcp", "'mcp', 'serve'");
    rustman(dir.path())
        .args(["mcp-client", "tools", "nope"])
        .assert()
        .code(2)
        .stderr("error: mcp: unknown server nope\n");
    rustman(dir.path())
        .args(["mcp-client", "call", "remote", "echo", "--arg", "novalue"])
        .assert()
        .code(2)
        .stderr("error: --arg novalue: expected K=V\n");
}

#[test]
fn logout_without_tokens_says_so() {
    let dir = workspace("http://127.0.0.1:9/mcp", "'mcp', 'serve'");
    rustman(dir.path())
        .args(["mcp-client", "logout", "remote"])
        .assert()
        .success()
        .stdout("No stored tokens for remote\n");
}

/// `path` quoted for a `stdio:` command line (Windows paths have spaces
/// and backslashes).
fn shell_quote(path: &Path) -> String {
    format!("'{}'", path.display())
}

const SECRET: &str = "s3cr3t-mcp-cli-canary";

/// [`workspace`] with a `dev` environment whose secret `api_token` is
/// [`SECRET`], sent in the URL of `remote` and in the request `echo`.
fn secret_workspace(url: &str) -> tempfile::TempDir {
    let dir = workspace(&format!("{url}?key={{{{api_token}}}}"), "'mcp', 'serve'");
    write(
        dir.path(),
        "environments/dev.yaml",
        "secrets: [api_token]\n",
    );
    write(
        dir.path(),
        "echo.rman.yaml",
        "version: 1\nkind: mcp\nname: Echo\nserver: remote\noperation: tools/call\nparams:\n  name: echo\n  arguments: {text: 'token {{api_token}}'}\nassertions:\n- content_text_contains: nothing\n",
    );
    rustman(dir.path())
        .args(["secret", "set", "api_token", "--env", "dev"])
        .write_stdin(SECRET)
        .assert()
        .success();
    dir
}

#[test]
fn secrets_never_reach_the_output() {
    let served = serve();
    let dir = secret_workspace(&served.server.url());
    let assert = rustman(dir.path())
        .args([
            "-vvv",
            "mcp-client",
            "tools",
            "remote",
            "--env",
            "dev",
            "--json",
        ])
        .assert()
        .success();
    let output = assert.get_output();
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(!text.contains(SECRET), "{text}");
    let assert = rustman(dir.path())
        .args(["-vv", "run", "echo.rman.yaml", "--env", "dev"])
        .assert()
        .code(1);
    let output = assert.get_output();
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(text.contains("content_text_contains"), "{text}");
    assert!(text.contains("«redacted:api_token»"), "{text}");
    assert!(!text.contains(SECRET), "{text}");
    let history = std::fs::read_dir(dir.path().join(".rustman/history")).unwrap();
    for entry in history {
        let text = std::fs::read_to_string(entry.unwrap().path()).unwrap();
        assert!(!text.contains(SECRET), "{text}");
    }
}

#[test]
fn an_unreachable_server_exits_3_without_its_secrets() {
    let port = common::closed_port();
    let dir = secret_workspace(&format!("http://127.0.0.1:{port}/mcp"));
    let assert = rustman(dir.path())
        .args(["mcp-client", "tools", "remote", "--env", "dev"])
        .assert()
        .code(3);
    let stderr = String::from_utf8_lossy(&assert.get_output().stderr).into_owned();
    assert!(
        stderr.starts_with("error: mcp: cannot connect to server remote"),
        "{stderr}"
    );
    assert!(!stderr.contains(SECRET), "{stderr}");
}

/// `--timeout` also bounds `initialize`: a server that never answers fails
/// after it, not after the 60 s default.
#[cfg(unix)]
#[test]
fn timeout_bounds_a_server_that_never_initializes() {
    let dir = tempfile::tempdir().unwrap();
    let started = std::time::Instant::now();
    rustman(dir.path())
        .args([
            "mcp-client",
            "tools",
            "stdio:sh -c 'cat > /dev/null; exit 0'",
            "--timeout",
            "500ms",
        ])
        .timeout(std::time::Duration::from_secs(30))
        .assert()
        .code(3)
        .stderr("error: mcp: request timed out after 500ms\n");
    assert!(started.elapsed() < std::time::Duration::from_secs(20));
}

/// The size of `path` once it has grown past a few lines.
#[cfg(unix)]
fn grown(path: &Path, child: &mut std::process::Child) -> u64 {
    use std::time::{Duration, Instant};
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        let size = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
        if size > 8 {
            return size;
        }
        assert!(Instant::now() < deadline, "the server never started");
        assert!(child.try_wait().unwrap().is_none(), "rustman ended early");
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// Ctrl+C stops the server with everything it started: a launcher's
/// background process stops writing its heartbeat once rustman exits.
#[cfg(unix)]
#[test]
fn ctrl_c_exits_130_and_stops_the_server_tree() {
    use std::process::{Command, Stdio};
    use std::time::Duration;

    let dir = tempfile::tempdir().unwrap();
    let heartbeat = dir.path().join("heartbeat");
    // Never answers `initialize`; its loop runs in a process of its own.
    let target = format!(
        "stdio:sh -c 'while :; do echo beat >> \"{}\"; sleep 0.1; done & wait'",
        heartbeat.display()
    );
    let mut child = Command::new(assert_cmd::cargo::cargo_bin("rustman"))
        .args(["mcp-client", "call", &target, "hang"])
        .current_dir(dir.path())
        .env("RUSTMAN_SECRET_STORE", "file")
        .env("RUSTMAN_DATA_DIR", common::test_data_dir())
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    grown(&heartbeat, &mut child);
    let killed = Command::new("kill")
        .args(["-INT", &child.id().to_string()])
        .status()
        .unwrap();
    assert!(killed.success());
    let output = child.wait_with_output().unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(output.status.code(), Some(130), "{stderr}");
    assert!(stderr.contains("interrupted"), "{stderr}");
    // Generous margin: a surviving loop beats ten times a second.
    std::thread::sleep(Duration::from_millis(500));
    let stopped = std::fs::metadata(&heartbeat).unwrap().len();
    std::thread::sleep(Duration::from_millis(1500));
    assert_eq!(
        std::fs::metadata(&heartbeat).unwrap().len(),
        stopped,
        "the server's background process still runs"
    );
}

/// A stdio server, as a shell script: it starts a background loop beating
/// into its first argument, answers `initialize`, then never answers again
/// and ignores the end of its stdin (so stopping it takes the whole grace
/// period, then a kill).
#[cfg(unix)]
const STUBBORN_SERVER: &str = r#"( while :; do echo beat >> "$1"; sleep 0.1; done ) &
read -r line
id=$(printf '%s' "$line" | sed -n 's/.*"id":\([0-9][0-9]*\).*/\1/p')
printf '{"jsonrpc":"2.0","id":%s,"result":{"protocolVersion":"2025-06-18","capabilities":{"tools":{}},"serverInfo":{"name":"sh","version":"1"}}}\n' "$id"
exec sleep 300
"#;

/// `rustman mcp-client call <stubborn server> hang`, once the server's
/// background loop beats; with the heartbeat path.
#[cfg(unix)]
fn stubborn_call(dir: &Path) -> (std::process::Child, std::path::PathBuf) {
    use std::process::{Command, Stdio};

    let script = dir.join("server.sh");
    let heartbeat = dir.join("heartbeat");
    std::fs::write(&script, STUBBORN_SERVER).unwrap();
    let target = format!(
        "stdio:sh {} {}",
        shell_quote(&script),
        shell_quote(&heartbeat)
    );
    let mut child = Command::new(assert_cmd::cargo::cargo_bin("rustman"))
        .args(["mcp-client", "call", &target, "hang"])
        .current_dir(dir)
        .env("RUSTMAN_SECRET_STORE", "file")
        .env("RUSTMAN_DATA_DIR", common::test_data_dir())
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    grown(&heartbeat, &mut child);
    // Let the session initialize and send its request.
    std::thread::sleep(std::time::Duration::from_millis(500));
    (child, heartbeat)
}

#[cfg(unix)]
fn signal(child: &std::process::Child, name: &str) {
    let sent = std::process::Command::new("kill")
        .args([&format!("-{name}"), &child.id().to_string()])
        .status()
        .unwrap();
    assert!(sent.success(), "kill -{name}");
}

/// Waits for `child`, failing after `limit`; its exit code and stderr.
#[cfg(unix)]
fn finish(mut child: std::process::Child, limit: std::time::Duration) -> (Option<i32>, String) {
    use std::io::Read;
    let deadline = std::time::Instant::now() + limit;
    while child.try_wait().unwrap().is_none() {
        if std::time::Instant::now() > deadline {
            let _ = child.kill();
            panic!("rustman did not exit within {limit:?}");
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    let mut stderr = String::new();
    child
        .stderr
        .take()
        .unwrap()
        .read_to_string(&mut stderr)
        .unwrap();
    (child.wait().unwrap().code(), stderr)
}

/// Fails when `heartbeat` still grows: a surviving loop beats ten times a
/// second.
#[cfg(unix)]
fn assert_stopped(heartbeat: &Path) {
    use std::time::Duration;
    std::thread::sleep(Duration::from_millis(500));
    let stopped = std::fs::metadata(heartbeat).unwrap().len();
    std::thread::sleep(Duration::from_millis(1500));
    assert_eq!(
        std::fs::metadata(heartbeat).unwrap().len(),
        stopped,
        "the server's background process still runs"
    );
}

/// A second Ctrl+C during the shutdown grace period quits at once, and
/// still stops the server with everything it started.
#[cfg(unix)]
#[test]
fn a_second_ctrl_c_quits_at_once_and_stops_the_server_tree() {
    use std::time::Duration;

    let dir = tempfile::tempdir().unwrap();
    let (child, heartbeat) = stubborn_call(dir.path());
    signal(&child, "INT");
    std::thread::sleep(Duration::from_millis(500));
    signal(&child, "INT");
    let (code, stderr) = finish(child, Duration::from_secs(3));
    assert_eq!(code, Some(130), "{stderr}");
    assert!(stderr.contains("interrupted again: quitting"), "{stderr}");
    assert_stopped(&heartbeat);
}

/// SIGTERM (as SIGHUP) is a Ctrl+C: the call is cancelled and the server
/// stopped with everything it started once its grace period is over.
#[cfg(unix)]
#[test]
fn sigterm_cancels_and_stops_the_server_tree() {
    use std::time::Duration;

    let dir = tempfile::tempdir().unwrap();
    let (child, heartbeat) = stubborn_call(dir.path());
    signal(&child, "TERM");
    let (code, stderr) = finish(child, Duration::from_secs(20));
    assert_eq!(code, Some(130), "{stderr}");
    assert!(stderr.contains("interrupted"), "{stderr}");
    assert_stopped(&heartbeat);
}
