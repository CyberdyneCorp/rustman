//! Digest, AWS SigV4 and JWT auth through `rustman send` (runner-parity
//! tasks 3.3, 3.5 and 3.6): exit codes, credential sources and what reaches
//! the terminal and history. Every server is in-process.

mod common;

use std::fs;
use std::path::Path;

use common::{Server, request, rustman, workspace, write};
use predicates::prelude::*;
use rustman_test_support::DigestServer;

/// A runtime serving a Digest server for the `rustman` binary.
struct Digest {
    server: DigestServer,
    _runtime: tokio::runtime::Runtime,
}

impl Digest {
    fn start() -> Self {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .unwrap();
        let server = runtime.block_on(DigestServer::start("SHA-256"));
        Self {
            server,
            _runtime: runtime,
        }
    }
}

fn history_text(ws: &Path) -> String {
    let mut text = String::new();
    for entry in fs::read_dir(ws.join(".rustman/history")).unwrap() {
        let path = entry.unwrap().path();
        if path.extension().is_some_and(|x| x == "json") {
            text.push_str(&fs::read_to_string(path).unwrap());
        }
    }
    text
}

#[test]
fn digest_auth_sends_after_the_challenge() {
    let digest = Digest::start();
    let server = Server::http();
    let ws = workspace(&server);
    let auth = "auth:\n  type: digest\n  username: Mufasa\n  password: Circle of Life\n";
    let url = digest.server.url("/data");
    write(ws.path(), "d.rman.yaml", &request("d", &url, auth));
    rustman(ws.path())
        .args(["send", "d", "--env", "dev", "--body-only"])
        .assert()
        .success()
        .stdout("authorized");
    assert_eq!(digest.server.hits().len(), 2);
}

#[test]
fn a_digest_body_over_10_mib_fails_before_sending() {
    let digest = Digest::start();
    let server = Server::http();
    let ws = workspace(&server);
    let big = fs::File::create(ws.path().join("big.bin")).unwrap();
    big.set_len(50 * 1024 * 1024).unwrap();
    let extra = "method: PUT\nbody:\n  type: file\n  path: big.bin\nauth:\n  type: digest\n  username: Mufasa\n  password: Circle of Life\n";
    let url = digest.server.url("/upload");
    write(ws.path(), "up.rman.yaml", &request("up", &url, extra));
    rustman(ws.path())
        .args(["send", "up", "--env", "dev"])
        .assert()
        .code(3)
        .stderr(predicate::str::contains(
            "digest: body larger than 10 MiB cannot be replayed",
        ));
    assert!(digest.server.hits().is_empty());
    assert_eq!(digest.server.body_bytes(), 0);
}

const SIGV4_ECHO: &str = "auth:\n  type: aws_sigv4\n  region: eu-west-1\n  service: execute-api\n";

/// A home folder holding `~/.aws/credentials` and `~/.aws/config`.
fn aws_home(credentials: &str, config: &str) -> tempfile::TempDir {
    let home = tempfile::tempdir().unwrap();
    write(home.path(), ".aws/credentials", credentials);
    write(home.path(), ".aws/config", config);
    home
}

fn sigv4_send(ws: &Path, home: &Path) -> assert_cmd::Command {
    let mut cmd = rustman(ws);
    cmd.args(["send", "s", "--env", "dev", "--no-history"])
        .env("HOME", home)
        .env("USERPROFILE", home)
        .env_remove("AWS_SHARED_CREDENTIALS_FILE")
        .env_remove("AWS_CONFIG_FILE")
        .env_remove("AWS_ACCESS_KEY_ID")
        .env_remove("AWS_SECRET_ACCESS_KEY")
        .env_remove("AWS_SESSION_TOKEN");
    cmd
}

#[test]
fn sigv4_reads_static_keys_from_a_profile() {
    let server = Server::http();
    let ws = workspace(&server);
    let auth = format!("{SIGV4_ECHO}  from: profile\n  profile: dev\n");
    write(
        ws.path(),
        "s.rman.yaml",
        &request("s", "{{base_url}}/aws-credential", &auth),
    );
    let home = aws_home(
        "[dev]\naws_access_key_id = AKIDPROFILE\naws_secret_access_key = PROFILE-SECRET-CANARY\n",
        "",
    );
    let output = sigv4_send(ws.path(), home.path())
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let stdout = String::from_utf8(output).unwrap();
    let scope = regex::Regex::new(r"\n\nAKIDPROFILE/[0-9]{8}/eu-west-1/execute-api/aws4_request\n");
    assert!(scope.unwrap().is_match(&stdout), "{stdout}");
    assert!(!stdout.contains("PROFILE-SECRET-CANARY"), "{stdout}");
}

#[test]
fn collection_headers_are_signed() {
    let server = Server::http();
    let ws = workspace(&server);
    write(
        ws.path(),
        "_collection.yaml",
        "headers:\n  - name: X-Trace\n    value: '1'\n",
    );
    let auth = format!("{SIGV4_ECHO}  access_key: AKID\n  secret_key: trace-secret-123\n");
    write(
        ws.path(),
        "s.rman.yaml",
        &request("s", "{{base_url}}/aws-credential", &auth),
    );
    let home = aws_home("", "");
    sigv4_send(ws.path(), home.path())
        .assert()
        .success()
        .stdout(predicate::str::ends_with(
            "\naccept;host;x-amz-content-sha256;x-amz-date;x-trace",
        ));
}

