//! Preparing and sending a call (spec "Unary calls", "Streaming calls",
//! "Message timeline", "Headless stream bounds", "Deadlines").
//!
//! [`PreparedCall::new`] does everything that can fail without the network:
//! it finds the method, checks the call type against the transport and the
//! number of messages, encodes every message (unknown fields included) and
//! checks the metadata. Sending then always goes through one bidirectional
//! stream, whatever the call type: the server cannot tell the difference,
//! and response headers and trailers stay apart.

use std::time::{Duration, Instant};

use http::uri::PathAndQuery;
use prost::Message as _;
use prost_reflect::{DynamicMessage, MethodDescriptor};
use rustman_core::Redactor;
use rustman_core::model::{GrpcTransport, StreamLimits};
use rustman_core::run::redact_value;
use rustman_core::trace::SentTrace;
use rustman_core::vars::{ResolvedGrpc, ResolvedRequest};
use rustman_http::SendOptions;
use serde_json::Value;
use std::sync::{Arc, Mutex, PoisonError};
use tokio::sync::mpsc;
use tokio_stream::StreamExt as _;
use tokio_stream::wrappers::ReceiverStream;
use tonic::Streaming;
use tonic::client::Grpc;
use tonic::transport::Channel;

use crate::browse::CallType;
use crate::codec::DynamicCodec;
use crate::connect::{ConnectTimings, Remote};
use crate::error::GrpcError;
use crate::exchange::{Direction, GrpcExchange, GrpcTimings, StreamStop, TimelineEntry};
use crate::json::{to_json, to_message};
use crate::metadata::{MetadataEntry, entries, inject_trace, request_metadata};
use crate::schema::Schema;
use crate::status::{CANCELLED, DEADLINE_EXCEEDED, GrpcStatus};
use crate::web;

/// The deadline when neither the request nor the workspace sets one.
pub const DEFAULT_DEADLINE_MS: u64 = 30_000;

/// The deadline of a call: `deadline_ms` from the request file, else the
/// workspace `[defaults] timeout_ms`, else 30 s.
pub fn effective_deadline(deadline_ms: Option<u64>, workspace_timeout_ms: Option<u64>) -> Duration {
    Duration::from_millis(
        deadline_ms
            .or(workspace_timeout_ms)
            .unwrap_or(DEFAULT_DEADLINE_MS),
    )
}

/// The bounds of a headless stream (`rustman run`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StreamBounds {
    pub max_messages: u32,
    pub duration: Duration,
}

impl StreamBounds {
    /// The bounds of a request's `stream` limits, defaults applied.
    pub fn of(limits: &StreamLimits) -> Self {
        Self {
            max_messages: limits.effective_max_messages(),
            duration: Duration::from_millis(limits.effective_duration_ms()),
        }
    }

    /// The stop due after `received` messages, if any.
    pub(crate) fn reached(&self, received: usize) -> Option<StreamStop> {
        let max = usize::try_from(self.max_messages).unwrap_or(usize::MAX);
        (received >= max).then_some(StreamStop::MaxMessages(self.max_messages))
    }

    pub(crate) fn duration_stop(&self) -> StreamStop {
        let ms = u64::try_from(self.duration.as_millis()).unwrap_or(u64::MAX);
        StreamStop::Duration(ms)
    }
}

/// How a call is sent.
#[derive(Clone, Debug)]
pub struct CallOptions {
    /// Sent as `grpc-timeout` and enforced on the whole call.
    pub deadline: Duration,
    /// Headless bounds of response streams; `None` runs until the server
    /// ends the stream or the deadline expires.
    pub bounds: Option<StreamBounds>,
    /// The HTTP settings of gRPC-Web calls (TLS, proxy, cookies).
    pub http: SendOptions,
}

impl CallOptions {
    pub fn new(deadline: Duration) -> Self {
        Self {
            deadline,
            bounds: None,
            http: SendOptions::default(),
        }
    }
}

/// A call ready to send: every message encoded, the metadata checked.
#[derive(Clone, Debug)]
pub struct PreparedCall {
    pub remote: Remote,
    pub method: MethodDescriptor,
    pub call_type: CallType,
    pub transport: GrpcTransport,
    pub messages: Vec<DynamicMessage>,
    pub metadata: Vec<MetadataEntry>,
    /// The resolved request, for gRPC-Web over `rustman-http` (headers,
    /// secrets, TLS files).
    pub request: ResolvedRequest,
}

