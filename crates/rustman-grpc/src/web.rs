//! gRPC-Web (spec "gRPC-Web", design D6): plain HTTP framing over
//! `rustman-http`, so proxies, TLS settings and cookies apply as for HTTP
//! requests. Each message is a 5-byte prefix (flag, big-endian length) plus
//! the encoded message; the server's trailers come last in the body, in a
//! frame whose flag has bit `0x80` set. Only unary and server-streaming
//! calls fit the protocol.

use std::time::Duration;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use bytes::{Buf as _, BufMut as _, Bytes, BytesMut};
use http::HeaderMap;
use prost::Message as _;
use prost_reflect::{DynamicMessage, MessageDescriptor};
use rustman_core::vars::{Resolved, ResolvedPair, ResolvedRequest};
use rustman_http::{HttpEngine, RequestError, StreamedResponse};

use crate::browse::CallType;
use crate::call::{CallOptions, Collected, PreparedCall, Recorder, StreamBounds, deadline_status};
use crate::connect::ConnectTimings;
use crate::error::GrpcError;
use crate::exchange::{Direction, GrpcExchange};
use crate::metadata::{MetadataEntry, decode_binary, header_entries, is_binary};
use crate::status::{
    CANCELLED, GrpcStatus, INTERNAL, PERMISSION_DENIED, RESOURCE_EXHAUSTED, UNAUTHENTICATED,
    UNAVAILABLE, UNIMPLEMENTED, UNKNOWN,
};

/// The content type of gRPC-Web requests.
pub const CONTENT_TYPE: &str = "application/grpc-web+proto";

/// The flag of a trailers frame.
const TRAILERS_FLAG: u8 = 0x80;

/// The flag of a compressed message frame.
const COMPRESSED_FLAG: u8 = 0x01;

/// The most digits a `grpc-timeout` value may have.
const MAX_TIMEOUT_VALUE: u128 = 99_999_999;

/// How much later than the deadline the HTTP engine's own timeout fires,
/// so an expired deadline is always the status `DEADLINE_EXCEEDED`.
const HTTP_TIMEOUT_MARGIN: Duration = Duration::from_secs(1);

/// Client streaming and bidi cannot be carried by gRPC-Web.
pub fn check_call_type(call_type: CallType) -> Result<(), GrpcError> {
    match call_type {
        CallType::Unary | CallType::ServerStreaming => Ok(()),
        CallType::ClientStreaming | CallType::Bidi => Err(GrpcError::Unsupported(
            "grpc-web supports only unary and server-streaming calls".into(),
        )),
    }
}

/// `message` framed for the request body.
pub fn frame(message: &[u8]) -> Bytes {
    let mut body = BytesMut::with_capacity(message.len() + 5);
    body.put_u8(0);
    body.put_u32(u32::try_from(message.len()).unwrap_or(u32::MAX));
    body.put_slice(message);
    body.freeze()
}

/// One frame of a response body.
#[derive(Debug, PartialEq, Eq)]
pub enum Frame {
    Message(Bytes),
    /// A compressed message: no compression is negotiated, so it is refused.
    Compressed,
    Trailers(Vec<MetadataEntry>),
}

/// A frame longer than the reader's limit.
#[derive(Debug, PartialEq, Eq)]
pub struct FrameTooLarge {
    pub len: u64,
    pub limit: u64,
}

impl FrameTooLarge {
    /// `RESOURCE_EXHAUSTED`, as a native client reports a message over its
    /// receive limit.
    pub fn status(&self) -> GrpcStatus {
        GrpcStatus::new(
            RESOURCE_EXHAUSTED,
            format!(
                "grpc-web frame of {} bytes exceeds the limit of {} bytes ([http] max_body_bytes)",
                self.len, self.limit
            ),
        )
    }
}

/// Splits a response body into frames as its chunks arrive. A frame
/// longer than `limit` is refused as soon as its prefix arrives, so a
/// server cannot make the client buffer more than one frame of that size.
#[derive(Debug)]
pub struct FrameReader {
    buffer: BytesMut,
    limit: u64,
}

impl FrameReader {
    pub fn new(limit: u64) -> Self {
        Self {
            buffer: BytesMut::new(),
            limit,
        }
    }

    pub fn push(&mut self, chunk: &[u8]) {
        self.buffer.extend_from_slice(chunk);
    }

