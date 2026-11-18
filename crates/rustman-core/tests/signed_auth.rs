//! Signature auth in a workspace (runner-parity tasks 3.5 and 3.6): the
//! JWT key checks when the workspace loads, interpolated claims, the key
//! file kept inside the workspace, and the token recorded as
//! `«redacted:jwt»`.

use std::fs;
use std::path::Path;

use rustman_core::model::{Auth, Jwt};
use rustman_core::run::SentRequest;
use rustman_core::vars::{NoSecrets, Resolver, resolve_request};
use rustman_core::{ConfigError, Redactor, Workspace};

/// The start of a PKCS#8 EC (P-256) private key: enough for the key-kind
/// check, which reads the algorithm identifier.
const EC_KEY: &str = "-----BEGIN PRIVATE KEY-----
MIGHAgEAMBMGByqGSM49AgEGCCqGSM49AwEHBG0wawIBAQQgevZzL1gdAFr88hb2
OF/2NxApJCzGCEDdfSp6VQO30hyhRANCAAQRWz+jn65BtOMvdyHKcvjBeBSDZH2r
1RTwjmYSi9R/zpBnuQ4EiMnCqfMr3mR8ZJ8a8gBaiEC6Dn2kDeHd8Mu1
-----END PRIVATE KEY-----
";

fn workspace(request: &str) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let write = |rel: &str, text: &str| {
        let path = dir.path().join(rel);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, text).unwrap();
    };
    write(
        "rustman.toml",
        "[workspace]\nname = \"t\"\nversion = 1\n[variables]\nuser_id = \"42\"\n",
    );
    write("keys/ec.pem", EC_KEY);
    write("api/me.rman.yaml", request);
    dir
}

fn request(auth: &str) -> String {
    format!("version: 1\nkind: http\nname: me\nurl: http://h/me\nauth:\n{auth}")
}

fn load_error(root: &Path) -> String {
    match Workspace::load(root) {
        Err(ConfigError::Invalid(errors)) => errors[0].to_string().replace('\\', "/"),
        other => panic!("expected a load error, got {other:?}"),
    }
}

#[test]
fn a_key_of_the_wrong_kind_fails_the_load() {
    let ws = workspace(&request(
        "  type: jwt\n  algorithm: RS256\n  private_key_file: keys/ec.pem\n",
    ));
    let error = load_error(ws.path());
    assert!(
        error.ends_with("jwt: RS256 requires an RSA private key"),
        "{error}"
    );
    assert!(error.starts_with("api/me.rman.yaml:8"), "{error}");
}

#[test]
fn a_key_file_outside_the_workspace_fails_the_load() {
    let ws = workspace(&request(
        "  type: jwt\n  algorithm: ES256\n  private_key_file: ../outside.pem\n",
    ));
    let error = load_error(ws.path());
    assert!(
        error.ends_with("file path escapes workspace: ../outside.pem"),
        "{error}"
    );
}

#[test]
fn claims_are_interpolated_and_the_key_file_resolved() {
    let ws = workspace(&request(
        "  type: jwt\n  algorithm: ES256\n  private_key_file: keys/ec.pem\n  claims: {sub: '{{user_id}}', n: 1}\n",
    ));
    let workspace = Workspace::load(ws.path()).unwrap();
    let request = &workspace.find_by_name("me")[0];
    let resolver = Resolver::for_request(&workspace.settings, request, None, &NoSecrets);
    let resolved = resolve_request(request, &resolver).unwrap();
    let Some(Auth::Jwt(jwt)) = &resolved.auth else {
        panic!("{:?}", resolved.auth);
    };
    let Jwt {
        claims,
        private_key_file,
        ..
    } = jwt.as_ref();
    assert_eq!(claims["sub"], "42");
    assert_eq!(claims["n"], 1);
    let path = private_key_file.as_deref().unwrap();
    assert_eq!(fs::read_to_string(path).unwrap(), EC_KEY);

    let sent = SentRequest::new(&resolved, &Redactor::for_request(&resolved));
    let header = sent
        .headers
        .iter()
        .find(|h| h.name == "Authorization")
        .unwrap();
    assert_eq!(header.value, "Bearer «redacted:jwt»");
}

#[test]
fn a_query_token_is_recorded_redacted_in_the_url() {
    let ws = workspace(&request(
        "  type: jwt\n  algorithm: HS256\n  secret: k\n  in: query\n  name: token\n",
    ));
    let workspace = Workspace::load(ws.path()).unwrap();
    let request = &workspace.find_by_name("me")[0];
    let resolver = Resolver::for_request(&workspace.settings, request, None, &NoSecrets);
    let resolved = resolve_request(request, &resolver).unwrap();
    let sent = SentRequest::new(&resolved, &Redactor::for_request(&resolved));
    assert_eq!(sent.url, "http://h/me?token=«redacted:jwt»");
}

#[test]
fn literal_signature_credentials_are_redacted() {
    let ws = workspace(&request(
        "  type: aws_sigv4\n  region: r\n  service: s3\n  access_key: AKID\n  secret_key: sk-literal-123\n  session_token: st-literal-123\n",
    ));
    let workspace = Workspace::load(ws.path()).unwrap();
    let request = &workspace.find_by_name("me")[0];
    let resolver = Resolver::for_request(&workspace.settings, request, None, &NoSecrets);
    let resolved = resolve_request(request, &resolver).unwrap();
    let shown = Redactor::for_request(&resolved)
        .text("echo sk-literal-123 st-literal-123")
        .into_owned();
    assert_eq!(
        shown,
        "echo «redacted:aws_secret_key» «redacted:aws_session_token»"
    );
}