#[test]
fn an_sso_profile_is_refused() {
    let server = Server::http();
    let ws = workspace(&server);
    let auth = format!("{SIGV4_ECHO}  from: profile\n  profile: dev\n");
    write(
        ws.path(),
        "s.rman.yaml",
        &request("s", "{{base_url}}/echo", &auth),
    );
    let home = aws_home(
        "",
        "[profile dev]\nsso_start_url = https://sso.example/start\n",
    );
    sigv4_send(ws.path(), home.path())
        .assert()
        .code(3)
        .stderr(predicate::str::contains(
            "aws_sigv4: profile 'dev' uses sso_start_url, which is not supported; export temporary credentials or use a secret provider",
        ));
}

#[test]
fn sigv4_reads_the_environment_and_never_shows_the_secrets() {
    let server = Server::http();
    let ws = workspace(&server);
    let auth = format!("{SIGV4_ECHO}  from: env\n");
    write(
        ws.path(),
        "s.rman.yaml",
        &request("s", "{{base_url}}/echo", &auth),
    );
    let home = aws_home("", "");
    let output = sigv4_send(ws.path(), home.path())
        .env("AWS_ACCESS_KEY_ID", "AKIDENV")
        .env("AWS_SECRET_ACCESS_KEY", "ENV-SECRET-CANARY")
        .env("AWS_SESSION_TOKEN", "ENV-TOKEN-CANARY")
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let stdout = String::from_utf8(output).unwrap();
    assert!(
        stdout.contains("AWS4-HMAC-SHA256 «redacted:Authorization»"),
        "{stdout}"
    );
    assert!(stdout.contains("x-amz-security-token"), "{stdout}");
    assert!(stdout.contains("«redacted:aws_session_token»"), "{stdout}");
    assert!(!stdout.contains("ENV-TOKEN-CANARY"), "{stdout}");
    assert!(!stdout.contains("ENV-SECRET-CANARY"), "{stdout}");
    // The scope names the access key from the environment.
    write(
        ws.path(),
        "s.rman.yaml",
        &request("s", "{{base_url}}/aws-credential", &auth),
    );
    sigv4_send(ws.path(), home.path())
        .env("AWS_ACCESS_KEY_ID", "AKIDENV")
        .env("AWS_SECRET_ACCESS_KEY", "ENV-SECRET-CANARY")
        .assert()
        .success()
        .stdout(predicate::str::contains("\n\nAKIDENV/"));
}

#[test]
fn a_clock_skew_rejection_prints_the_hint() {
    let server = Server::http();
    let ws = workspace(&server);
    let auth = format!("{SIGV4_ECHO}  access_key: AKID\n  secret_key: skew-secret-123\n");
    write(
        ws.path(),
        "s.rman.yaml",
        &request("s", "{{base_url}}/aws-skew", &auth),
    );
    let home = aws_home("", "");
    sigv4_send(ws.path(), home.path()).assert().stderr(
        predicate::str::is_match(
            "check the system clock; request was signed at [0-9]{8}T[0-9]{6}Z",
        )
        .unwrap(),
    );
}

#[test]
fn jwt_tokens_are_redacted_in_history_and_output() {
    let server = Server::http();
    let ws = workspace(&server);
    write(
        ws.path(),
        "environments/dev.yaml",
        &format!(
            "variables:\n  base_url: {}\n  user_id: '42'\nsecrets: [jwt_secret]\n",
            server.url("")
        ),
    );
    let auth = "auth:\n  type: jwt\n  algorithm: HS256\n  secret: '{{jwt_secret}}'\n  claims: {sub: '{{user_id}}', scope: read}\n";
    write(
        ws.path(),
        "j.rman.yaml",
        &request("j", "{{base_url}}/echo", auth),
    );
    rustman(ws.path())
        .args(["secret", "set", "jwt_secret", "--env", "dev"])
        .write_stdin("JWT-SECRET-CANARY")
        .assert()
        .success();
    let output = rustman(ws.path())
        .args(["send", "j", "--env", "dev"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let stdout = String::from_utf8(output).unwrap();
    assert!(stdout.contains("Bearer «redacted:jwt»"), "{stdout}");
    assert!(!stdout.contains("eyJ"), "{stdout}");
    let history = history_text(ws.path());
    assert!(history.contains("Bearer «redacted:jwt»"), "{history}");
    assert!(!history.contains("eyJ"), "{history}");
    assert!(!history.contains("JWT-SECRET-CANARY"), "{history}");
}

#[test]
fn a_jwt_key_of_the_wrong_kind_fails_the_workspace_load() {
    let server = Server::http();
    let ws = workspace(&server);
    let key = fs::read(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../rustman-http/tests/fixtures/jwt/ec.pem"
    ))
    .unwrap();
    fs::create_dir_all(ws.path().join("keys")).unwrap();
    fs::write(ws.path().join("keys/ec.pem"), key).unwrap();
    let auth = "auth:\n  type: jwt\n  algorithm: RS256\n  private_key_file: keys/ec.pem\n";
    write(
        ws.path(),
        "j.rman.yaml",
        &request("j", "{{base_url}}/echo", auth),
    );
    rustman(ws.path())
        .args(["send", "j", "--env", "dev"])
        .assert()
        .code(2)
        .stderr(predicate::str::contains(
            "jwt: RS256 requires an RSA private key",
        ));
}