impl PreparedCall {
    /// Prepare `resolved` against `schema`, to be sent to `remote`. Fails
    /// before any network traffic on an unknown method or field, a call type
    /// the transport cannot carry, or invalid metadata.
    pub fn new(
        resolved: &ResolvedGrpc,
        schema: &Schema,
        remote: Remote,
    ) -> Result<Self, GrpcError> {
        let method = schema.method(&resolved.method)?;
        let call_type = CallType::of(&method);
        if resolved.transport == GrpcTransport::GrpcWeb {
            web::check_call_type(call_type)?;
        }
        check_message_count(&resolved.method, call_type, resolved.messages.len())?;
        let input = method.input();
        let messages = resolved
            .messages
            .iter()
            .map(|m| to_message(&input, m))
            .collect::<Result<Vec<_>, _>>()?;
        let metadata: Vec<MetadataEntry> = resolved
            .request
            .headers
            .iter()
            .map(|p| MetadataEntry::new(&p.name.text, &p.value.text))
            .collect();
        request_metadata(&metadata)?;
        Ok(Self {
            remote,
            method,
            call_type,
            transport: resolved.transport,
            messages,
            metadata,
            request: resolved.request.clone(),
        })
    }

    /// `package.Service/Method`.
    pub fn path(&self) -> String {
        format!(
            "{}/{}",
            self.method.parent_service().full_name(),
            self.method.name()
        )
    }

    /// Send every message, half-close, and collect the responses.
    pub async fn send(&self, options: &CallOptions) -> Result<GrpcExchange, GrpcError> {
        if self.transport == GrpcTransport::GrpcWeb {
            return web::send(self, options).await;
        }
        let (tx, rx) = mpsc::channel(self.messages.len().max(1));
        for message in &self.messages {
            // The channel holds every message: this cannot fail.
            let _ = tx.try_send(message.clone());
        }
        drop(tx);
        self.send_with(options, rx).await
    }

    /// Send the messages received on `outbound` (the prepared ones are
    /// ignored) and half-close when its sender is dropped: interactive
    /// streaming. Native transport only.
    pub async fn send_with(
        &self,
        options: &CallOptions,
        outbound: mpsc::Receiver<DynamicMessage>,
    ) -> Result<GrpcExchange, GrpcError> {
        self.send_live(options, outbound, None).await
    }

    /// [`PreparedCall::send_with`], with every timeline entry also sent to
    /// `live` as it happens (redacted), for a view of a running stream.
    pub async fn send_live(
        &self,
        options: &CallOptions,
        outbound: mpsc::Receiver<DynamicMessage>,
        live: Option<LiveFeed>,
    ) -> Result<GrpcExchange, GrpcError> {
        let connection = self.remote.connect().await?;
        let recorder = Recorder::new(self.call_type != CallType::Unary).with_live(live);
        let mut state = Collected::default();
        let run = self.exchange(connection.channel, outbound, options, &recorder, &mut state);
        let status = match tokio::time::timeout(options.deadline, run).await {
            Ok(result) => result?,
            Err(_) => deadline_status(options.deadline),
        };
        Ok(self.finish(state, status, &recorder, connection.timings))
    }

