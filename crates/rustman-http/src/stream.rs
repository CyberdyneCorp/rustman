//! Responses read as they arrive, for protocols that stream (GraphQL over
//! Server-Sent Events), and the head of a request for protocols that
//! upgrade one (WebSocket handshakes).
//!
//! Both build the request exactly as [`HttpEngine::send`] does: auth
//! helpers, signing, the jar's cookies and, for streams, the same TLS,
//! proxies and connection pools. Redirects are not followed and the body is
//! handed over chunk by chunk instead of being read whole. Every error is
//! redacted with the request's secrets.

use std::time::{Duration, Instant, SystemTime};

use bytes::Bytes;
use http::{HeaderMap, Uri};
use http_body_util::BodyExt;
use hyper::body::Incoming;
use rustman_core::Redactor;
use rustman_core::vars::ResolvedRequest;

use crate::engine::{HttpEngine, SendOptions, add_credentials, on_the_wire};
use crate::error::{Phase, RequestError};
use crate::exchange::HttpVersion;
use crate::transport::network_error;

/// A response whose body is still arriving.
#[derive(Debug)]
pub struct StreamedResponse {
    /// The URL the request was sent to.
    pub url: String,
    pub http_version: HttpVersion,
    pub status: u16,
    pub headers: HeaderMap,
    /// From sending the request until the response head arrived.
    pub ttfb: Duration,
    /// Sent over TLS with certificate verification off.
    pub insecure: bool,
    /// Non-fatal problems building the request.
    pub warnings: Vec<String>,
    /// The sensitive header values sent and received (cookies, proxy
    /// credentials), to merge into the request's redactor.
    pub credentials: Redactor,
    body: Incoming,
    redactor: Redactor,
}

impl StreamedResponse {
    /// The next chunk of the body, `None` at its end. Trailers are skipped.
    pub async fn chunk(&mut self) -> Option<Result<Bytes, RequestError>> {
        loop {
            match self.body.frame().await? {
                Ok(frame) => {
                    if let Ok(data) = frame.into_data() {
                        return Some(Ok(data));
                    }
                }
                Err(e) => return Some(Err(network_error(&e).redacted(&self.redactor))),
            }
        }
    }

    /// The header values of `name`, as text.
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers.get(name).and_then(|v| v.to_str().ok())
    }
}

impl HttpEngine {
    /// Send `request` and return as soon as the response head arrives,
    /// within `options.timeout`; the body is read with
    /// [`StreamedResponse::chunk`] for as long as the caller wants.
    pub async fn open_stream(
        &self,
        request: &ResolvedRequest,
        options: &SendOptions,
    ) -> Result<StreamedResponse, RequestError> {
        let redactor = Redactor::for_request(request);
        self.open_unredacted(request, options, &redactor)
            .await
            .map_err(|e| e.redacted(&redactor))
    }

    async fn open_unredacted(
        &self,
        request: &ResolvedRequest,
        options: &SendOptions,
        redactor: &Redactor,
    ) -> Result<StreamedResponse, RequestError> {
        let prepared = Self::prepare(request, None).await?;
        let transport = self.transport(options)?;
        let mut wire = on_the_wire(&prepared.outgoing, options);
        if let Some(signer) = &prepared.signer {
            signer.sign(&mut wire, SystemTime::now())?;
        }
        let mut credentials = Redactor::new();
        add_credentials(&mut credentials, wire.headers());
        let url = wire.uri().to_string();
        let started = Instant::now();
        let response = tokio::time::timeout(options.timeout, transport.open(wire))
            .await
            .map_err(|_| RequestError::Timeout {
                after: options.timeout,
                phase: Phase::Response,
            })??;
        let ttfb = started.elapsed();
        add_credentials(&mut credentials, response.headers());
        if let Some(jar) = &options.cookies {
            jar.store(prepared.outgoing.uri(), response.headers())
                .await?;
        }
        let (head, body) = response.into_parts();
        Ok(StreamedResponse {
            url,
            http_version: HttpVersion::from_http(head.version),
            status: head.status.as_u16(),
            headers: head.headers,
            ttfb,
            insecure: !options.tls.verify,
            warnings: prepared
                .warnings
                .iter()
                .map(|w| redactor.text(w).into_owned())
                .collect(),
            credentials,
            body,
            redactor: redactor.clone(),
        })
    }
}

/// The URL and headers `request` would be sent with: auth helpers applied,
/// signed, with the cookies of `options`' jar. For protocols that upgrade
/// an HTTP request, such as a WebSocket handshake. Errors are redacted.
pub async fn request_head(
    request: &ResolvedRequest,
    options: &SendOptions,
) -> Result<(Uri, HeaderMap), RequestError> {
    let head = async {
        let prepared = HttpEngine::prepare(request, None).await?;
        let mut wire = on_the_wire(&prepared.outgoing, options);
        if let Some(signer) = &prepared.signer {
            signer.sign(&mut wire, SystemTime::now())?;
        }
        let (parts, _) = wire.into_parts();
        Ok((parts.uri, parts.headers))
    };
    head.await
        .map_err(|e: RequestError| e.redacted(&Redactor::for_request(request)))
}
