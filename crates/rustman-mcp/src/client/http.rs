//! Streamable HTTP over `rustman-http` (mcp-client spec "Streamable HTTP
//! transport with legacy fallback"). rmcp's transport worker drives the
//! protocol (`Mcp-Session-Id`, SSE response streams, the GET stream, the
//! closing DELETE); this module sends its requests through the
//! [`HttpEngine`], so TLS settings, proxies and the definition's headers and
//! auth apply as for any request. Every error message is redacted.

use std::borrow::Cow;
use std::collections::HashMap;
use std::sync::{Arc, Mutex, PoisonError};

use bytes::Bytes;
use futures_util::stream::{self, BoxStream};
use http::{HeaderName, HeaderValue};
use rmcp::model::{ClientJsonRpcMessage, ClientRequest, JsonRpcMessage, ServerJsonRpcMessage};
use rmcp::transport::common::http_header::{
    EVENT_STREAM_MIME_TYPE, HEADER_LAST_EVENT_ID, HEADER_SESSION_ID, JSON_MIME_TYPE,
};
use rmcp::transport::streamable_http_client::{
    AuthRequiredError, SseError, StreamableHttpClient, StreamableHttpError,
    StreamableHttpPostResponse,
};
use rustman_core::model::RequestSettings;
use rustman_core::vars::{
    Resolved, ResolvedBody, ResolvedPair, ResolvedRequest, TlsFiles, UsedSecret,
};
use rustman_http::{HttpEngine, SendOptions, StreamedResponse};
use sse_stream::Sse;

use super::oauth::Authorizer;
use super::resolve::ResolvedHttp;
use super::shared::SharedRedactor;
use super::sse_limit::{MAX_SSE_EVENT, limited_events};

/// The most of an error or JSON body read into memory.
const MAX_ERROR_BODY: usize = 64 * 1024;

/// A transport failure, already redacted.
#[derive(Debug, thiserror::Error)]
#[error("{0}")]
pub struct HttpError(String);

impl From<String> for HttpError {
    fn from(message: String) -> Self {
        Self(message)
    }
}

type HttpResult<T> = Result<T, StreamableHttpError<HttpError>>;

/// Sends MCP's HTTP requests with the definition's headers and auth.
/// Clones share the engine and what they recorded.
#[derive(Clone)]
pub(crate) struct EngineClient {
    inner: Arc<Inner>,
}

struct Inner {
    engine: Arc<HttpEngine>,
    options: SendOptions,
    server: ResolvedHttp,
    secrets: Vec<UsedSecret>,
    redactor: SharedRedactor,
    /// Adds the OAuth access token to every request.
    oauth: Option<Arc<Authorizer>>,
    /// The status answered to the POST carrying `initialize`.
    initialize_status: Mutex<Option<u16>>,
    /// The `WWW-Authenticate` challenge of the last `401`.
    challenge: Mutex<Option<String>>,
}

impl EngineClient {
    pub fn new(
        engine: Arc<HttpEngine>,
        options: SendOptions,
        server: ResolvedHttp,
        secrets: Vec<UsedSecret>,
        redactor: SharedRedactor,
        oauth: Option<Arc<Authorizer>>,
    ) -> Self {
        Self {
            inner: Arc::new(Inner {
                engine,
                options,
                server,
                secrets,
                redactor,
                oauth,
                initialize_status: Mutex::new(None),
                challenge: Mutex::new(None),
            }),
        }
    }

