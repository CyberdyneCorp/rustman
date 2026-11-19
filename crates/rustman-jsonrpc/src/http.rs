//! Calls and batches over HTTP through `rustman-http` (design D1): one
//! POST carries the envelope or the batch array, so timeouts, TLS,
//! proxies, auth helpers and cookies apply as for `kind: http`.

use std::future::Future;
use std::pin::Pin;
use std::time::{Instant, SystemTime};

use rustman_core::vars::{ResolvedBody, ResolvedJsonrpc, ResolvedRequest};
use rustman_http::{Exchange, HttpEngine, RequestError, SendOptions};
use serde_json::Value;
use tokio_util::sync::CancellationToken;

use crate::envelope::{Envelope, IdSequence, request_body};
use crate::error::JsonrpcError;
use crate::outcome::{
    Attempt, CallResult, RpcExchange, TransportKind, batch_results, notification_result,
    single_result,
};
use crate::retry::{RetryPolicy, is_retryable_status, limit_exceeded, pause, retry_after};

/// A boxed future of one HTTP exchange.
pub type HttpFuture<'a> = Pin<Box<dyn Future<Output = Result<Exchange, RequestError>> + Send + 'a>>;

/// What sends the POST: the engine itself, or a front-end's wrapper that
/// adds authorization flows or a safety policy.
pub trait HttpSend: Sync {
    fn send_http<'a>(
        &'a self,
        request: &'a ResolvedRequest,
        options: &'a SendOptions,
    ) -> HttpFuture<'a>;
}

impl HttpSend for HttpEngine {
    fn send_http<'a>(
        &'a self,
        request: &'a ResolvedRequest,
        options: &'a SendOptions,
    ) -> HttpFuture<'a> {
        Box::pin(self.send(request, options))
    }
}

/// The HTTP request carrying `body` to the endpoint of `target`.
pub fn http_request(target: &ResolvedJsonrpc, body: Value) -> ResolvedRequest {
    let mut request = target.http.clone();
    request.method = "POST".to_owned();
    request.body = ResolvedBody::Json(body);
    request
}

/// Send the calls of `target` in one POST, retrying idempotent requests.
/// Every attempt is recorded; errors are redacted. A response body larger
/// than `[http] max_body_bytes` is refused (see [`capped`]).
pub async fn send_http(
    sender: &dyn HttpSend,
    options: &SendOptions,
    target: &ResolvedJsonrpc,
    cancel: &CancellationToken,
) -> RpcExchange {
    let started = Instant::now();
    let mut exchange = RpcExchange::new(TransportKind::Http, target.batch);
    let (envelopes, body) = request_body(&target.calls, target.batch, &mut IdSequence::default());
    exchange.sent = body.to_string();
    let request = http_request(target, body);
    let policy = RetryPolicy::of(target);
    let limit = options.body.max_in_memory;
    let options = &capped(options);
    let mut retry = 0;
    loop {
        let attempt_started = Instant::now();
        let sent = sender.send_http(&request, options).await;
        let answer = Answer::read(sent, &envelopes, target.batch, limit);
        let mut attempt = answer.attempt(attempt_started, &request);
        let again = retry < policy.retries && answer.retryable();
        if again {
            retry += 1;
            let delay = policy.delay(retry, answer.retry_after);
            attempt.retry_in = Some(delay);
            exchange.attempts.push(attempt);
            if pause(delay, cancel).await {
                continue;
            }
            answer.finish(&mut exchange, &request);
            break;
        }
        exchange.attempts.push(attempt);
        answer.finish(&mut exchange, &request);
        break;
    }
    exchange.elapsed = started.elapsed();
    exchange
}

/// `options` with the body download stopped at the in-memory cap
/// (`[http] max_body_bytes`): an answer is parsed whole and recorded, so
/// it is never spilled to disk, read back or stored past that cap.
fn capped(options: &SendOptions) -> SendOptions {
    let mut capped = options.clone();
    let limit = options.body.max_in_memory;
    capped.body.max_download = limit;
    capped.body.max_decoded = limit;
    capped
}

