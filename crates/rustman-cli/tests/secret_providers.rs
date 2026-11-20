//! Secret providers through `rustman send` and `rustman run`
//! (add-secret-providers sections 1-3): values reach the wire only, are
//! redacted in history and reports when a server echoes them, are never
//! written to disk, and unresolved references stop the request.

mod common;

use std::fs;
use std::path::Path;

use assert_cmd::Command;
use common::{Server, request, rustman, workspace, write};
use rustman_test_support::TestServer;
use rustman_test_support::routes::timed_hits;

/// A value no other test uses, so finding it anywhere is a leak.
const CANARY: &str = "canary-9f3c1e-provider-secret";

/// `rustman` with its user config directory in `config` and the provider
/// values in its environment.
fn rustman_with(dir: &Path, config: &Path) -> Command {
    let mut cmd = rustman(dir);
    cmd.env("XDG_CONFIG_HOME", config)
        .env("APPDATA", config)
        .env(
            "DB_JSON",
            format!(r#"{{"password":"{CANARY}","port":5432}}"#),
        )
        .env("RUSTMAN_TEST_CANARY", CANARY);
    cmd
}

/// A workspace with `[secret_providers.aws]` and `[secret_providers.ci]`,
/// both of type `env`.
fn provider_workspace(server: &Server, extra: &str) -> tempfile::TempDir {
    let ws = workspace(server);
    write(
        ws.path(),
        "rustman.toml",
        &format!(
            "[workspace]\nname = \"t\"\nversion = 1\n\n[secret_providers.aws]\ntype = \"env\"\n\n[secret_providers.ci]\ntype = \"env\"\n{extra}"
        ),
    );
    ws
}

/// Every file under `dir` that contains `needle`.
fn files_containing(dir: &Path, needle: &[u8]) -> Vec<String> {
    let mut found = Vec::new();
    let Ok(entries) = fs::read_dir(dir) else {
        return found;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            found.extend(files_containing(&path, needle));
        } else if let Ok(bytes) = fs::read(&path)
            && bytes.windows(needle.len()).any(|w| w == needle)
        {
            found.push(path.display().to_string());
        }
    }
    found
}

fn history_text(ws: &Path) -> String {
    let mut text = String::new();
    for entry in fs::read_dir(ws.join(".rustman/history")).unwrap().flatten() {
        if entry.path().extension().is_some_and(|x| x == "json") {
            text.push_str(&fs::read_to_string(entry.path()).unwrap());
        }
    }
    text
}

#[test]
fn echoed_provider_values_are_redacted_and_never_written() {
    let server = Server::http();
    let ws = provider_workspace(&server, "");
    let config = tempfile::tempdir().unwrap();
    let headers = "method: POST\nheaders:\n  - name: X-Db\n    value: '{{secret:aws/DB_JSON#password}}'\nbody:\n  type: text\n  content: 'pw={{secret:aws/DB_JSON#password}}'\n";
    write(
        ws.path(),
        "echo.rman.yaml",
        &request("echo", "{{base_url}}/echo", headers),
    );
    let out = rustman_with(ws.path(), config.path())
        .args(["run", "echo.rman.yaml", "--env", "dev"])
        .args(["--reporter", "json:out/report.json"])
        .assert()
        .success()
        .get_output()
        .clone();
    let label = "«redacted:aws/DB_JSON#password»";
    let report = fs::read_to_string(ws.path().join("out/report.json")).unwrap();
    assert!(!report.contains(CANARY), "{report}");
    assert!(report.contains(label), "{report}");
    let history = history_text(ws.path());
    assert!(!history.contains(CANARY), "{history}");
    assert!(history.contains(label), "{history}");
    let terminal = String::from_utf8_lossy(&out.stdout);
    assert!(!terminal.contains(CANARY), "{terminal}");
    assert!(!String::from_utf8_lossy(&out.stderr).contains(CANARY));

    // `send` prints the echoed body redacted too.
    let out = rustman_with(ws.path(), config.path())
        .args(["send", "echo", "--env", "dev"])
        .assert()
        .success()
        .get_output()
        .clone();
    let printed = String::from_utf8_lossy(&out.stdout);
    assert!(!printed.contains(CANARY), "{printed}");
    assert!(printed.contains(label), "{printed}");

    // Nothing on disk: the workspace (with .rustman/) and the config dir.
    for dir in [ws.path(), config.path()] {
        let leaks = files_containing(dir, CANARY.as_bytes());
        assert!(leaks.is_empty(), "the value was written to {leaks:?}");
    }
    let request_file = fs::read_to_string(ws.path().join("echo.rman.yaml")).unwrap();
    assert!(request_file.contains("{{secret:aws/DB_JSON#password}}"));
}

