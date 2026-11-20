//! `rustman secret get|test|list-providers` (add-secret-providers task 5.1)
//! against a stub Vault: values are masked, `--reveal` needs a terminal,
//! and health checks fail with exit code 3.

mod common;

use std::path::Path;

use assert_cmd::Command;
use common::{rustman, write};
use rustman_test_support::{GraphqlHttpServer, RecordedRequest, Reply};

/// 24 characters, never printed unless revealed on a terminal.
const CANARY: &str = "cli-canary-0123456789abc";
const TOKEN: &str = "hvs.cli-token-canary";

fn vault_stub(req: &RecordedRequest) -> Reply {
    if req.header("x-vault-token") != Some(TOKEN) {
        return Reply::raw(
            403,
            "application/json",
            r#"{"errors":["permission denied"]}"#,
        );
    }
    match req.uri.as_str() {
        "/v1/kv/app" => Reply::raw(
            200,
            "application/json",
            &format!(r#"{{"data":{{"pw":"{CANARY}"}}}}"#),
        ),
        "/v1/auth/token/lookup-self" => Reply::raw(200, "application/json", r#"{"data":{}}"#),
        _ => Reply::raw(404, "application/json", r#"{"errors":[]}"#),
    }
}

/// The stub on a runtime of its own, which the test keeps alive.
struct Stub {
    server: GraphqlHttpServer,
    _runtime: tokio::runtime::Runtime,
}

impl Stub {
    fn start() -> Self {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .enable_all()
            .build()
            .unwrap();
        let server = runtime.block_on(GraphqlHttpServer::start(vault_stub));
        Self {
            server,
            _runtime: runtime,
        }
    }

    fn address(&self) -> String {
        self.server.url("").trim_end_matches('/').to_owned()
    }
}

/// A workspace with provider `vault` (at the stub) and `ci` (`env`). The
/// commands set `VAULT_ADDR` to the stub too: the user's token goes only to
/// an address the user chose (docs/secrets.md, "The user config").
fn workspace(stub: &Stub) -> tempfile::TempDir {
    let ws = tempfile::tempdir().unwrap();
    write(
        ws.path(),
        "rustman.toml",
        &format!(
            "[workspace]\nname = \"t\"\nversion = 1\n\n[secret_providers.vault]\ntype = \"vault\"\naddress = \"{}\"\n\n[secret_providers.ci]\ntype = \"env\"\n",
            stub.address()
        ),
    );
    ws
}

/// `rustman` with an empty user config and `token` as `VAULT_TOKEN`.
fn secret_cmd(ws: &Path, config: &Path, token: &str) -> Command {
    let mut cmd = rustman(ws);
    cmd.env("XDG_CONFIG_HOME", config)
        .env("APPDATA", config)
        .env("HOME", config)
        .env("VAULT_TOKEN", token)
        .env_remove("VAULT_ADDR")
        .arg("secret");
    cmd
}

fn output_of(cmd: &mut Command, code: i32) -> String {
    let out = cmd.assert().code(code).get_output().clone();
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(!text.contains(CANARY), "the value leaked: {text}");
    assert!(!text.contains(TOKEN), "the token leaked: {text}");
    text
}

#[test]
fn masked_by_default() {
    let stub = Stub::start();
    let ws = workspace(&stub);
    let config = tempfile::tempdir().unwrap();
    let out = secret_cmd(ws.path(), config.path(), TOKEN)
        .env("VAULT_ADDR", stub.address())
        .args(["get", "vault/kv/app#pw"])
        .assert()
        .code(0)
        .get_output()
        .clone();
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "•••••••• (24 chars, vault)\n"
    );
    // An unresolved reference is a request error; a malformed one a usage error.
    let text = output_of(
        secret_cmd(ws.path(), config.path(), TOKEN)
            .env("VAULT_ADDR", stub.address())
            .args(["get", "vault/kv/app#user"]),
        3,
    );
    assert!(
        text.contains("error: unresolved secret vault/kv/app#user: field 'user' not found"),
        "{text}"
    );
    let text = output_of(
        secret_cmd(ws.path(), config.path(), TOKEN)
            .env("VAULT_ADDR", stub.address())
            .args(["get", "vault"]),
        2,
    );
    assert!(
        text.contains("invalid secret reference 'secret:vault'"),
        "{text}"
    );
    // Nothing was written anywhere.
    assert!(!ws.path().join(".rustman").exists());
}

#[test]
fn reveal_into_a_pipe_refused() {
    let stub = Stub::start();
    let ws = workspace(&stub);
    let config = tempfile::tempdir().unwrap();
    let text = output_of(
        secret_cmd(ws.path(), config.path(), TOKEN)
            .env("VAULT_ADDR", stub.address())
            .args(["get", "vault/kv/app#pw", "--reveal"]),
        2,
    );
    assert!(
        text.contains("error: --reveal requires an interactive terminal"),
        "{text}"
    );
    assert!(stub.server.hits().is_empty(), "the provider was contacted");
}

#[test]
fn provider_health_check() {
    let stub = Stub::start();
    let ws = workspace(&stub);
    let config = tempfile::tempdir().unwrap();
    let text = output_of(
        secret_cmd(ws.path(), config.path(), "hvs.expired")
            .env("VAULT_ADDR", stub.address())
            .args(["test", "vault"]),
        3,
    );
    assert!(
        text.contains("error: vault: authentication failed"),
        "{text}"
    );
    let text = output_of(
        secret_cmd(ws.path(), config.path(), TOKEN)
            .env("VAULT_ADDR", stub.address())
            .args(["test"]),
        0,
    );
    assert!(
        text.contains("vault: ok") && text.contains("ci: ok"),
        "{text}"
    );
    let text = output_of(
        secret_cmd(ws.path(), config.path(), TOKEN)
            .env("VAULT_ADDR", stub.address())
            .args(["test", "lastpass"]),
        3,
    );
    assert!(
        text.contains("unknown secret provider 'lastpass' (configured: ci, vault)"),
        "{text}"
    );
}

#[test]
fn list_providers_shows_settings_never_values() {
    let stub = Stub::start();
    let ws = workspace(&stub);
    let config = tempfile::tempdir().unwrap();
    write(
        &config.path().join("rustman"),
        "config.toml",
        "[secret_providers.pass]\ntype = \"cmd\"\ncommand = [\"pass\", \"show\"]\n",
    );
    let text = output_of(
        secret_cmd(ws.path(), config.path(), TOKEN)
            .env("VAULT_ADDR", stub.address())
            .arg("list-providers"),
        0,
    );
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(
        lines,
        [
            "NAME   TYPE   AUTH   ORIGIN     STATUS",
            "ci     env    -      workspace  available",
            "pass   cmd    -      user       available",
            "vault  vault  token  workspace  available",
        ]
    );
    assert!(
        stub.server.hits().is_empty(),
        "listing contacted a provider"
    );
    // --types needs no workspace.
    let empty = tempfile::tempdir().unwrap();
    let out = secret_cmd(empty.path(), config.path(), TOKEN)
        .args(["list-providers", "--types"])
        .assert()
        .code(0)
        .get_output()
        .clone();
    let types = String::from_utf8_lossy(&out.stdout);
    let types: Vec<&str> = types.lines().collect();
    assert_eq!(types, rustman_secrets::built_types());
    assert!(types.starts_with(&["keychain", "env", "sops", "onepassword", "vault", "cmd"]));
}

/// `--reveal` on a terminal prints the value (the one place it does).
#[cfg(unix)]
#[test]
fn reveal_on_a_terminal() {
    use std::io::Read;

    use portable_pty::{CommandBuilder, PtySize, native_pty_system};

    let stub = Stub::start();
    let ws = workspace(&stub);
    let config = tempfile::tempdir().unwrap();
    let mut cmd = CommandBuilder::new(env!("CARGO_BIN_EXE_rustman"));
    cmd.args(["secret", "get", "vault/kv/app#pw", "--reveal"]);
    cmd.cwd(ws.path());
    cmd.env("XDG_CONFIG_HOME", config.path());
    cmd.env("HOME", config.path());
    cmd.env("VAULT_TOKEN", TOKEN);
    cmd.env("VAULT_ADDR", stub.address());
    cmd.env("RUSTMAN_SECRET_STORE", "file");
    let pty = native_pty_system()
        .openpty(PtySize {
            rows: 24,
            cols: 80,
            pixel_width: 0,
            pixel_height: 0,
        })
        .unwrap();
    let mut child = pty.slave.spawn_command(cmd).unwrap();
    drop(pty.slave);
    let mut reader = pty.master.try_clone_reader().unwrap();
    let reading = std::thread::spawn(move || {
        let mut seen = Vec::new();
        let _ = reader.read_to_end(&mut seen);
        seen
    });
    let status = child.wait().unwrap();
    drop(pty.master);
    let seen = String::from_utf8_lossy(&reading.join().unwrap()).into_owned();
    assert!(status.success(), "{seen}");
    assert!(seen.contains(CANARY), "{seen}");
}

/// A cloned workspace names the Vault address: without `VAULT_ADDR` (or a
/// user-config `address`) naming it too, the user's token is never sent.
#[test]
fn a_workspace_vault_address_never_receives_the_token() {
    let stub = Stub::start();
    let ws = workspace(&stub);
    let config = tempfile::tempdir().unwrap();
    // An unresolved reference is exit code 3, as in `send`; `secret test`
    // exits 0 or 3 (the spec), whatever made the check fail.
    for args in [["get", "vault/kv/app#pw"], ["test", "vault"]] {
        let text = output_of(secret_cmd(ws.path(), config.path(), TOKEN).args(args), 3);
        assert!(
            text.contains("is set in rustman.toml; set VAULT_ADDR to it"),
            "{text}"
        );
    }
    assert!(stub.server.hits().is_empty(), "the token was sent");
}