/// Whether `error` is the body reader stopping at `limit` bytes (as
/// received or decompressed).
fn over_limit(error: &RequestError, limit: u64) -> bool {
    let RequestError::Network(message) = error else {
        return false;
    };
    message.starts_with("network: the ")
        && message.ends_with(&format!("response body is larger than {limit} bytes"))
}

/// One HTTP answer, read as JSON-RPC.
struct Answer {
    http: Option<Exchange>,
    received: Option<String>,
    calls: Result<Vec<CallResult>, JsonrpcError>,
    retry_after: Option<std::time::Duration>,
}

impl Answer {
    fn read(
        sent: Result<Exchange, RequestError>,
        envelopes: &[Envelope],
        batch: bool,
        limit: u64,
    ) -> Self {
        let mut http = match sent {
            Ok(http) => http,
            Err(e) if over_limit(&e, limit) => {
                return Self::unanswered(JsonrpcError::body_too_large(limit));
            }
            Err(e) => return Self::unanswered(JsonrpcError::Transport(e)),
        };
        if http.spill.is_some() {
            // A sender that ignored the cap: keep only the head, unread.
            http.body = http.body.slice(..0);
            return Self::failed(http, JsonrpcError::body_too_large(limit));
        }
        let body = http.body.clone();
        let received = String::from_utf8_lossy(&body).into_owned();
        let calls = interpret(http.status, &body, envelopes, batch);
        let retry_after = http
            .headers
            .get("retry-after")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| retry_after(v, SystemTime::now()));
        Self {
            http: Some(http),
            received: Some(received),
            calls,
            retry_after,
        }
    }

    fn unanswered(error: JsonrpcError) -> Self {
        Self {
            http: None,
            received: None,
            calls: Err(error),
            retry_after: None,
        }
    }

    fn failed(http: Exchange, error: JsonrpcError) -> Self {
        Self {
            http: Some(http),
            received: None,
            calls: Err(error),
            retry_after: None,
        }
    }

    fn status(&self) -> Option<u16> {
        self.http.as_ref().map(|h| h.status)
    }

    /// Connection errors, timeouts, 429/502/503/504 and `-32005`.
    fn retryable(&self) -> bool {
        if self.status().is_some_and(is_retryable_status) {
            return true;
        }
        match &self.calls {
            Ok(calls) => limit_exceeded(calls),
            Err(e) => e.is_transient(),
        }
    }

    fn attempt(&self, started: Instant, request: &ResolvedRequest) -> Attempt {
        let error = match &self.calls {
            Ok(calls) if limit_exceeded(calls) => calls.iter().find_map(CallResult::failure),
            Ok(_) => None,
            Err(e) => Some(self.redacted(e.clone(), request).to_string()),
        };
        Attempt {
            status: self.status(),
            error,
            elapsed: started.elapsed(),
            retry_in: None,
        }
    }

    fn redacted(&self, error: JsonrpcError, request: &ResolvedRequest) -> JsonrpcError {
        let redactor = match &self.http {
            Some(http) => http.redactor(request),
            None => rustman_core::Redactor::for_request(request),
        };
        error.redacted(&redactor)
    }

    fn finish(self, exchange: &mut RpcExchange, request: &ResolvedRequest) {
        match &self.calls {
            Ok(calls) => exchange.calls.clone_from(calls),
            Err(e) => exchange.error = Some(self.redacted(e.clone(), request)),
        }
        exchange.received = self.received;
        exchange.http = self.http;
    }
}