    /// The challenge of the last `401`, when there was one.
    pub fn challenge(&self) -> Option<String> {
        self.inner
            .challenge
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    pub fn authorizer(&self) -> Option<&Arc<Authorizer>> {
        self.inner.oauth.as_ref()
    }

    /// The HTTP status of the `initialize` POST, once sent.
    pub fn initialize_status(&self) -> Option<u16> {
        *self
            .inner
            .initialize_status
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }

    /// A request to `uri` with the definition's headers and auth, then
    /// `headers`, uncompressed so streams arrive as sent.
    pub fn request(
        &self,
        method: &str,
        uri: &str,
        headers: Vec<(String, String)>,
        body: ResolvedBody,
    ) -> ResolvedRequest {
        let pair = |name: &str, value: &str| ResolvedPair {
            name: Resolved::literal(name),
            value: Resolved::literal(value),
        };
        let mut all = self.inner.server.headers.clone();
        all.extend(headers.iter().map(|(n, v)| pair(n, v)));
        all.push(pair("Accept-Encoding", "identity"));
        ResolvedRequest {
            method: method.to_owned(),
            url: Resolved::literal(uri),
            query: Vec::new(),
            headers: all,
            body,
            auth: self.inner.server.auth.clone(),
            settings: RequestSettings::default(),
            tls: TlsFiles::default(),
            secrets: self.inner.secrets.clone(),
        }
    }

    /// Send `request` (with the OAuth access token, when there is one),
    /// returning when the response head arrives.
    pub async fn open(&self, request: &ResolvedRequest) -> Result<StreamedResponse, HttpError> {
        let mut request = request.clone();
        if let Some(oauth) = &self.inner.oauth
            && let Some(token) = oauth
                .bearer()
                .await
                .map_err(|e| self.error(&e.to_string()))?
        {
            request.headers.push(ResolvedPair {
                name: Resolved::literal("Authorization"),
                value: Resolved::literal(format!("Bearer {}", token.expose())),
            });
            request.secrets.push(token);
        }
        let response = self
            .inner
            .engine
            .open_stream(&request, &self.inner.options)
            .await
            .map_err(|e| self.error(&e.to_string()))?;
        if response.status == 401 {
            let challenge = response.header("www-authenticate").unwrap_or_default();
            *self
                .inner
                .challenge
                .lock()
                .unwrap_or_else(PoisonError::into_inner) = Some(challenge.to_owned());
        }
        Ok(response)
    }

    pub fn error(&self, message: &str) -> HttpError {
        HttpError(self.inner.redactor.text(message))
    }

    fn record_initialize(&self, message: &ClientJsonRpcMessage, status: u16) {
        if let JsonRpcMessage::Request(request) = message
            && matches!(request.request, ClientRequest::InitializeRequest(_))
        {
            *self
                .inner
                .initialize_status
                .lock()
                .unwrap_or_else(PoisonError::into_inner) = Some(status);
        }
    }
}

/// Header pairs for rmcp's session id, auth and custom headers.
fn protocol_headers(
    session_id: Option<&str>,
    auth_header: Option<String>,
    custom: HashMap<HeaderName, HeaderValue>,
) -> Vec<(String, String)> {
    let mut headers = Vec::new();
    if let Some(id) = session_id {
        headers.push((HEADER_SESSION_ID.to_owned(), id.to_owned()));
    }
    if let Some(token) = auth_header {
        headers.push(("Authorization".to_owned(), format!("Bearer {token}")));
    }
    for (name, value) in custom {
        let value = String::from_utf8_lossy(value.as_bytes()).into_owned();
        headers.push((name.as_str().to_owned(), value));
    }
    headers
}

/// The body of `response` as SSE events of at most `max_event` bytes each.
pub(crate) fn sse_events(
    response: StreamedResponse,
    max_event: usize,
) -> BoxStream<'static, Result<Sse, SseError>> {
    let chunks = stream::unfold(response, |mut response| async move {
        let chunk = response.chunk().await?;
        Some((chunk.map_err(|e| HttpError(e.to_string())), response))
    });
    limited_events(chunks, max_event)
}

/// Up to [`MAX_ERROR_BODY`] bytes of the body, as text.
pub(crate) async fn read_text(
    response: &mut StreamedResponse,
    limit: usize,
) -> Result<String, HttpError> {
    let mut body = Vec::new();
    while let Some(chunk) = response.chunk().await {
        let chunk: Bytes = chunk.map_err(|e| HttpError(e.to_string()))?;
        body.extend_from_slice(&chunk);
        if body.len() > limit {
            return Err(HttpError(format!(
                "response body larger than {limit} bytes"
            )));
        }
    }
    Ok(String::from_utf8_lossy(&body).into_owned())
}