#[test]
fn provider_backed_environment_variables_resolve_and_keep_the_file() {
    let server = Server::http();
    let ws = provider_workspace(&server, "");
    let config = tempfile::tempdir().unwrap();
    let env_text = format!(
        "variables:\n  base_url: {}\n  api_token: {{secret: \"ci/RUSTMAN_TEST_CANARY\"}}\n",
        server.url("")
    );
    write(ws.path(), "environments/dev.yaml", &env_text);
    let headers = "headers:\n  - name: X-Token\n    value: '{{api_token}}'\n";
    write(
        ws.path(),
        "echo.rman.yaml",
        &request("echo", "{{base_url}}/echo", headers),
    );
    let out = rustman_with(ws.path(), config.path())
        .args(["send", "echo", "--env", "dev"])
        .assert()
        .success()
        .get_output()
        .clone();
    let printed = String::from_utf8_lossy(&out.stdout);
    assert!(printed.contains("«redacted:api_token»"), "{printed}");
    assert!(!printed.contains(CANARY), "{printed}");
    let after = fs::read_to_string(ws.path().join("environments/dev.yaml")).unwrap();
    assert_eq!(after, env_text);
}

#[test]
fn a_missing_field_stops_the_request_with_exit_code_3() {
    let server = Server::http();
    let ws = provider_workspace(&server, "");
    let config = tempfile::tempdir().unwrap();
    let id = common::unique(ws.path());
    let url = format!("{{{{base_url}}}}/timed/0?id={id}&user={{{{secret:aws/DB_JSON#user}}}}");
    write(ws.path(), "db.rman.yaml", &request("db", &url, ""));
    let out = rustman_with(ws.path(), config.path())
        .args(["run", "db.rman.yaml", "--env", "dev"])
        .assert()
        .code(3)
        .get_output()
        .clone();
    let all = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        all.contains("unresolved secret aws/DB_JSON#user: field 'user' not found"),
        "{all}"
    );
    assert!(!all.contains(CANARY), "{all}");
    assert!(timed_hits(&id).is_empty(), "the request was sent");
}

#[test]
fn unknown_and_compiled_out_providers_are_request_errors() {
    let server = Server::http();
    let extra =
        "\n[secret_providers.cloud]\ntype = \"aws-secrets-manager\"\nregion = \"eu-west-1\"\n";
    let ws = provider_workspace(&server, extra);
    let config = tempfile::tempdir().unwrap();
    write(
        ws.path(),
        "lp.rman.yaml",
        &request("lp", "{{base_url}}/get?k={{secret:lastpass/x}}", ""),
    );
    write(
        ws.path(),
        "cloud.rman.yaml",
        &request("cloud", "{{base_url}}/get?k={{secret:cloud/prod/db}}", ""),
    );
    let out = rustman_with(ws.path(), config.path())
        .args(["send", "lp", "--env", "dev"])
        .assert()
        .code(3)
        .get_output()
        .clone();
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("unknown secret provider 'lastpass' (configured: aws, ci, cloud)"),
        "{stderr}"
    );
    // `secret get` gives the same exit code as `send` for the same failure.
    let out = rustman_with(ws.path(), config.path())
        .args(["secret", "get", "lastpass/x"])
        .assert()
        .code(3)
        .get_output()
        .clone();
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("unknown secret provider 'lastpass'"),
        "{stderr}"
    );
    if rustman_secrets::is_built("secrets-aws") {
        // "Cloud type in a minimal build" needs a build without it.
        return;
    }
    let not_built = "secret provider type 'aws-secrets-manager' is not available in this build (feature secrets-aws)";
    for args in [
        &["send", "cloud", "--env", "dev"][..],
        &["secret", "get", "cloud/prod/db"][..],
        &["secret", "test", "cloud"][..],
    ] {
        let out = rustman_with(ws.path(), config.path())
            .args(args)
            .assert()
            .code(3)
            .get_output()
            .clone();
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(stderr.contains(not_built), "{args:?}: {stderr}");
    }
}

