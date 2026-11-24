//! Subscriptions over Server-Sent Events (GraphQL over SSE, distinct
//! connections mode): the operation is POSTed with `Accept:
//! text/event-stream` through `rustman-http` (TLS, proxies, auth, cookies),
//! and every `next` event is recorded until `complete`, the limits or the
//! caller's stop, which closes the connection.
//!
//! The event stream is parsed by `rustman-http`'s [`SseParser`], the one
//! WHATWG parser shared with realtime SSE sessions, rather than the
//! `eventsource-stream` crate named in design D4.

use std::time::{Duration, Instant};

use rustman_core::vars::{Resolved, ResolvedGraphql, ResolvedPair, ResolvedRequest};
use rustman_http::{
    EVENT_STREAM, HttpEngine, SendOptions, SseEvent, SseParser, SseTooLarge, StreamedResponse,
    is_event_stream,
};
use serde_json::Value;

use super::{Collector, Ended, SubscribeOptions, Subscription, SubscriptionProtocol};
use crate::error::GraphqlError;
use crate::execute::HttpOperation;

/// Run `operation` as a subscription over SSE to the endpoint of `target`.
pub async fn subscribe_sse(
    engine: &HttpEngine,
    send_options: &SendOptions,
    target: &ResolvedGraphql,
    operation: &HttpOperation,
    options: &SubscribeOptions,
) -> Result<Subscription, GraphqlError> {
    let started = Instant::now();
    let request = stream_request(operation, target);
    let limit = options.within(started, send_options.timeout);
    let mut response = open(engine, &request, send_options, options, limit).await?;
    check_stream(&response)?;
    let connected = started.elapsed();
    let mut collector = Collector::new(options, started);
    let mut warnings = std::mem::take(&mut response.warnings);
    // Events are held in memory, so the in-memory body cap bounds them.
    let mut parser = SseParser::new(send_options.body.max_in_memory).keeping_typed_events();
    let read = Reader {
        parser: &mut parser,
        warnings: &mut warnings,
    };
    let ended = collect(&mut response, &mut collector, read).await?;
    let collected = collector.finish();
    Ok(Subscription {
        protocol: SubscriptionProtocol::Sse,
        request,
        status: response.status,
        headers: response.headers.clone(),
        events: collected.events,
        event_count: collected.event_count,
        ended,
        failure: collected.failure,
        connected,
        elapsed: started.elapsed(),
        trace: response.trace.clone(),
        insecure: response.insecure,
        warnings,
    })
}

/// Open the stream within `limit` (the HTTP timeout, shortened to what is
/// left of a headless `duration`), unless the caller stops first.
async fn open(
    engine: &HttpEngine,
    request: &ResolvedRequest,
    send_options: &SendOptions,
    options: &SubscribeOptions,
    limit: Duration,
) -> Result<StreamedResponse, GraphqlError> {
    let opening = tokio::time::timeout(limit, engine.open_stream(request, send_options));
    let opened = tokio::select! {
        biased;
        () = options.cancel.cancelled() => {
            return Err(GraphqlError::Subscription("SSE: stopped before the stream opened".into()));
        }
        opened = opening => opened,
    };
    let response = opened.map_err(|_| {
        GraphqlError::Subscription(format!(
            "SSE: the stream did not open within {} ms",
            limit.as_millis()
        ))
    })??;
    Ok(response)
}

/// The POST of the operation, asking for an event stream, uncompressed.
fn stream_request(operation: &HttpOperation, target: &ResolvedGraphql) -> ResolvedRequest {
    let mut request = operation.http_request(target, true);
    request
        .headers
        .retain(|h| !h.name.text.eq_ignore_ascii_case("accept"));
    let header = |name: &str, value: &str| ResolvedPair {
        name: Resolved::literal(name),
        value: Resolved::literal(value),
    };
    request.headers.insert(0, header("Accept", EVENT_STREAM));
    let encoding = request
        .headers
        .iter()
        .any(|h| h.name.text.eq_ignore_ascii_case("accept-encoding"));
    if !encoding {
        request.headers.push(header("Accept-Encoding", "identity"));
    }
    request
}

/// Reject a response that is not a successful event stream.
fn check_stream(response: &StreamedResponse) -> Result<(), GraphqlError> {
    let media = response.header("content-type").unwrap_or_default();
    if (200..300).contains(&response.status) && is_event_stream(media) {
        return Ok(());
    }
    Err(GraphqlError::Subscription(format!(
        "SSE: HTTP {} with content type '{media}', expected {EVENT_STREAM}",
        response.status
    )))
}

/// What reads the stream: the parser, and the warnings it adds to.
struct Reader<'a> {
    parser: &'a mut SseParser,
    warnings: &'a mut Vec<String>,
}

/// Read events until `complete`, the end of the stream, an event over the
/// size limit, the limits or the caller's stop.
async fn collect(
    response: &mut StreamedResponse,
    collector: &mut Collector<'_>,
    read: Reader<'_>,
) -> Result<Ended, GraphqlError> {
    loop {
        let chunk = tokio::select! {
            biased;
            () = collector.options.cancel.cancelled() => return Ok(Ended::Stopped),
            () = collector.deadline() => return Ok(Ended::Duration),
            chunk = response.chunk() => chunk,
        };
        let Some(chunk) = chunk.transpose()? else {
            collector.fail("SSE: the stream ended without a complete event".into());
            return Ok(Ended::Closed);
        };
        let events = match read.parser.push(&chunk) {
            Ok(events) => events,
            Err(SseTooLarge { limit, .. }) => {
                collector.fail(format!(
                    "SSE: an event is larger than the {limit} byte limit (max_body_bytes)"
                ));
                return Ok(Ended::Error);
            }
        };
        for event in events {
            if let Some(ended) = apply(event, collector, read.warnings) {
                return Ok(ended);
            }
        }
    }
}

/// Record a `next` event; `Some` when the subscription ends with it.
fn apply(
    event: SseEvent,
    collector: &mut Collector<'_>,
    warnings: &mut Vec<String>,
) -> Option<Ended> {
    match event.event.as_str() {
        "complete" => Some(Ended::Complete),
        "next" => match serde_json::from_str::<Value>(&event.data) {
            Ok(payload) => collector.push(payload).then_some(Ended::MaxEvents),
            Err(_) => {
                warnings.push("graphql: SSE: a next event that is not JSON was skipped".into());
                None
            }
        },
        _ => None,
    }
}