fn media_type(response: &StreamedResponse) -> String {
    let media = response.header("content-type").unwrap_or_default();
    media
        .split(';')
        .next()
        .unwrap_or_default()
        .trim()
        .to_ascii_lowercase()
}

fn unexpected(client: &EngineClient, message: String) -> StreamableHttpError<HttpError> {
    let redacted = client.inner.redactor.text(&message);
    StreamableHttpError::UnexpectedServerResponse(Cow::Owned(redacted))
}

/// A `401` asking for authorization, as rmcp reports it.
fn auth_required(response: &StreamedResponse) -> Option<StreamableHttpError<HttpError>> {
    let challenge = response.header("www-authenticate")?;
    (response.status == 401)
        .then(|| StreamableHttpError::AuthRequired(AuthRequiredError::new(challenge.to_owned())))
}

/// A failed POST: a JSON-RPC error in the body is the answer, anything
/// else fails the transport with the status and the start of the body.
async fn failed_post(
    client: &EngineClient,
    mut response: StreamedResponse,
    session_attached: bool,
) -> HttpResult<StreamableHttpPostResponse> {
    if let Some(error) = auth_required(&response) {
        return Err(error);
    }
    if response.status == 404 && session_attached {
        return Err(StreamableHttpError::SessionExpired);
    }
    let session = response.header(HEADER_SESSION_ID).map(str::to_owned);
    let json = media_type(&response) == JSON_MIME_TYPE;
    let body = read_text(&mut response, MAX_ERROR_BODY)
        .await
        .unwrap_or_default();
    if json
        && let Ok(message @ JsonRpcMessage::Error(_)) =
            serde_json::from_str::<ServerJsonRpcMessage>(&body)
    {
        return Ok(StreamableHttpPostResponse::Json(message, session));
    }
    Err(unexpected(
        client,
        format!("HTTP {}: {body}", response.status),
    ))
}

/// A successful POST: an event stream, a JSON message, or nothing.
async fn answered_post(
    client: &EngineClient,
    mut response: StreamedResponse,
    expects_reply: bool,
    max_event: usize,
) -> HttpResult<StreamableHttpPostResponse> {
    let session = response.header(HEADER_SESSION_ID).map(str::to_owned);
    let media = media_type(&response);
    if media == EVENT_STREAM_MIME_TYPE {
        return Ok(StreamableHttpPostResponse::Sse(
            sse_events(response, max_event),
            session,
        ));
    }
    let limit = client
        .inner
        .options
        .body
        .max_in_memory
        .try_into()
        .unwrap_or(usize::MAX);
    let body = read_text(&mut response, limit)
        .await
        .map_err(StreamableHttpError::Client)?;
    if !expects_reply && body.trim().is_empty() {
        return Ok(StreamableHttpPostResponse::Accepted);
    }
    if media != JSON_MIME_TYPE {
        return Err(StreamableHttpError::UnexpectedContentType(Some(media)));
    }
    let message = serde_json::from_str::<ServerJsonRpcMessage>(&body)?;
    Ok(StreamableHttpPostResponse::Json(message, session))
}

impl StreamableHttpClient for EngineClient {
    type Error = HttpError;

    async fn post_message(
        &self,
        uri: Arc<str>,
        message: ClientJsonRpcMessage,
        session_id: Option<Arc<str>>,
        auth_header: Option<String>,
        custom_headers: HashMap<HeaderName, HeaderValue>,
    ) -> HttpResult<StreamableHttpPostResponse> {
        self.post_message_with_max_sse_event_size(
            uri,
            message,
            session_id,
            auth_header,
            custom_headers,
            MAX_SSE_EVENT,
        )
        .await
    }