#[test]
fn a_slow_provider_times_out_as_unresolved() {
    let server = Server::http();
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(1)
        .enable_all()
        .build()
        .unwrap();
    let stalled = runtime.block_on(TestServer::stalled("http"));
    let address = stalled.url("").trim_end_matches('/').to_owned();
    let extra = format!(
        "\n[secret_providers.vault]\ntype = \"vault\"\naddress = \"{address}\"\ntimeout_ms = 300\n"
    );
    let ws = provider_workspace(&server, &extra);
    let config = tempfile::tempdir().unwrap();
    write(
        ws.path(),
        "slow.rman.yaml",
        &request("slow", "{{base_url}}/get?k={{secret:vault/kv/app#pw}}", ""),
    );
    let out = rustman_with(ws.path(), config.path())
        .env("VAULT_TOKEN", "hvs.test-token")
        .env("VAULT_ADDR", &address)
        .args(["run", "slow.rman.yaml", "--env", "dev"])
        .assert()
        .code(3)
        .get_output()
        .clone();
    let all = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        all.contains("unresolved secret vault/kv/app#pw: vault: timed out after 300 ms"),
        "{all}"
    );
    assert!(!all.contains("hvs.test-token"), "{all}");
}

#[test]
fn workspace_credentials_and_commands_are_configuration_errors() {
    let server = Server::http();
    let config = tempfile::tempdir().unwrap();
    let cases = [
        (
            "[secret_providers.vault]\ntype = \"vault\"\ntoken = \"hvs.abc\"\n",
            "credentials must not be stored in rustman.toml (token)",
        ),
        (
            "[secret_providers.x]\ntype = \"cmd\"\ncommand = [\"pass\", \"show\"]\n",
            "cmd secret providers can only be defined in user config",
        ),
    ];
    for (table, message) in cases {
        let ws = workspace(&server);
        write(
            ws.path(),
            "rustman.toml",
            &format!("[workspace]\nname = \"t\"\nversion = 1\n\n{table}"),
        );
        write(
            ws.path(),
            "a.rman.yaml",
            &request("a", "{{base_url}}/get", ""),
        );
        let out = rustman_with(ws.path(), config.path())
            .args(["send", "a", "--env", "dev"])
            .assert()
            .code(2)
            .get_output()
            .clone();
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(stderr.contains(message), "{stderr}");
        assert!(!stderr.contains("hvs.abc"), "{stderr}");
    }
}

#[test]
fn user_config_defines_command_providers() {
    let server = Server::http();
    let ws = workspace(&server);
    let config = tempfile::tempdir().unwrap();
    let command = echo_script(config.path());
    write(
        &config.path().join("rustman"),
        "config.toml",
        &format!("[secret_providers.pass]\ntype = \"cmd\"\ncommand = {command}\n"),
    );
    write(
        ws.path(),
        "echo.rman.yaml",
        &request(
            "echo",
            "{{base_url}}/echo",
            "headers:\n  - name: X-Token\n    value: '{{secret:pass/work/api}}'\n",
        ),
    );
    let out = rustman_with(ws.path(), config.path())
        .args(["send", "echo", "--env", "dev"])
        .assert()
        .success()
        .get_output()
        .clone();
    let printed = String::from_utf8_lossy(&out.stdout);
    assert!(printed.contains("«redacted:pass/work/api»"), "{printed}");
    assert!(!printed.contains("cmd-value-for-work/api"), "{printed}");
}

