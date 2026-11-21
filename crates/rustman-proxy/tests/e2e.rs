//! End to end (add-traffic-recorder task 8.1): a client trusting the local
//! CA calls a local TLS API through the intercepting proxy, as `curl -x`
//! does; the session is then saved as request files and exported as HAR,
//! and no raw secret is left anywhere in the workspace. Runs on every CI
//! platform: the CA is generated in memory, so neither the OS keychain
//! nor a trust store is touched.

mod common;

use std::path::{Path, PathBuf};

use bytes::Bytes;
use http::Request;
use http_body_util::Full;
use rustman_proxy::ca::LocalCa;
use rustman_proxy::har::write_har;
use rustman_proxy::save::{SaveOptions, save};
use rustman_proxy::{ProxyConfig, Recorded};
use rustman_test_support::certs::TestCerts;
use rustman_test_support::server::{TestServer, Tls};
use serde_json::Value;

use common::{client_tls, connect, handshake, send_http1, start, trusting};

const TOKEN: &str = "eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiI0MiJ9.c2lnbmF0dXJl";
const COOKIE: &str = "sid=c00kie-e2e-7f3a";
const SCHEMA: &str = include_str!("fixtures/har-1.2.schema.json");

/// Every file under `dir`.
fn files(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for entry in std::fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            out.extend(files(&path));
        } else {
            out.push(path);
        }
    }
    out
}

fn workspace() -> tempfile::TempDir {
    let ws = tempfile::tempdir().unwrap();
    std::fs::write(
        ws.path().join("rustman.toml"),
        "[workspace]\nname = \"e2e\"\nversion = 1\n",
    )
    .unwrap();
    ws
}

fn har_errors(doc: &Value) -> Vec<String> {
    let schema: Value = serde_json::from_str(SCHEMA).unwrap();
    let validator = jsonschema::options()
        .should_validate_formats(true)
        .build(&schema)
        .unwrap();
    validator.iter_errors(doc).map(|e| e.to_string()).collect()
}

#[tokio::test]
async fn record_intercept_save_and_har_leave_no_secret_in_the_workspace() {
    let certs = TestCerts::generate();
    let api = TestServer::https(&certs, Tls::H2AndHttp1).await;
    let ca = LocalCa::generate("Rustman E2E CA").unwrap();
    let config = ProxyConfig {
        intercept: vec!["localhost".into()],
        upstream_tls: trusting(&certs.ca_pem),
        ..ProxyConfig::default()
    };
    let proxy = start(config, Some(&ca)).await;
    assert!(proxy.addr.ip().is_loopback());
    let authority = format!("localhost:{}", api.port());

    for id in [17, 42] {
        let tcp = connect(&proxy, &authority).await;
        let tls = handshake(tcp, &client_tls(ca.cert_pem(), &[b"http/1.1"]), "localhost")
            .await
            .expect("the client trusts the local CA");
        let request = Request::get(format!("/users/{id}"))
            .header("host", &authority)
            .header("authorization", format!("Bearer {TOKEN}"))
            .header("cookie", COOKIE)
            .body(Full::new(Bytes::new()))
            .unwrap();
        send_http1(tls, request).await;
    }
    let captures = proxy.captures(2).await;
    assert!(
        captures
            .iter()
            .all(|c| matches!(&c.recorded, Recorded::Exchange(e) if e.intercepted)),
        "{captures:?}"
    );

    let ws = workspace();
    let options = SaveOptions {
        root: ws.path(),
        into: None,
        environment: "recorded".into(),
    };
    let saved = save(&captures, &options, None).unwrap();
    assert_eq!(saved.written, 1);
    assert_eq!(saved.secrets, ["auth_token", "cookie"]);
    let har_path = ws.path().join("out.har");
    write_har(&har_path, &captures, false).unwrap();

    let host_dir = format!("recorded/localhost-{}", api.port());
    let file =
        std::fs::read_to_string(ws.path().join(&host_dir).join("get-users-id.rman.yaml")).unwrap();
    assert!(file.contains("{{base_url}}/users/{{user_id}}"), "{file}");
    assert!(file.contains("Bearer {{auth_token}}"), "{file}");
    let env = std::fs::read_to_string(ws.path().join("environments/recorded.yaml")).unwrap();
    assert!(
        env.contains(&format!("base_url: https://localhost:{}", api.port())),
        "{env}"
    );

    let har: Value = serde_json::from_str(&std::fs::read_to_string(&har_path).unwrap()).unwrap();
    assert_eq!(har_errors(&har), Vec::<String>::new());
    let entries = har["log"]["entries"].as_array().unwrap();
    assert_eq!(entries.len(), 2);
    assert_eq!(entries[0]["comment"], "decrypted by the Rustman local CA");

    for path in files(ws.path()) {
        let text = String::from_utf8_lossy(&std::fs::read(&path).unwrap()).into_owned();
        for secret in ["eyJ", "c00kie-e2e-7f3a"] {
            assert!(!text.contains(secret), "{secret} in {}", path.display());
        }
    }
}
