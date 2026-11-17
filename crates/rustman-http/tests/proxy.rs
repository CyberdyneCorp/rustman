//! Proxies: environment variables, explicit, `none`, HTTP `CONNECT`,
//! forwarding and SOCKS5 (task 4.6).

mod support;

use std::collections::HashMap;
use std::path::Path;

use rustman_core::settings::Settings;
use rustman_http::{
    ClientAuth, HttpEngine, HttpVersion, ProxySettings, RequestError, SendOptions, TlsSettings,
};
use support::proxy::HttpProxyOptions;
use support::request::{echoed_header, get};
use support::{TestCerts, TestProxy, TestServer, Tls};

/// Options trusting the test CA, with `proxy`.
fn via(proxy: ProxySettings, certs: &TestCerts) -> SendOptions {
    SendOptions {
        tls: TlsSettings {
            ca_bundle: Some(certs.ca_pem.clone().into_bytes()),
            ..TlsSettings::default()
        },
        proxy,
        ..SendOptions::default()
    }
}

fn from_env(vars: &[(&str, &str)]) -> ProxySettings {
    let vars: HashMap<String, String> = vars
        .iter()
        .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
        .collect();
    ProxySettings::from_env_with(|name| vars.get(name).cloned()).unwrap()
}

#[tokio::test]
async fn an_environment_proxy_tunnels_https_with_connect() {
    let certs = TestCerts::generate();
    let server = TestServer::https(&certs, Tls::H2AndHttp1).await;
    let proxy = TestProxy::http().await;
    let options = via(from_env(&[("HTTPS_PROXY", &proxy.url())]), &certs);
    let exchange = HttpEngine::new()
        .send(&get(&server.url("/status/200")), &options)
        .await
        .unwrap();
    assert_eq!(exchange.status, 200);
    assert_eq!(exchange.http_version, HttpVersion::Http2);
    assert!(exchange.timings.tls.is_some());
    let log = proxy.log();
    assert_eq!(log.len(), 1);
    let connect = format!("CONNECT localhost:{} HTTP/1.1\r\n", server.port());
    assert!(log[0].starts_with(&connect), "{log:?}");
}

#[tokio::test]
async fn no_proxy_hosts_connect_directly() {
    let certs = TestCerts::generate();
    let server = TestServer::https(&certs, Tls::H2AndHttp1).await;
    let proxy = TestProxy::http().await;
    let env = from_env(&[
        ("HTTPS_PROXY", &proxy.url()),
        ("NO_PROXY", "example.org,localhost"),
    ]);
    let exchange = HttpEngine::new()
        .send(&get(&server.url("/status/200")), &via(env, &certs))
        .await
        .unwrap();
    assert_eq!(exchange.status, 200);
    assert!(proxy.log().is_empty(), "{:?}", proxy.log());
}

#[tokio::test]
async fn plain_http_is_forwarded_with_proxy_credentials() {
    let server = TestServer::http().await;
    let proxy = TestProxy::http_with(HttpProxyOptions {
        require_auth: Some("Basic YW5hOnB3".into()),
        ..HttpProxyOptions::default()
    })
    .await;
    let url = proxy.url().replace("http://", "http://ana:pw@");
    let options = via(
        ProxySettings::explicit(&url).unwrap(),
        &TestCerts::generate(),
    );
    let exchange = HttpEngine::new()
        .send(&get(&server.url("/echo")), &options)
        .await
        .unwrap();
    assert_eq!(exchange.status, 200);
    let log = proxy.log();
    let forwarded = format!("GET {} HTTP/1.1\r\n", server.url("/echo"));
    assert!(log[0].starts_with(&forwarded), "{log:?}");
    let echo: serde_json::Value = serde_json::from_slice(&exchange.body).unwrap();
    assert_eq!(echo["path"], "/echo");
}

#[tokio::test]
async fn connect_sends_proxy_credentials() {
    let certs = TestCerts::generate();
    let server = TestServer::https(&certs, Tls::H2AndHttp1).await;
    let proxy = TestProxy::http_with(HttpProxyOptions {
        require_auth: Some("Basic YW5hOnB3".into()),
        ..HttpProxyOptions::default()
    })
    .await;
    let with_auth = proxy.url().replace("http://", "http://ana:pw@");
    let options = via(ProxySettings::explicit(&with_auth).unwrap(), &certs);
    let exchange = HttpEngine::new()
        .send(&get(&server.url("/echo")), &options)
        .await
        .unwrap();
    let echo: serde_json::Value = serde_json::from_slice(&exchange.body).unwrap();
    // The credentials stay with the proxy; the server never sees them.
    assert!(echoed_header(&echo, "proxy-authorization").is_empty());
}

#[tokio::test]
async fn a_refused_connect_is_a_proxy_error() {
    let certs = TestCerts::generate();
    let server = TestServer::https(&certs, Tls::H2AndHttp1).await;
    let proxy = TestProxy::http_with(HttpProxyOptions {
        require_auth: Some("Basic YW5hOnB3".into()),
        ..HttpProxyOptions::default()
    })
    .await;
    let options = via(ProxySettings::explicit(&proxy.url()).unwrap(), &certs);
    let err = HttpEngine::new()
        .send(&get(&server.url("/")), &options)
        .await
        .unwrap_err();
    assert!(matches!(err, RequestError::Proxy(_)), "{err:?}");
    let expected = format!(
        "proxy: CONNECT to localhost:{} via {} failed: HTTP/1.1 407 Proxy Authentication Required",
        server.port(),
        proxy.url()
    );
    assert_eq!(err.to_string(), expected);
}