#[test]
fn exports_show_references_redacted_without_fetching() {
    let server = Server::http();
    let ws = workspace(&server);
    let config = tempfile::tempdir().unwrap();
    let hits = config.path().join("hits");
    let command = recording_script(config.path(), &hits);
    write(
        &config.path().join("rustman"),
        "config.toml",
        &format!("[secret_providers.pass]\ntype = \"cmd\"\ncommand = {command}\n"),
    );
    let env_text = format!(
        "variables:\n  base_url: {}\n  api_token: {{secret: \"pass/work/token\"}}\n",
        server.url("")
    );
    write(ws.path(), "environments/dev.yaml", &env_text);
    let headers = "headers:\n  - name: X-Token\n    value: '{{secret:pass/work/api#key}}'\n  - name: X-Api\n    value: '{{api_token}}'\n";
    write(
        ws.path(),
        "echo.rman.yaml",
        &request("echo", "{{base_url}}/echo", headers),
    );
    for args in [
        &["export", "curl", "echo", "--env", "dev"][..],
        &[
            "export", "snippet", "--lang", "python", "echo", "--env", "dev",
        ][..],
    ] {
        let out = rustman_with(ws.path(), config.path())
            .args(args)
            .assert()
            .success()
            .get_output()
            .clone();
        let printed = String::from_utf8_lossy(&out.stdout);
        assert!(
            printed.contains("«redacted:pass/work/api#key»"),
            "{args:?}: {printed}"
        );
        assert!(
            printed.contains("«redacted:api_token»"),
            "{args:?}: {printed}"
        );
        assert!(!printed.contains("recorded-value"), "{args:?}: {printed}");
    }
    assert!(!hits.exists(), "an export contacted the provider");
}

/// Write a script that appends to `hits` and prints `recorded-value`;
/// returns the TOML argv that runs it.
fn recording_script(dir: &Path, hits: &Path) -> String {
    let (name, text, shell) = if cfg!(windows) {
        (
            "record.cmd",
            format!(
                "@echo off\r\necho hit>> \"{}\"\r\necho recorded-value\r\n",
                hits.display()
            ),
            None,
        )
    } else {
        (
            "record.sh",
            format!(
                "#!/bin/sh\necho hit >> '{}'\necho recorded-value\n",
                hits.display()
            ),
            Some("sh"),
        )
    };
    let path = dir.join(name);
    fs::write(&path, text).unwrap();
    let path = format!("'{}'", path.display());
    match shell {
        Some(shell) => format!("[\"{shell}\", {path}]"),
        None => format!("[{path}]"),
    }
}

/// Write a script printing `cmd-value-for-<arg>`; returns the TOML argv
/// that runs it.
fn echo_script(dir: &Path) -> String {
    let (name, text, shell) = if cfg!(windows) {
        ("pass.cmd", "@echo off\r\necho cmd-value-for-%1\r\n", None)
    } else {
        (
            "pass.sh",
            "#!/bin/sh\necho \"cmd-value-for-$1\"\n",
            Some("sh"),
        )
    };
    let path = dir.join(name);
    fs::write(&path, text).unwrap();
    // A TOML literal string: backslashes of Windows paths stay as they are.
    let path = format!("'{}'", path.display());
    match shell {
        Some(shell) => format!("[\"{shell}\", {path}]"),
        None => format!("[{path}]"),
    }
}

#[test]
fn keychain_provider_round_trips_secret_set() {
    let server = Server::http();
    let ws = workspace(&server);
    let config = tempfile::tempdir().unwrap();
    write(
        ws.path(),
        "rustman.toml",
        "[workspace]\nname = \"t\"\nversion = 1\n\n[secret_providers.kc]\ntype = \"keychain\"\n",
    );
    write(
        ws.path(),
        "environments/dev.yaml",
        &format!(
            "variables:\n  base_url: {}\nsecrets: [api_token]\n",
            server.url("")
        ),
    );
    rustman_with(ws.path(), config.path())
        .args(["secret", "set", "api_token", "--env", "dev"])
        .write_stdin(CANARY)
        .assert()
        .success();
    write(
        ws.path(),
        "echo.rman.yaml",
        &request(
            "echo",
            "{{base_url}}/echo",
            "headers:\n  - name: X-Token\n    value: '{{secret:kc/dev/api_token}}'\n",
        ),
    );
    let out = rustman_with(ws.path(), config.path())
        .args(["send", "echo", "--env", "dev"])
        .assert()
        .success()
        .get_output()
        .clone();
    let printed = String::from_utf8_lossy(&out.stdout);
    assert!(printed.contains("«redacted:kc/dev/api_token»"), "{printed}");
    assert!(!printed.contains(CANARY), "{printed}");
}

