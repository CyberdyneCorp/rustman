//! Sending one OTLP export request over rustman-http's own transport
//! (hyper with rustls on ring): gRPC unary over HTTP/2, or `http/protobuf`.
//! No environment variable is read here; [`ExportTarget`] already holds
//! everything.

use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use http::{HeaderMap, Method, Request};
use http_body_util::{BodyExt, Full};
use rustman_core::Redactor;
use rustman_core::trace::{ExportProtocol, ExportTarget};

use crate::dns::Dns;
use crate::proxy::ProxySettings;
use crate::tls::TlsSettings;
use crate::transport::{Transport, TransportKey};

/// How long connecting to the collector may take.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(3);
/// How long one export request may take in all.
const EXPORT_TIMEOUT: Duration = Duration::from_secs(10);

/// Sends export bodies to one collector.
pub(super) struct Sender {
    transport: Transport,
    target: ExportTarget,
    /// The export header values (usually an API key): a collector or
    /// proxy may echo them back in its error.
    redactor: Redactor,
}

impl Sender {
    pub fn new(target: ExportTarget) -> Result<Self, String> {
        let key = TransportKey {
            tls: TlsSettings::default(),
            connect_timeout: CONNECT_TIMEOUT,
            http2_prior_knowledge: target.protocol == ExportProtocol::Grpc,
            proxy: ProxySettings::default(),
        };
        let transport =
            Transport::new(Arc::new(Dns::default()), &key).map_err(|e| e.to_string())?;
        let redactor = header_redactor(&target.headers);
        Ok(Self {
            transport,
            target,
            redactor,
        })
    }

    /// Send `body` (an encoded `ExportTraceServiceRequest`); an error never
    /// shows an export header value.
    pub async fn export(&self, body: Vec<u8>) -> Result<(), String> {
        self.send(body)
            .await
            .map_err(|e| self.redactor.text(&e).into_owned())
    }

    async fn send(&self, body: Vec<u8>) -> Result<(), String> {
        let request = self.request(body)?;
        let exchange = async {
            let response = self
                .transport
                .open(request)
                .await
                .map_err(|e| e.to_string())?;
            let (head, body) = response.into_parts();
            let collected = body.collect().await.map_err(|e| e.to_string())?;
            check(
                self.target.protocol,
                head.status,
                &head.headers,
                collected.trailers(),
            )
        };
        tokio::time::timeout(EXPORT_TIMEOUT, exchange)
            .await
            .map_err(|_| format!("no answer within {}s", EXPORT_TIMEOUT.as_secs()))?
    }

    fn request(&self, body: Vec<u8>) -> Result<Request<Full<Bytes>>, String> {
        let (content_type, body) = match self.target.protocol {
            ExportProtocol::Grpc => ("application/grpc", grpc_frame(&body)),
            ExportProtocol::HttpProtobuf => ("application/x-protobuf", body),
        };
        let mut builder = Request::builder()
            .method(Method::POST)
            .uri(&self.target.url)
            .header(http::header::CONTENT_TYPE, content_type)
            .header(http::header::USER_AGENT, crate::USER_AGENT_VALUE);
        if self.target.protocol == ExportProtocol::Grpc {
            builder = builder.header(http::header::TE, "trailers");
        }
        for (name, value) in &self.target.headers {
            builder = builder.header(name.as_str(), value.as_str());
        }
        builder
            .body(Full::new(Bytes::from(body)))
            .map_err(|_| "invalid collector endpoint or header".to_owned())
    }
}

/// Redacts each header value, and the credential after its scheme.
fn header_redactor(headers: &[(String, String)]) -> Redactor {
    let mut redactor = Redactor::new();
    for (name, value) in headers {
        redactor.add_value(name, value);
        redactor.add_header_value(name, value);
    }
    redactor
}

/// A gRPC message frame: uncompressed flag, big-endian length, message.
fn grpc_frame(message: &[u8]) -> Vec<u8> {
    let len = u32::try_from(message.len()).unwrap_or(u32::MAX);
    let mut frame = Vec::with_capacity(message.len() + 5);
    frame.push(0);
    frame.extend_from_slice(&len.to_be_bytes());
    frame.extend_from_slice(message);
    frame
}

/// Whether the collector accepted the export.
fn check(
    protocol: ExportProtocol,
    status: http::StatusCode,
    headers: &HeaderMap,
    trailers: Option<&HeaderMap>,
) -> Result<(), String> {
    if !status.is_success() {
        return Err(format!("the collector answered HTTP {}", status.as_u16()));
    }
    if protocol == ExportProtocol::HttpProtobuf {
        return Ok(());
    }
    let field = |name: &str| {
        trailers
            .and_then(|t| t.get(name))
            .or_else(|| headers.get(name))
            .and_then(|v| v.to_str().ok())
    };
    match field("grpc-status") {
        Some("0") => Ok(()),
        Some(code) => Err(format!(
            "the collector answered gRPC status {code}: {}",
            field("grpc-message").unwrap_or_default()
        )),
        None => Err("the collector answered without a gRPC status".to_owned()),
    }
}

#[cfg(test)]
mod tests {
    use http::{HeaderValue, StatusCode};

    use super::*;

    fn trailers(status: &'static str) -> HeaderMap {
        let mut map = HeaderMap::new();
        map.insert("grpc-status", HeaderValue::from_static(status));
        map
    }

    #[test]
    fn errors_never_show_an_export_header_value() {
        let headers = [
            ("x-api-key".to_owned(), "k3y-c4n4ry-Qz9".to_owned()),
            (
                "authorization".to_owned(),
                "Bearer t0k-c4n4ry-Qz9".to_owned(),
            ),
        ];
        let redactor = header_redactor(&headers);
        let mut map = trailers("16");
        map.insert(
            "grpc-message",
            HeaderValue::from_static("bad key k3y-c4n4ry-Qz9 or token t0k-c4n4ry-Qz9"),
        );
        let error = check(ExportProtocol::Grpc, StatusCode::OK, &map, None).unwrap_err();
        let shown = redactor.text(&error);
        assert!(
            !shown.contains("c4n4ry") && shown.contains("gRPC status 16"),
            "{shown}"
        );
    }

    #[test]
    fn grpc_frames_carry_the_length() {
        assert_eq!(grpc_frame(b"ab"), [0, 0, 0, 0, 2, b'a', b'b']);
    }

    #[test]
    fn answers_are_checked_per_protocol() {
        let empty = HeaderMap::new();
        let grpc = ExportProtocol::Grpc;
        assert!(check(grpc, StatusCode::OK, &empty, Some(&trailers("0"))).is_ok());
        assert!(check(grpc, StatusCode::OK, &trailers("0"), None).is_ok());
        let unavailable = check(grpc, StatusCode::OK, &empty, Some(&trailers("14")));
        assert!(unavailable.unwrap_err().contains("gRPC status 14"));
        assert!(check(grpc, StatusCode::OK, &empty, None).is_err());
        assert!(check(grpc, StatusCode::BAD_GATEWAY, &empty, None).is_err());
        let http = ExportProtocol::HttpProtobuf;
        assert!(check(http, StatusCode::OK, &empty, None).is_ok());
        assert_eq!(
            check(http, StatusCode::UNAUTHORIZED, &empty, None).unwrap_err(),
            "the collector answered HTTP 401"
        );
    }
}