    /// The next complete frame, if one has arrived.
    pub fn next_frame(&mut self) -> Result<Option<Frame>, FrameTooLarge> {
        if self.buffer.len() < 5 {
            return Ok(None);
        }
        let flag = self.buffer[0];
        let len = u32::from_be_bytes([
            self.buffer[1],
            self.buffer[2],
            self.buffer[3],
            self.buffer[4],
        ]);
        if u64::from(len) > self.limit {
            return Err(FrameTooLarge {
                len: len.into(),
                limit: self.limit,
            });
        }
        let Ok(len) = usize::try_from(len) else {
            return Ok(None);
        };
        if self.buffer.len() < 5 + len {
            return Ok(None);
        }
        self.buffer.advance(5);
        let payload = self.buffer.split_to(len).freeze();
        if flag & TRAILERS_FLAG != 0 {
            return Ok(Some(Frame::Trailers(parse_trailers(&payload))));
        }
        if flag & COMPRESSED_FLAG != 0 {
            return Ok(Some(Frame::Compressed));
        }
        Ok(Some(Frame::Message(payload)))
    }

    /// Bytes of an incomplete frame are left over.
    pub fn has_leftover(&self) -> bool {
        !self.buffer.is_empty()
    }
}

/// The `name: value` lines of a trailers frame, names lowercased. Lines
/// end with `\r\n`, or a bare `\n` from lenient servers and proxies.
fn parse_trailers(block: &[u8]) -> Vec<MetadataEntry> {
    String::from_utf8_lossy(block)
        .split('\n')
        .filter_map(|line| line.split_once(':'))
        .map(|(name, value)| MetadataEntry::new(name.trim().to_ascii_lowercase(), value.trim()))
        .collect()
}

/// The status in `entries` (`grpc-status`, `grpc-message`,
/// `grpc-status-details-bin`), if they carry one.
pub fn status_from(entries: &[MetadataEntry]) -> Option<GrpcStatus> {
    let get = |name: &str| {
        entries
            .iter()
            .find(|e| e.name == name)
            .map(|e| e.value.as_str())
    };
    let code = get("grpc-status")?.trim().parse::<i32>().ok()?;
    let message = get("grpc-message")
        .map(|m| {
            percent_encoding::percent_decode_str(m)
                .decode_utf8_lossy()
                .into_owned()
        })
        .unwrap_or_default();
    let details = get("grpc-status-details-bin")
        .and_then(|d| decode_binary("grpc-status-details-bin", d).ok())
        .unwrap_or_default();
    Some(GrpcStatus::with_details(code, message, &details))
}

/// The gRPC status of an HTTP error answer, per the gRPC HTTP mapping.
pub fn status_of_http(status: u16) -> GrpcStatus {
    let code = match status {
        400 => INTERNAL,
        401 => UNAUTHENTICATED,
        403 => PERMISSION_DENIED,
        404 => UNIMPLEMENTED,
        429 | 502..=504 => UNAVAILABLE,
        _ => UNKNOWN,
    };
    GrpcStatus::new(code, format!("HTTP {status}"))
}

/// Send `call` over gRPC-Web.
pub(crate) async fn send(
    call: &PreparedCall,
    options: &CallOptions,
) -> Result<GrpcExchange, GrpcError> {
    let request = web_request(call, options)?;
    let body = call
        .messages
        .iter()
        .map(|m| frame(&m.encode_to_vec()))
        .fold(BytesMut::new(), |mut all, f| {
            all.extend_from_slice(&f);
            all
        })
        .freeze();
    let recorder = Recorder::new(call.call_type != CallType::Unary);
    for message in &call.messages {
        recorder.record(Direction::Sent, message);
    }
    let mut http = options.http.clone();
    http.timeout = options.deadline.saturating_add(HTTP_TIMEOUT_MARGIN);
    let mut state = Collected::default();
    let run = exchange(call, &request, &http, body, options, &recorder, &mut state);
    let status = match tokio::time::timeout(options.deadline, run).await {
        Ok(result) => past_deadline(result?, &state, options.deadline, recorder.elapsed()),
        Err(_) => deadline_status(options.deadline),
    };
    Ok(call.finish(state, status, &recorder, ConnectTimings::default()))
}

/// `status`, which the call ended with after `elapsed`; `DEADLINE_EXCEEDED`
/// when it is a `CANCELLED` the server sent once the deadline had passed.
/// A server enforcing `grpc-timeout` (tonic's does) answers `CANCELLED`
/// when it expires, and when that answer is ready as our own timer fires,
/// the answer wins the race; it is the same expired deadline, as for
/// native gRPC calls. A `CANCELLED` of our own stream bounds is kept.
fn past_deadline(
    status: GrpcStatus,
    state: &Collected,
    deadline: Duration,
    elapsed: Duration,
) -> GrpcStatus {
    let expired = status.code == CANCELLED && state.stopped.is_none() && elapsed >= deadline;
    if expired {
        return deadline_status(deadline);
    }
    status
}