    async fn post_message_with_max_sse_event_size(
        &self,
        uri: Arc<str>,
        message: ClientJsonRpcMessage,
        session_id: Option<Arc<str>>,
        auth_header: Option<String>,
        custom_headers: HashMap<HeaderName, HeaderValue>,
        max_sse_event_size: usize,
    ) -> HttpResult<StreamableHttpPostResponse> {
        let mut headers = protocol_headers(session_id.as_deref(), auth_header, custom_headers);
        let accept = format!("{JSON_MIME_TYPE}, {EVENT_STREAM_MIME_TYPE}");
        headers.insert(0, ("Accept".to_owned(), accept));
        let body = serde_json::to_value(&message)?;
        let request = self.request("POST", &uri, headers, ResolvedBody::Json(body));
        let response = self
            .open(&request)
            .await
            .map_err(StreamableHttpError::Client)?;
        self.record_initialize(&message, response.status);
        match response.status {
            202 | 204 => Ok(StreamableHttpPostResponse::Accepted),
            200..=299 => {
                let expects_reply = matches!(message, JsonRpcMessage::Request(_));
                answered_post(self, response, expects_reply, max_sse_event_size).await
            }
            _ => failed_post(self, response, session_id.is_some()).await,
        }
    }

    async fn delete_session(
        &self,
        uri: Arc<str>,
        session_id: Arc<str>,
        auth_header: Option<String>,
        custom_headers: HashMap<HeaderName, HeaderValue>,
    ) -> HttpResult<()> {
        let headers = protocol_headers(Some(&session_id), auth_header, custom_headers);
        let request = self.request("DELETE", &uri, headers, ResolvedBody::None);
        let response = self
            .open(&request)
            .await
            .map_err(StreamableHttpError::Client)?;
        match response.status {
            200..=299 | 405 => Ok(()),
            status => Err(unexpected(
                self,
                format!("HTTP {status} closing the session"),
            )),
        }
    }

    async fn get_stream(
        &self,
        uri: Arc<str>,
        session_id: Option<Arc<str>>,
        last_event_id: Option<String>,
        auth_header: Option<String>,
        custom_headers: HashMap<HeaderName, HeaderValue>,
    ) -> HttpResult<BoxStream<'static, Result<Sse, SseError>>> {
        self.get_stream_with_max_sse_event_size(
            uri,
            session_id,
            last_event_id,
            auth_header,
            custom_headers,
            MAX_SSE_EVENT,
        )
        .await
    }

    async fn get_stream_with_max_sse_event_size(
        &self,
        uri: Arc<str>,
        session_id: Option<Arc<str>>,
        last_event_id: Option<String>,
        auth_header: Option<String>,
        custom_headers: HashMap<HeaderName, HeaderValue>,
        max_sse_event_size: usize,
    ) -> HttpResult<BoxStream<'static, Result<Sse, SseError>>> {
        let mut headers = protocol_headers(session_id.as_deref(), auth_header, custom_headers);
        headers.insert(0, ("Accept".to_owned(), EVENT_STREAM_MIME_TYPE.to_owned()));
        if let Some(id) = last_event_id {
            headers.push((HEADER_LAST_EVENT_ID.to_owned(), id));
        }
        let request = self.request("GET", &uri, headers, ResolvedBody::None);
        let response = self
            .open(&request)
            .await
            .map_err(StreamableHttpError::Client)?;
        if response.status == 405 {
            return Err(StreamableHttpError::ServerDoesNotSupportSse);
        }
        if let Some(error) = auth_required(&response) {
            return Err(error);
        }
        if !(200..300).contains(&response.status) || media_type(&response) != EVENT_STREAM_MIME_TYPE
        {
            let status = response.status;
            return Err(unexpected(
                self,
                format!("HTTP {status} opening the event stream"),
            ));
        }
        Ok(sse_events(response, max_sse_event_size))
    }
}
