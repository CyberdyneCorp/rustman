//! A bare hyper client for the server self-test, independent of the engine.

use std::sync::Arc;

use bytes::Bytes;
use http::{HeaderMap, Request, StatusCode, Version};
use http_body_util::{BodyExt, Empty};
use hyper::client::conn::{http1, http2};
use hyper_util::rt::{TokioExecutor, TokioIo};
use rustls::ClientConfig;
use rustls_pki_types::ServerName;
use tokio::io::{AsyncRead, AsyncWrite};
use tokio_rustls::TlsConnector;

use super::certs::{TestCerts, certs_from_pem, key_from_pem};
use super::server::{TestServer, tcp};

pub struct RawResponse {
    pub version: Version,
    pub status: StatusCode,
    pub headers: HeaderMap,
    pub body: Bytes,
}

/// How the raw client talks to the server.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    Http1,
    /// HTTP/2 over cleartext with prior knowledge.
    H2c,
    /// TLS trusting the test CA, offering `h2` and `http/1.1`.
    Tls,
    /// TLS presenting the test client certificate.
    TlsWithClientCert,
}

pub async fn get(server: &TestServer, path: &str, mode: Mode, certs: &TestCerts) -> RawResponse {
    try_get(server, path, mode, certs).await.unwrap()
}

pub async fn try_get(
    server: &TestServer,
    path: &str,
    mode: Mode,
    certs: &TestCerts,
) -> Result<RawResponse, String> {
    let tcp = tcp(server).await;
    let request = Request::get(server.url(path))
        .header("host", format!("localhost:{}", server.port()))
        .body(Empty::<Bytes>::new())
        .unwrap();
    match mode {
        Mode::Http1 => send(tcp, request, false).await,
        Mode::H2c => send(tcp, request, true).await,
        Mode::Tls | Mode::TlsWithClientCert => {
            let config = client_config(certs, mode == Mode::TlsWithClientCert);
            let name = ServerName::try_from("localhost").unwrap();
            let tls = TlsConnector::from(Arc::new(config))
                .connect(name, tcp)
                .await
                .map_err(|e| e.to_string())?;
            let h2 = tls.get_ref().1.alpn_protocol() == Some(b"h2");
            send(tls, request, h2).await
        }
    }
}

async fn send<S>(io: S, mut request: Request<Empty<Bytes>>, h2: bool) -> Result<RawResponse, String>
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    let io = TokioIo::new(io);
    let response = if h2 {
        *request.version_mut() = Version::HTTP_2;
        request.headers_mut().remove("host");
        let (mut sender, conn) = http2::handshake(TokioExecutor::new(), io)
            .await
            .map_err(|e| e.to_string())?;
        tokio::spawn(conn);
        sender.send_request(request).await
    } else {
        let path = request.uri().path_and_query().unwrap().as_str().to_owned();
        *request.uri_mut() = path.parse().unwrap();
        let (mut sender, conn) = http1::handshake(io).await.map_err(|e| e.to_string())?;
        tokio::spawn(conn);
        sender.send_request(request).await
    }
    .map_err(|e| e.to_string())?;
    let (parts, body) = response.into_parts();
    let body = body.collect().await.map_err(|e| e.to_string())?.to_bytes();
    Ok(RawResponse {
        version: parts.version,
        status: parts.status,
        headers: parts.headers,
        body,
    })
}

fn client_config(certs: &TestCerts, client_cert: bool) -> ClientConfig {
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let mut roots = rustls::RootCertStore::empty();
    roots.add_parsable_certificates(certs_from_pem(&certs.ca_pem));
    let builder = ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .unwrap()
        .with_root_certificates(roots);
    let mut config = if client_cert {
        builder
            .with_client_auth_cert(
                certs_from_pem(&certs.client_cert_pem),
                key_from_pem(&certs.client_key_pem),
            )
            .unwrap()
    } else {
        builder.with_no_client_auth()
    };
    config.alpn_protocols = vec![b"h2".to_vec(), b"http/1.1".to_vec()];
    config
}