async fn exchange(
    call: &PreparedCall,
    request: &ResolvedRequest,
    http: &rustman_http::SendOptions,
    body: Bytes,
    options: &CallOptions,
    recorder: &Recorder,
    state: &mut Collected,
) -> Result<GrpcStatus, GrpcError> {
    let engine = HttpEngine::new();
    let mut response = engine
        .open_stream_with_body(request, http, body)
        .await
        .map_err(|e| request_error(call, e, options))?;
    state.trace.clone_from(&response.trace);
    state.first_byte = Some(recorder.elapsed());
    state.headers = header_entries(&response.headers);
    if let Some(status) = trailers_only(&response.headers, state) {
        return Ok(status);
    }
    if response.status != 200 {
        return Ok(status_of_http(response.status));
    }
    let bounds = options.bounds.filter(|_| call.streams_responses());
    let reader = BodyReader {
        output: call.method.output(),
        bounds,
        recorder,
        limit: options.http.body.max_in_memory,
    };
    reader.read(&mut response, state).await
}

/// A `grpc-status` in the response headers: a trailers-only answer.
fn trailers_only(headers: &HeaderMap, state: &mut Collected) -> Option<GrpcStatus> {
    let status = status_from(&header_entries(headers))?;
    state.trailers = std::mem::take(&mut state.headers);
    Some(status)
}

/// Reads the frames of a response body.
struct BodyReader<'a> {
    output: MessageDescriptor,
    bounds: Option<StreamBounds>,
    recorder: &'a Recorder,
    /// The longest frame accepted: `[http] max_body_bytes`.
    limit: u64,
}

impl BodyReader<'_> {
    async fn read(
        &self,
        response: &mut StreamedResponse,
        state: &mut Collected,
    ) -> Result<GrpcStatus, GrpcError> {
        let mut frames = FrameReader::new(self.limit);
        let mut received = 0;
        loop {
            let Some(chunk) = self.next_chunk(response, state).await else {
                return Ok(self.stopped(state));
            };
            let Some(chunk) = chunk else {
                return Ok(ended_early(&frames));
            };
            frames.push(&chunk.map_err(|e| GrpcError::Transport {
                target: response.url.clone(),
                message: e.to_string(),
            })?);
            if let Some(status) = self.drain(&mut frames, state, &mut received)? {
                return Ok(status);
            }
        }
    }

    /// Handle every complete frame; the status once the call is over.
    fn drain(
        &self,
        frames: &mut FrameReader,
        state: &mut Collected,
        received: &mut usize,
    ) -> Result<Option<GrpcStatus>, GrpcError> {
        loop {
            let frame = match frames.next_frame() {
                Ok(Some(frame)) => frame,
                Ok(None) => return Ok(None),
                Err(too_large) => return Ok(Some(too_large.status())),
            };
            if let Some(status) = self.on_frame(frame, state, received)? {
                return Ok(Some(status));
            }
        }
    }

    /// The next body chunk, or `None` when the duration bound elapsed.
    async fn next_chunk(
        &self,
        response: &mut StreamedResponse,
        state: &mut Collected,
    ) -> Option<Option<Result<Bytes, RequestError>>> {
        let Some(bounds) = self.bounds else {
            return Some(response.chunk().await);
        };
        let until = self.recorder.started + bounds.duration;
        match tokio::time::timeout_at(until.into(), response.chunk()).await {
            Ok(chunk) => Some(chunk),
            Err(_) => {
                state.stopped = Some(bounds.duration_stop());
                None
            }
        }
    }

    fn stopped(&self, state: &Collected) -> GrpcStatus {
        let message = state.stopped.map(|s| s.message()).unwrap_or_default();
        GrpcStatus::new(CANCELLED, message)
    }

    /// Handle one frame; the status once the call is over.
    fn on_frame(
        &self,
        frame: Frame,
        state: &mut Collected,
        received: &mut usize,
    ) -> Result<Option<GrpcStatus>, GrpcError> {
        match frame {
            Frame::Message(bytes) => {
                let message = DynamicMessage::decode(self.output.clone(), bytes).map_err(|e| {
                    GrpcError::Message(format!("cannot decode {}: {e}", self.output.full_name()))
                })?;
                self.recorder.record(Direction::Received, &message);
                *received += 1;
                let stop = self.bounds.and_then(|b| b.reached(*received));
                Ok(stop.map(|stop| {
                    state.stopped = Some(stop);
                    GrpcStatus::new(CANCELLED, stop.message())
                }))
            }
            Frame::Compressed => Ok(Some(GrpcStatus::new(
                UNIMPLEMENTED,
                "grpc-web compressed messages are not supported",
            ))),
            Frame::Trailers(trailers) => {
                let status = status_from(&trailers).unwrap_or_else(|| {
                    GrpcStatus::new(UNKNOWN, "grpc-web trailers without grpc-status")
                });
                state.trailers = trailers;
                Ok(Some(status))
            }
        }
    }
}