#[tokio::test]
async fn an_unreachable_proxy_is_a_proxy_error() {
    let port = {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        listener.local_addr().unwrap().port()
    };
    let options = SendOptions {
        proxy: ProxySettings::explicit(&format!("http://127.0.0.1:{port}")).unwrap(),
        ..SendOptions::default()
    };
    let err = HttpEngine::new()
        .send(&get("http://localhost:1/"), &options)
        .await
        .unwrap_err();
    assert!(
        err.to_string()
            .starts_with(&format!("proxy: cannot connect to 127.0.0.1:{port}")),
        "{err}"
    );
}

#[tokio::test]
async fn an_https_proxy_is_reached_over_tls() {
    let certs = TestCerts::generate();
    let server = TestServer::https(&certs, Tls::H2AndHttp1).await;
    let proxy = TestProxy::https(&certs).await;
    let options = via(
        ProxySettings::explicit(&proxy.localhost_url()).unwrap(),
        &certs,
    );
    let exchange = HttpEngine::new()
        .send(&get(&server.url("/status/200")), &options)
        .await
        .unwrap();
    assert_eq!(exchange.status, 200);
    assert!(proxy.log()[0].starts_with("CONNECT localhost:"));
}

#[tokio::test]
async fn the_client_certificate_is_not_offered_to_an_https_proxy() {
    let certs = TestCerts::generate();
    let server = TestServer::https(&certs, Tls::ClientAuth).await;
    let mut config = support::server::server_config(&certs, Tls::OptionalClientAuth);
    config.alpn_protocols = vec![b"http/1.1".to_vec()];
    let proxy = TestProxy::http_with(HttpProxyOptions {
        tls: Some(std::sync::Arc::new(config)),
        ..HttpProxyOptions::default()
    })
    .await;
    let mut options = via(
        ProxySettings::explicit(&proxy.localhost_url()).unwrap(),
        &certs,
    );
    let auth = ClientAuth::from_pem(
        certs.client_cert_pem.as_bytes(),
        certs.client_key_pem.as_bytes(),
    )
    .unwrap();
    options.tls.client_auth = Some(auth);
    let exchange = HttpEngine::new()
        .send(&get(&server.url("/status/200")), &options)
        .await
        .unwrap();
    // The origin required the certificate and got it; the proxy asked and
    // got none.
    assert_eq!(exchange.status, 200);
    let log = proxy.log();
    assert!(log[0].starts_with("CONNECT localhost:"), "{log:?}");
    assert!(!log.contains(&"client certificate".to_owned()), "{log:?}");
}

#[tokio::test]
async fn socks5_tunnels_https_and_plain_http() {
    let certs = TestCerts::generate();
    let tls_server = TestServer::https(&certs, Tls::H2AndHttp1).await;
    let plain_server = TestServer::http().await;
    let proxy = TestProxy::socks5(None).await;
    let options = via(from_env(&[("ALL_PROXY", &proxy.url())]), &certs);
    let engine = HttpEngine::new();
    for url in [
        tls_server.url("/status/200"),
        plain_server.url("/status/200"),
    ] {
        let exchange = engine.send(&get(&url), &options).await.unwrap();
        assert_eq!(exchange.status, 200, "{url}");
    }
    assert_eq!(
        proxy.log(),
        [
            format!("SOCKS5 localhost:{}", tls_server.port()),
            format!("SOCKS5 localhost:{}", plain_server.port()),
        ]
    );
}

#[tokio::test]
async fn socks5_sends_credentials() {
    let server = TestServer::http().await;
    let proxy = TestProxy::socks5(Some(("ana", "pw"))).await;
    let url = proxy.url().replace("socks5://", "socks5://ana:pw@");
    let options = SendOptions {
        proxy: ProxySettings::explicit(&url).unwrap(),
        ..SendOptions::default()
    };
    let exchange = HttpEngine::new()
        .send(&get(&server.url("/status/204")), &options)
        .await
        .unwrap();
    assert_eq!(exchange.status, 204);
    assert_eq!(proxy.log()[0], "auth ana:pw");
}

fn settings(extra: &str) -> Settings {
    let text = format!("[workspace]\nname = \"t\"\nversion = 1\n{extra}");
    Settings::parse(Path::new("rustman.toml"), &text).unwrap().0
}

#[tokio::test]
async fn proxy_none_bypasses_the_workspace_proxy() {
    let server = TestServer::http().await;
    let proxy = TestProxy::http().await;
    let settings = settings(&format!("[http]\nproxy = \"{}\"\n", proxy.url()));
    let dir = tempfile::tempdir().unwrap();
    let engine = HttpEngine::new();
    let mut request = get(&server.url("/status/200"));
    let options = SendOptions::for_request(&settings, dir.path(), &request).unwrap();
    engine.send(&request, &options).await.unwrap();
    assert_eq!(proxy.log().len(), 1);
    request.settings.proxy = Some("none".into());
    let options = SendOptions::for_request(&settings, dir.path(), &request).unwrap();
    engine.send(&request, &options).await.unwrap();
    assert_eq!(proxy.log().len(), 1);
}

#[tokio::test]
async fn proxy_errors_hide_credentials() {
    let mut request = get("http://localhost/");
    request.settings.proxy = Some("ftp://ana:s3cr3t@proxy".into());
    let dir = tempfile::tempdir().unwrap();
    let err = SendOptions::for_request(&settings(""), dir.path(), &request).unwrap_err();
    assert!(!err.to_string().contains("s3cr3t"), "{err}");
}