/// A shared `rustman.toml` naming Vault without `address` breaks nothing
/// for a teammate without `VAULT_ADDR`: requests that use no Vault secret
/// are sent, and a Vault reference fails as that request's error.
#[test]
fn a_vault_without_address_fails_only_its_references() {
    let server = Server::http();
    let extra = "\n[secret_providers.vault]\ntype = \"vault\"\n";
    let ws = provider_workspace(&server, extra);
    let config = tempfile::tempdir().unwrap();
    write(
        ws.path(),
        "plain.rman.yaml",
        &request("plain", "{{base_url}}/get", ""),
    );
    write(
        ws.path(),
        "v.rman.yaml",
        &request("v", "{{base_url}}/get?k={{secret:vault/kv/app#pw}}", ""),
    );
    let send = |name: &str| {
        let mut cmd = rustman_with(ws.path(), config.path());
        cmd.env_remove("VAULT_ADDR")
            .args(["send", name, "--env", "dev"]);
        cmd.output().unwrap()
    };
    let out = send("plain");
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let out = send("v");
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("vault/kv/app#pw: vault: no address (set 'address' or VAULT_ADDR)"),
        "{stderr}"
    );
}

/// `rustman bench` resolves references through the providers, as `send`
/// and `run` do, and never shows the value.
#[test]
fn bench_resolves_provider_references() {
    let server = Server::http();
    let ws = provider_workspace(&server, "");
    let config = tempfile::tempdir().unwrap();
    let id = common::unique(ws.path());
    let url = format!("{{{{base_url}}}}/timed/0?id={id}-{{{{secret:ci/RUSTMAN_TEST_CANARY}}}}");
    write(ws.path(), "a.rman.yaml", &request("a", &url, ""));
    let origin = server.url("");
    let out = rustman_with(ws.path(), config.path())
        .args(["bench", "a.rman.yaml", "--env", "dev", "--iterations", "3"])
        .args(["--target", origin.trim_end_matches('/')])
        .args(["--threshold", "error_rate<1%"])
        .output()
        .unwrap();
    let all = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(out.status.success(), "{all}");
    assert!(!all.contains(CANARY), "{all}");
    assert_eq!(timed_hits(&format!("{id}-{CANARY}")).len(), 3);
}

/// `rustman history replay` fetches provider secrets again, directly
/// referenced or behind an environment variable, and keeps none in history.
#[test]
fn history_replay_fetches_provider_secrets_again() {
    let server = Server::http();
    let ws = provider_workspace(&server, "");
    let config = tempfile::tempdir().unwrap();
    let env_text = format!(
        "variables:\n  base_url: {}\n  api_token: {{secret: \"ci/RUSTMAN_TEST_CANARY\"}}\n",
        server.url("")
    );
    write(ws.path(), "environments/dev.yaml", &env_text);
    let id = common::unique(ws.path());
    let url = format!(
        "{{{{base_url}}}}/timed/0?id={id}-{{{{secret:ci/RUSTMAN_TEST_CANARY}}}}-{{{{api_token}}}}"
    );
    write(ws.path(), "a.rman.yaml", &request("a", &url, ""));
    rustman_with(ws.path(), config.path())
        .args(["send", "a", "--env", "dev"])
        .assert()
        .success();
    let history = history_text(ws.path());
    assert!(!history.contains(CANARY), "{history}");
    let out = rustman_with(ws.path(), config.path())
        .args(["history", "list", "--json"])
        .output()
        .unwrap();
    let rows: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    let entry = rows[0]["id"].as_str().unwrap().to_owned();
    let out = rustman_with(ws.path(), config.path())
        .args(["history", "replay", &entry])
        .output()
        .unwrap();
    let all = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(out.status.success(), "{all}");
    assert!(!all.contains(CANARY), "{all}");
    assert_eq!(timed_hits(&format!("{id}-{CANARY}-{CANARY}")).len(), 2);
    for dir in [ws.path(), config.path()] {
        let leaks = files_containing(dir, CANARY.as_bytes());
        assert!(leaks.is_empty(), "the value was written to {leaks:?}");
    }
}