/// The status of a body that ended before its trailers.
fn ended_early(frames: &FrameReader) -> GrpcStatus {
    let what = if frames.has_leftover() {
        "grpc-web response ended inside a frame"
    } else {
        "grpc-web response ended without trailers"
    };
    GrpcStatus::new(UNKNOWN, what)
}

/// The HTTP request of a gRPC-Web call: a POST of the framed messages to
/// `/<package.Service>/<Method>`, with the metadata as headers.
fn web_request(call: &PreparedCall, options: &CallOptions) -> Result<ResolvedRequest, GrpcError> {
    let scheme = if call.remote.tls { "https" } else { "http" };
    let url = format!(
        "{scheme}://{}/{}",
        call.remote.target.authority(),
        call.path()
    );
    let mut request = call.request.clone();
    request.method = "POST".to_owned();
    request.url = Resolved::literal(url);
    for header in &mut request.headers {
        if is_binary(&header.name.text) {
            let bytes = decode_binary(&header.name.text, &header.value.text)?;
            header.value.text = STANDARD.encode(bytes);
        }
    }
    let timeout = grpc_timeout(options.deadline);
    let fixed = [
        ("content-type", CONTENT_TYPE.to_owned()),
        ("accept", CONTENT_TYPE.to_owned()),
        ("x-grpc-web", "1".to_owned()),
        ("grpc-timeout", timeout),
    ];
    request
        .headers
        .extend(fixed.into_iter().map(|(name, value)| ResolvedPair {
            name: Resolved::literal(name),
            value: Resolved::literal(value),
        }));
    Ok(request)
}

/// `deadline` as a `grpc-timeout` value of at most eight digits:
/// milliseconds, or whole seconds (capped) when that is too many.
fn grpc_timeout(deadline: Duration) -> String {
    let millis = deadline.as_millis().max(1);
    if millis <= MAX_TIMEOUT_VALUE {
        return format!("{millis}m");
    }
    format!("{}S", deadline.as_secs().min(99_999_999))
}