    async fn exchange(
        &self,
        channel: Channel,
        outbound: mpsc::Receiver<DynamicMessage>,
        options: &CallOptions,
        recorder: &Recorder,
        state: &mut Collected,
    ) -> Result<GrpcStatus, GrpcError> {
        let mut grpc = Grpc::new(channel);
        grpc.ready().await.map_err(|e| self.transport_error(&e))?;
        let sent = recorder.clone();
        let stream = ReceiverStream::new(outbound).map(move |m| {
            sent.record(Direction::Sent, &m);
            m
        });
        let mut request = tonic::Request::new(stream);
        *request.metadata_mut() = request_metadata(&self.metadata)?;
        if let Some(propagation) = &options.http.trace {
            state.trace = inject_trace(request.metadata_mut(), propagation);
        }
        request.set_timeout(options.deadline);
        let path = PathAndQuery::try_from(format!("/{}", self.path()))
            .map_err(|e| GrpcError::Message(format!("invalid method path: {e}")))?;
        let codec = DynamicCodec::new(self.method.output());
        let response = match grpc.streaming(request, path, codec).await {
            Ok(response) => response,
            Err(status) => {
                state.trailers = entries(status.metadata());
                return Ok(status_of(&status, options.deadline, recorder));
            }
        };
        state.first_byte = Some(recorder.elapsed());
        state.headers = entries(response.metadata());
        let mut inbound = response.into_inner();
        let bounds = options.bounds.filter(|_| self.streams_responses());
        match receive(&mut inbound, bounds, recorder, state).await {
            Received::Ended => {
                if let Ok(Some(trailers)) = inbound.trailers().await {
                    state.trailers = entries(&trailers);
                }
                Ok(GrpcStatus::new(0, ""))
            }
            Received::Failed(status) => {
                state.trailers = entries(status.metadata());
                Ok(status_of(&status, options.deadline, recorder))
            }
            Received::Stopped(stop) => {
                state.stopped = Some(stop);
                Ok(GrpcStatus::new(CANCELLED, stop.message()))
            }
        }
    }

    pub(crate) fn streams_responses(&self) -> bool {
        matches!(self.call_type, CallType::ServerStreaming | CallType::Bidi)
    }

    fn transport_error(&self, error: &tonic::transport::Error) -> GrpcError {
        GrpcError::Transport {
            target: self.remote.target.text.clone(),
            message: error.to_string(),
        }
    }

    /// The exchange of a call that ended with `status`.
    pub(crate) fn finish(
        &self,
        state: Collected,
        status: GrpcStatus,
        recorder: &Recorder,
        connect: ConnectTimings,
    ) -> GrpcExchange {
        GrpcExchange {
            protocol: "grpc",
            target: self.remote.target.text.clone(),
            method: self.path(),
            call_type: self.call_type,
            transport: match self.transport {
                GrpcTransport::Grpc => "grpc",
                GrpcTransport::GrpcWeb => "grpc-web",
            },
            status,
            headers: state.headers,
            trailers: state.trailers,
            messages: recorder.received(),
            timeline: recorder.timeline(),
            timings: GrpcTimings {
                connect: connect.connect,
                tls: connect.tls,
                first_byte: state.first_byte,
                total: recorder.elapsed(),
            },
            stopped: state.stopped,
            trace: state.trace,
        }
    }
}

/// Unary and server-streaming methods take exactly one message.
fn check_message_count(path: &str, call_type: CallType, count: usize) -> Result<(), GrpcError> {
    let single = matches!(call_type, CallType::Unary | CallType::ServerStreaming);
    if single && count != 1 {
        return Err(GrpcError::Message(format!(
            "{path} is a {call_type} method and takes exactly one message, got {count}"
        )));
    }
    Ok(())
}

/// What a call collected besides its messages.
#[derive(Debug, Default)]
pub(crate) struct Collected {
    pub headers: Vec<MetadataEntry>,
    pub trailers: Vec<MetadataEntry>,
    pub first_byte: Option<Duration>,
    pub stopped: Option<StreamStop>,
    /// The trace context the call carried (metadata, or gRPC-Web headers).
    pub trace: Option<SentTrace>,
}

/// The status of an expired deadline.
pub(crate) fn deadline_status(deadline: Duration) -> GrpcStatus {
    GrpcStatus::new(
        DEADLINE_EXCEEDED,
        format!("deadline of {} ms exceeded", deadline.as_millis()),
    )
}

/// The status of a failed tonic call. tonic's own `grpc-timeout` timer
/// reports `CANCELLED`; past the deadline that is `DEADLINE_EXCEEDED`.
fn status_of(status: &tonic::Status, deadline: Duration, recorder: &Recorder) -> GrpcStatus {
    let expired = status.code() == tonic::Code::Cancelled && recorder.elapsed() >= deadline;
    if expired {
        return deadline_status(deadline);
    }
    GrpcStatus::from_tonic(status)
}

enum Received {
    /// The server ended the stream with `OK`.
    Ended,
    Failed(tonic::Status),
    Stopped(StreamStop),
}

