//! The legacy HTTP+SSE transport (protocol revision 2024-11-05), used when
//! a server rejects the Streamable HTTP `initialize` POST with a 4xx other
//! than 401 (design D2). rmcp 3.10 has no client for it, so this is a thin
//! [`Transport`]: GET the server URL as an event stream, wait for the
//! `endpoint` event, then POST every message to that endpoint and read the
//! answers as `message` events of the stream.

use std::future::Future;

use futures_util::StreamExt;
use futures_util::stream::BoxStream;
use rmcp::RoleClient;
use rmcp::model::{ClientJsonRpcMessage, ServerJsonRpcMessage};
use rmcp::transport::Transport;
use rmcp::transport::common::http_header::{EVENT_STREAM_MIME_TYPE, JSON_MIME_TYPE};
use rmcp::transport::streamable_http_client::SseError;
use rustman_core::vars::ResolvedBody;
use sse_stream::Sse;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

use super::http::{EngineClient, HttpError, read_text, sse_events};
use super::sse_limit::MAX_SSE_EVENT;

/// Messages read ahead of the session.
const QUEUE: usize = 64;

/// The most of an error body shown.
const MAX_ERROR_BODY: usize = 4 * 1024;

pub(crate) struct LegacySse {
    client: EngineClient,
    endpoint: String,
    incoming: mpsc::Receiver<ServerJsonRpcMessage>,
    reader: JoinHandle<()>,
}

impl LegacySse {
    /// Open the event stream at `url` and wait for its `endpoint` event.
    pub async fn connect(client: EngineClient, url: &str) -> Result<Self, HttpError> {
        let headers = vec![("Accept".to_owned(), EVENT_STREAM_MIME_TYPE.to_owned())];
        let request = client.request("GET", url, headers, ResolvedBody::None);
        let response = client.open(&request).await?;
        if !(200..300).contains(&response.status) {
            let status = response.status;
            return Err(client.error(&format!(
                "legacy SSE: HTTP {status} opening the event stream"
            )));
        }
        let mut events = sse_events(response, MAX_SSE_EVENT);
        let endpoint = endpoint(&mut events, url).await.ok_or_else(|| {
            client.error("legacy SSE: the stream ended without an endpoint event")
        })?;
        let (tx, incoming) = mpsc::channel(QUEUE);
        let reader = tokio::spawn(forward(events, tx));
        Ok(Self {
            client,
            endpoint,
            incoming,
            reader,
        })
    }
}

/// The URL of the first `endpoint` event, resolved against `base`.
async fn endpoint(
    events: &mut BoxStream<'static, Result<Sse, SseError>>,
    base: &str,
) -> Option<String> {
    while let Some(event) = events.next().await {
        let event = event.ok()?;
        if event.event.as_deref() == Some("endpoint") {
            let data = event.data.unwrap_or_default();
            let joined = url::Url::parse(base).ok()?.join(data.trim()).ok()?;
            return Some(joined.to_string());
        }
    }
    None
}

/// Forward every `message` event that is a JSON-RPC message; others are
/// skipped. Stops at the end of the stream or when the session is gone.
async fn forward(
    mut events: BoxStream<'static, Result<Sse, SseError>>,
    tx: mpsc::Sender<ServerJsonRpcMessage>,
) {
    while let Some(Ok(event)) = events.next().await {
        if !matches!(event.event.as_deref(), None | Some("message")) {
            continue;
        }
        let Some(data) = event.data else { continue };
        let Ok(message) = serde_json::from_str::<ServerJsonRpcMessage>(&data) else {
            continue;
        };
        if tx.send(message).await.is_err() {
            return;
        }
    }
}

impl Transport<RoleClient> for LegacySse {
    type Error = HttpError;

    fn send(
        &mut self,
        item: ClientJsonRpcMessage,
    ) -> impl Future<Output = Result<(), HttpError>> + Send + 'static {
        let client = self.client.clone();
        let endpoint = self.endpoint.clone();
        async move {
            let body = serde_json::to_value(&item).map_err(|e| client.error(&e.to_string()))?;
            let headers = vec![("Accept".to_owned(), JSON_MIME_TYPE.to_owned())];
            let request = client.request("POST", &endpoint, headers, ResolvedBody::Json(body));
            let mut response = client.open(&request).await?;
            if (200..300).contains(&response.status) {
                return Ok(());
            }
            let text = read_text(&mut response, MAX_ERROR_BODY)
                .await
                .unwrap_or_default();
            let status = response.status;
            Err(client.error(&format!("legacy SSE: HTTP {status}: {text}")))
        }
    }

    fn receive(&mut self) -> impl Future<Output = Option<ServerJsonRpcMessage>> + Send {
        self.incoming.recv()
    }

    async fn close(&mut self) -> Result<(), HttpError> {
        self.reader.abort();
        self.incoming.close();
        Ok(())
    }
}

impl Drop for LegacySse {
    fn drop(&mut self) {
        self.reader.abort();
    }
}