/// The calls' results from an HTTP answer. A non-2xx status is the
/// exchange error `HTTP <status>` unless the body is a JSON-RPC error.
fn interpret(
    status: u16,
    body: &[u8],
    envelopes: &[Envelope],
    batch: bool,
) -> Result<Vec<CallResult>, JsonrpcError> {
    let success = (200..300).contains(&status);
    if envelopes.iter().all(|e| e.id.is_none()) {
        if !success {
            return Err(JsonrpcError::Http(status));
        }
        return Ok(envelopes.iter().map(notification_result).collect());
    }
    let parsed: Option<Value> = serde_json::from_slice(body).ok();
    let matched = parsed.map(|value| match_calls(envelopes, &value, batch));
    match matched {
        Some(Ok(calls)) if success || carries_rpc_error(&calls) => Ok(calls),
        Some(Err(e)) if success => Err(e),
        None if success => Err(JsonrpcError::Invalid("the body is not JSON".to_owned())),
        _ => Err(JsonrpcError::Http(status)),
    }
}

fn match_calls(
    envelopes: &[Envelope],
    value: &Value,
    batch: bool,
) -> Result<Vec<CallResult>, JsonrpcError> {
    match envelopes {
        [single] if !batch => single_result(single, value).map(|r| vec![r]),
        many => batch_results(many, value),
    }
}

fn carries_rpc_error(calls: &[CallResult]) -> bool {
    calls
        .iter()
        .any(|c| matches!(c.outcome, crate::outcome::CallOutcome::Error(_)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use rustman_core::vars::ResolvedCall;

    fn envelopes(calls: &[ResolvedCall], batch: bool) -> Vec<Envelope> {
        request_body(calls, batch, &mut IdSequence::default()).0
    }

    #[test]
    fn a_non_2xx_rpc_error_is_that_error() {
        let env = envelopes(&[ResolvedCall::new("m", None)], false);
        let body = br#"{"jsonrpc":"2.0","id":1,"error":{"code":-32000,"message":"bad"}}"#;
        let calls = interpret(500, body, &env, false).unwrap();
        assert_eq!(calls[0].failure().as_deref(), Some("rpc error -32000: bad"));
        let err = interpret(502, b"<html>bad gateway</html>", &env, false).unwrap_err();
        assert_eq!(err, JsonrpcError::Http(502));
        let err =
            interpret(500, br#"{"jsonrpc":"2.0","id":1,"result":1}"#, &env, false).unwrap_err();
        assert_eq!(err, JsonrpcError::Http(500));
    }

    #[test]
    fn a_2xx_body_that_is_not_json_is_invalid() {
        let env = envelopes(&[ResolvedCall::new("m", None)], false);
        let err = interpret(200, b"ok", &env, false).unwrap_err();
        assert_eq!(
            err.to_string(),
            "invalid JSON-RPC response: the body is not JSON"
        );
    }

    #[test]
    fn only_the_body_limit_of_the_cap_is_over_limit() {
        let raw =
            RequestError::Network("network: the response body is larger than 64 bytes".into());
        let decoded = RequestError::Network(
            "network: the decompressed response body is larger than 64 bytes".into(),
        );
        assert!(over_limit(&raw, 64) && over_limit(&decoded, 64));
        assert!(!over_limit(&raw, 65));
        assert!(!over_limit(
            &RequestError::Network("network: reset".into()),
            64
        ));
        assert!(!over_limit(
            &RequestError::Io("the response body is larger than 64 bytes".into()),
            64
        ));
        let options = capped(&SendOptions::default());
        assert_eq!(options.body.max_download, options.body.max_in_memory);
        assert_eq!(options.body.max_decoded, options.body.max_in_memory);
    }

    #[test]
    fn notifications_complete_on_the_http_response() {
        let mut call = ResolvedCall::new("log", None);
        call.notification = true;
        let env = envelopes(&[call], false);
        let calls = interpret(204, b"", &env, false).unwrap();
        assert!(calls[0].failure().is_none());
        assert_eq!(
            interpret(503, b"", &env, false).unwrap_err(),
            JsonrpcError::Http(503)
        );
    }
}