/// Receive until the stream ends, fails or reaches `bounds`.
async fn receive(
    inbound: &mut Streaming<DynamicMessage>,
    bounds: Option<StreamBounds>,
    recorder: &Recorder,
    state: &mut Collected,
) -> Received {
    let mut received = 0;
    loop {
        if let Some(stop) = bounds.and_then(|b| b.reached(received)) {
            return Received::Stopped(stop);
        }
        let next = match bounds {
            Some(b) => {
                let until = recorder.started + b.duration;
                match tokio::time::timeout_at(until.into(), inbound.message()).await {
                    Ok(next) => next,
                    Err(_) => return Received::Stopped(b.duration_stop()),
                }
            }
            None => inbound.message().await,
        };
        match next {
            Ok(Some(message)) => {
                state.first_byte.get_or_insert_with(|| recorder.elapsed());
                recorder.record(Direction::Received, &message);
                received += 1;
            }
            Ok(None) => return Received::Ended,
            Err(status) => return Received::Failed(status),
        }
    }
}

/// Where a running call's timeline entries go as they happen, with the
/// secrets of the request replaced first.
#[derive(Clone, Debug)]
pub struct LiveFeed {
    pub entries: mpsc::UnboundedSender<TimelineEntry>,
    pub redactor: Redactor,
}

impl LiveFeed {
    fn push(&self, entry: &TimelineEntry) {
        let redacted = TimelineEntry {
            message: redact_value(&entry.message, &self.redactor),
            ..entry.clone()
        };
        // The viewer may be gone: the call goes on without it.
        let _ = self.entries.send(redacted);
    }
}

/// Records sent and received messages as they pass, with their time since
/// the call started.
#[derive(Clone, Debug)]
pub(crate) struct Recorder {
    pub started: Instant,
    /// Keep a timeline (streaming calls).
    timeline: bool,
    entries: Arc<Mutex<Vec<TimelineEntry>>>,
    live: Option<LiveFeed>,
}

impl Recorder {
    pub fn new(timeline: bool) -> Self {
        Self {
            started: Instant::now(),
            timeline,
            entries: Arc::default(),
            live: None,
        }
    }

    #[must_use]
    pub fn with_live(mut self, live: Option<LiveFeed>) -> Self {
        self.live = live;
        self
    }

    pub fn elapsed(&self) -> Duration {
        self.started.elapsed()
    }

    pub fn record(&self, direction: Direction, message: &DynamicMessage) {
        let entry = TimelineEntry {
            direction,
            at: self.elapsed(),
            size_bytes: message.encoded_len(),
            message: to_json(message),
        };
        if let Some(live) = &self.live {
            live.push(&entry);
        }
        self.entries
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(entry);
    }

    /// The received messages, in order.
    pub fn received(&self) -> Vec<Value> {
        self.entries
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .iter()
            .filter(|e| e.direction == Direction::Received)
            .map(|e| e.message.clone())
            .collect()
    }

    /// The timeline of a streaming call; empty for unary calls.
    pub fn timeline(&self) -> Vec<TimelineEntry> {
        if !self.timeline {
            return Vec::new();
        }
        self.entries
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_deadline_chain() {
        assert_eq!(
            effective_deadline(Some(200), Some(5_000)),
            Duration::from_millis(200)
        );
        assert_eq!(
            effective_deadline(None, Some(5_000)),
            Duration::from_millis(5_000)
        );
        assert_eq!(
            effective_deadline(None, None),
            Duration::from_millis(30_000)
        );
    }

    #[test]
    fn bounds_default_and_stop() {
        let bounds = StreamBounds::of(&StreamLimits::default());
        assert_eq!(bounds.max_messages, 100);
        assert_eq!(bounds.duration, Duration::from_secs(30));
        assert_eq!(bounds.reached(99), None);
        assert_eq!(bounds.reached(100), Some(StreamStop::MaxMessages(100)));
        assert_eq!(bounds.duration_stop(), StreamStop::Duration(30_000));
    }

    #[test]
    fn single_message_methods() {
        assert!(check_message_count("a.S/M", CallType::Unary, 1).is_ok());
        assert!(check_message_count("a.S/M", CallType::Bidi, 0).is_ok());
        let err = check_message_count("a.S/M", CallType::ServerStreaming, 2).unwrap_err();
        assert_eq!(
            err.to_string(),
            "grpc: a.S/M is a server streaming method and takes exactly one message, got 2"
        );
    }
}