fn request_error(call: &PreparedCall, error: RequestError, options: &CallOptions) -> GrpcError {
    let target = call.remote.target.text.clone();
    match error {
        RequestError::Tls(message) => GrpcError::Tls { target, message },
        RequestError::Timeout { .. } => GrpcError::Transport {
            target,
            message: format!("deadline of {} ms exceeded", options.deadline.as_millis()),
        },
        other => GrpcError::Transport {
            target,
            message: other.to_string(),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frames_round_trip() {
        let mut reader = FrameReader::new(1024);
        let framed = frame(b"abc");
        assert_eq!(framed.as_ref(), [0, 0, 0, 0, 3, b'a', b'b', b'c']);
        reader.push(&framed[..4]);
        assert_eq!(reader.next_frame(), Ok(None));
        reader.push(&framed[4..]);
        let block = b"grpc-status: 0\r\ngrpc-message: \r\n";
        reader.push(&[0x80, 0, 0, 0, u8::try_from(block.len()).unwrap()]);
        reader.push(block);
        assert_eq!(
            reader.next_frame(),
            Ok(Some(Frame::Message(Bytes::from_static(b"abc"))))
        );
        let Ok(Some(Frame::Trailers(trailers))) = reader.next_frame() else {
            panic!("a trailers frame");
        };
        assert_eq!(status_from(&trailers).unwrap().label(), "OK (0)");
        assert!(!reader.has_leftover());
    }

    #[test]
    fn a_frame_over_the_limit_is_refused_from_its_prefix() {
        let mut reader = FrameReader::new(8);
        reader.push(&frame(b"12345678"));
        assert!(matches!(reader.next_frame(), Ok(Some(Frame::Message(_)))));
        // A declared 4 GiB frame is refused before any of it is buffered.
        reader.push(&[0, 0xff, 0xff, 0xff, 0xff]);
        let too_large = reader.next_frame().unwrap_err();
        assert_eq!(
            too_large,
            FrameTooLarge {
                len: u64::from(u32::MAX),
                limit: 8
            }
        );
        let status = too_large.status();
        assert_eq!(status.label(), "RESOURCE_EXHAUSTED (8)");
        assert!(
            status.message.contains("max_body_bytes"),
            "{}",
            status.message
        );
    }

    #[test]
    fn trailers_with_bare_newlines_keep_the_status() {
        let block = b"grpc-status: 5\ngrpc-message: gone\n";
        let mut reader = FrameReader::new(1024);
        reader.push(&[0x80, 0, 0, 0, u8::try_from(block.len()).unwrap()]);
        reader.push(block);
        let Ok(Some(Frame::Trailers(trailers))) = reader.next_frame() else {
            panic!("a trailers frame");
        };
        assert_eq!(
            status_from(&trailers).unwrap().render(),
            "NOT_FOUND (5): gone"
        );
    }

    #[test]
    fn statuses_from_trailers_and_http() {
        let entries = [
            MetadataEntry::new("grpc-status", "5"),
            MetadataEntry::new("grpc-message", "no%20such%20product"),
        ];
        let status = status_from(&entries).unwrap();
        assert_eq!(status.render(), "NOT_FOUND (5): no such product");
        assert!(status_from(&[]).is_none());
        assert_eq!(status_of_http(404).label(), "UNIMPLEMENTED (12)");
        assert_eq!(status_of_http(503).label(), "UNAVAILABLE (14)");
        assert_eq!(status_of_http(500).label(), "UNKNOWN (2)");
        assert_eq!(status_of_http(400).label(), "INTERNAL (13)");
        assert_eq!(status_of_http(401).label(), "UNAUTHENTICATED (16)");
        assert_eq!(status_of_http(403).label(), "PERMISSION_DENIED (7)");
        assert_eq!(status_of_http(429).label(), "UNAVAILABLE (14)");
    }

    #[test]
    fn only_unary_and_server_streaming() {
        assert!(check_call_type(CallType::Unary).is_ok());
        assert!(check_call_type(CallType::ServerStreaming).is_ok());
        for call_type in [CallType::ClientStreaming, CallType::Bidi] {
            assert_eq!(
                check_call_type(call_type).unwrap_err().to_string(),
                "grpc: grpc-web supports only unary and server-streaming calls"
            );
        }
    }

    #[test]
    fn a_compressed_frame_is_reported_as_such() {
        let mut reader = FrameReader::new(1024);
        reader.push(&[COMPRESSED_FLAG, 0, 0, 0, 2, 1, 2]);
        assert_eq!(reader.next_frame(), Ok(Some(Frame::Compressed)));
        assert!(!reader.has_leftover());
    }

    /// The server's `CANCELLED` for an expired `grpc-timeout` can arrive
    /// as our own deadline fires; either way the status is
    /// `DEADLINE_EXCEEDED`.
    #[test]
    fn a_cancelled_answer_past_the_deadline_is_deadline_exceeded() {
        let deadline = Duration::from_millis(200);
        let cancelled = || GrpcStatus::new(CANCELLED, "Timeout expired");
        let late = past_deadline(cancelled(), &Collected::default(), deadline, deadline);
        assert_eq!(late.label(), "DEADLINE_EXCEEDED (4)");
        assert_eq!(late.message, "deadline of 200 ms exceeded");
        let early = Duration::from_millis(150);
        let before = past_deadline(cancelled(), &Collected::default(), deadline, early);
        assert_eq!(before.label(), "CANCELLED (1)", "before the deadline");
        let bounded = Collected {
            stopped: Some(crate::exchange::StreamStop::MaxMessages(3)),
            ..Collected::default()
        };
        let stopped = past_deadline(cancelled(), &bounded, deadline, deadline);
        assert_eq!(stopped.label(), "CANCELLED (1)", "our own stream bound");
        let failed = GrpcStatus::new(INTERNAL, "boom");
        let other = past_deadline(failed, &Collected::default(), deadline, deadline);
        assert_eq!(other.label(), "INTERNAL (13)");
    }

    #[test]
    fn grpc_timeouts_have_at_most_eight_digits() {
        assert_eq!(grpc_timeout(Duration::ZERO), "1m");
        assert_eq!(grpc_timeout(Duration::from_millis(250)), "250m");
        assert_eq!(grpc_timeout(Duration::from_millis(99_999_999)), "99999999m");
        assert_eq!(grpc_timeout(Duration::from_millis(100_000_000)), "100000S");
        assert_eq!(grpc_timeout(Duration::MAX), "99999999S");
    }

    #[test]
    fn ended_early_statuses() {
        let mut frames = FrameReader::new(1024);
        assert!(ended_early(&frames).message.contains("without trailers"));
        frames.push(&[0, 0]);
        assert!(ended_early(&frames).message.contains("inside a frame"));
    }
}
