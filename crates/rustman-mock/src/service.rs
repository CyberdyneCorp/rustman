//! Handling one request: the request limits, the reserved `/__rustman/`
//! paths, routing, security and validation, overrides, the stateful
//! store and the response chosen from the document, then latency and
//! faults. Every error the mock makes is problem+json.

use std::convert::Infallible;
use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use http::header::{ACCEPT, ALLOW, CONNECTION, CONTENT_LENGTH, CONTENT_TYPE, WWW_AUTHENTICATE};
use http::request::Parts;
use http::{HeaderMap, HeaderValue, Request, Response, StatusCode, Version};
use http_body_util::combinators::UnsyncBoxBody;
use http_body_util::{BodyExt, Full, LengthLimitError, Limited};
use hyper::body::Incoming;
use rustman_webhook::Limits;

use rustman_webhook::Capture;

use crate::faults::{Injected, Latency, SlowBody};
use crate::log::{LogEntry, MockEvent, Received};
use crate::overrides::Override;
use crate::prefer::Prefer;
use crate::problem::Problem;
use crate::proxy::Outgoing;
use crate::record::Recorder;
use crate::request::RequestView;
use crate::respond::{Ask, Chosen, Source, respond};
use crate::router::Routed;
use crate::server::Shared;
use crate::state::MockState;
use crate::{cors, security, stateful};

/// A response body: whole, or paced by a `slow_body_bps` fault.
pub(crate) type MockBody = UnsyncBoxBody<Bytes, Infallible>;
type Reply = Response<MockBody>;
type Plain = Response<Full<Bytes>>;

/// Bytes a header field adds to a head besides its name and value
/// (`": "` and CRLF).
const FIELD_OVERHEAD: usize = 4;

/// The reserved prefix of the mock's own endpoints.
pub const RESERVED_PREFIX: &str = "/__rustman/";

/// A `drop_connection` fault: hyper closes the connection without
/// writing a response.
#[derive(Debug, thiserror::Error)]
#[error("connection dropped by a fault")]
pub(crate) struct Dropped;

pub(crate) async fn handle(req: Request<Incoming>, shared: Arc<Shared>) -> Result<Reply, Dropped> {
    let started = Instant::now();
    let version = req.version();
    let (parts, body) = req.into_parts();
    let body = match receive(&parts, body, &shared.config.limits).await {
        Ok(body) => body,
        Err(problem) => {
            let mut reply = close_http1(problem.response(), version);
            finish(
                &shared,
                &parts,
                &[],
                &Outcome::refused(&problem),
                &mut reply,
                started,
            );
            return Ok(reply.map(boxed));
        }
    };
    let state = shared.state.load_full();
    let mut planned = plan(&parts, &body, &shared, &state);
    if planned.outcome.forward {
        planned.reply = proxied(&parts, body.clone(), &shared).await;
    }
    let outcome = std::mem::take(&mut planned.outcome);
    let delivered = deliver(planned).await;
    match delivered {
        Ok(mut reply) => {
            finish(&shared, &parts, &body, &outcome, &mut reply, started);
            Ok(reply)
        }
        Err(dropped) => {
            shared.log(|| entry(&shared, &parts, &body, &outcome, None, started));
            Err(dropped)
        }
    }
}

/// Add the CORS headers (never to a proxied response, so a page cannot
/// read the upstream through the mock) and log the request.
fn finish<B>(
    shared: &Shared,
    parts: &Parts,
    body: &[u8],
    outcome: &Outcome,
    reply: &mut Response<B>,
    started: Instant,
) {
    if shared.cors && !outcome.forward {
        cors::decorate(&parts.headers, reply.headers_mut());
    }
    let status = Some(reply.status().as_u16());
    shared.log(|| entry(shared, parts, body, outcome, status, started));
}

fn entry(
    shared: &Shared,
    parts: &Parts,
    body: &[u8],
    outcome: &Outcome,
    status: Option<u16>,
    started: Instant,
) -> LogEntry {
    let received = Received {
        method: parts.method.as_str(),
        path_and_query: parts.uri.path_and_query().map_or("/", |p| p.as_str()),
        headers: &parts.headers,
        body,
    };
    let mut entry = received.entry(&shared.redactor, &shared.state.load().credentials);
    entry.status = status;
    entry.source = outcome.source;
    entry.operation.clone_from(&outcome.operation);
    entry.override_name.clone_from(&outcome.override_name);
    entry.errors = outcome
        .errors
        .iter()
        .map(|e| shared.redactor.text(e).into_owned())
        .collect();
    entry.duration_ms = started.elapsed().as_secs_f64() * 1000.0;
    entry
}

/// The body, once the head and body passed the limits.
async fn receive(parts: &Parts, body: Incoming, limits: &Limits) -> Result<Bytes, Problem> {
    check_headers(&parts.headers, limits)?;
    let declared = parts
        .headers
        .get(CONTENT_LENGTH)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse::<u64>().ok());
    if declared.is_some_and(|length| length > limits.max_body as u64) {
        return Err(too_large(limits.max_body));
    }
    let collect = Limited::new(body, limits.max_body).collect();
    match tokio::time::timeout(limits.body_timeout, collect).await {
        Ok(Ok(collected)) => Ok(collected.to_bytes()),
        Ok(Err(err)) if err.downcast_ref::<LengthLimitError>().is_some() => {
            Err(too_large(limits.max_body))
        }
        Ok(Err(err)) => Err(Problem::new(
            StatusCode::BAD_REQUEST,
            "malformed-request",
            "Malformed request",
            format!("invalid body: {err}"),
        )),
        Err(_) => Err(Problem::new(
            StatusCode::REQUEST_TIMEOUT,
            "request-timeout",
            "Request timeout",
            "the request body did not arrive in time",
        )),
    }
}

fn check_headers(headers: &HeaderMap, limits: &Limits) -> Result<(), Problem> {
    let too_large = |detail: String| {
        Problem::new(
            StatusCode::REQUEST_HEADER_FIELDS_TOO_LARGE,
            "header-fields-too-large",
            "Request header fields too large",
            detail,
        )
    };
    if headers.len() > limits.max_headers {
        let detail = format!("more than {} header fields", limits.max_headers);
        return Err(too_large(detail));
    }
    let bytes: usize = headers
        .iter()
        .map(|(name, value)| name.as_str().len() + value.len() + FIELD_OVERHEAD)
        .sum();
    if bytes > limits.max_header_bytes {
        let detail = format!(
            "header fields larger than {} bytes",
            limits.max_header_bytes
        );
        return Err(too_large(detail));
    }
    Ok(())
}

fn too_large(max_body: usize) -> Problem {
    Problem::new(
        StatusCode::PAYLOAD_TOO_LARGE,
        "body-too-large",
        "Request body too large",
        format!("the body is larger than {max_body} bytes"),
    )
}

/// What the request log says about a reply.
#[derive(Debug)]
struct Outcome {
    source: Source,
    operation: Option<String>,
    override_name: Option<String>,
    errors: Vec<String>,
    /// Forward the request to the `--proxy-unmatched` upstream.
    forward: bool,
}

impl Default for Outcome {
    fn default() -> Self {
        Self::from(Source::Error)
    }
}

impl From<Source> for Outcome {
    fn from(source: Source) -> Self {
        Self {
            source,
            operation: None,
            override_name: None,
            errors: Vec::new(),
            forward: false,
        }
    }
}

impl Outcome {
    /// A problem the mock answered with: its validation errors, else its
    /// detail.
    fn refused(problem: &Problem) -> Self {
        let errors = if problem.errors.is_empty() {
            vec![problem.detail.clone()]
        } else {
            problem
                .errors
                .iter()
                .map(|e| format!("{} {}: {}", e.location, e.pointer, e.message))
                .collect()
        };
        Self {
            errors,
            ..Self::from(Source::Error)
        }
    }
}

/// A reply with its log outcome, before latency and faults.
struct Answer<'s> {
    reply: Plain,
    outcome: Outcome,
    chosen: Option<&'s Override>,
}

impl<'s> Answer<'s> {
    fn new(reply: Plain, source: Source) -> Self {
        Self {
            reply,
            outcome: Outcome::from(source),
            chosen: None,
        }
    }

    fn problem(problem: &Problem) -> Self {
        Self {
            reply: problem.response(),
            outcome: Outcome::refused(problem),
            chosen: None,
        }
    }

    fn chosen(chosen: Result<Chosen, Problem>) -> Self {
        match chosen {
            Ok(chosen) => {
                let source = chosen.source;
                Self::new(reply(chosen), source)
            }
            Err(problem) => Self::problem(&problem),
        }
    }

    fn operation(mut self, label: String) -> Self {
        self.outcome.operation = Some(label);
        self
    }
}

/// A reply with what still has to happen to it.
struct Planned {
    reply: Plain,
    outcome: Outcome,
    delay: Duration,
    injected: Injected,
    slow_bps: Option<u64>,
}

impl Planned {
    fn plain(reply: Plain, outcome: Outcome) -> Self {
        Self {
            reply,
            outcome,
            delay: Duration::ZERO,
            injected: Injected::None,
            slow_bps: None,
        }
    }
}

/// Wait, then answer, fail or drop as planned.
async fn deliver(planned: Planned) -> Result<Reply, Dropped> {
    if !planned.delay.is_zero() {
        tokio::time::sleep(planned.delay).await;
    }
    match planned.injected {
        Injected::Drop => Err(Dropped),
        Injected::Error(status) => Ok(fault_problem(status).response().map(boxed)),
        Injected::None => Ok(match planned.slow_bps {
            Some(bps) => slow(planned.reply, bps).await,
            None => planned.reply.map(boxed),
        }),
    }
}

fn boxed(body: Full<Bytes>) -> MockBody {
    body.boxed_unsync()
}

async fn slow(reply: Plain, bps: u64) -> Reply {
    let (parts, body) = reply.into_parts();
    let bytes = match body.collect().await {
        Ok(collected) => collected.to_bytes(),
        Err(never) => match never {},
    };
    Response::from_parts(parts, SlowBody::new(bytes, bps).boxed_unsync())
}

fn fault_problem(status: StatusCode) -> Problem {
    Problem::new(
        status,
        "injected-fault",
        "Injected fault",
        format!(
            "the mock answered {} as configured by a fault",
            status.as_u16()
        ),
    )
}

/// The reply to the request and its latency and faults.
fn plan(parts: &Parts, body: &Bytes, shared: &Shared, state: &MockState) -> Planned {
    let path = parts.uri.path();
    if path.starts_with(RESERVED_PREFIX) {
        let answer = reserved(parts, shared);
        return Planned::plain(answer.reply, answer.outcome);
    }
    if shared.cors && cors::is_preflight(&parts.method, &parts.headers) {
        let reply = cors::preflight(&parts.headers, allowed_methods(state, path));
        return Planned::plain(reply, Outcome::from(Source::Cors));
    }
    let method = parts.method.as_str();
    let mut request = RequestView::new(method, path, parts.uri.query(), &parts.headers, body);
    let stripped = state.router.strip_base(path);
    let answer = match state.router.route(method, path) {
        Routed::Operation {
            operation, params, ..
        } => {
            request.params = params;
            operation_answer(operation, &mut request, stripped, shared, state)
        }
        unmatched => unmatched_answer(&unmatched, &mut request, stripped, shared, state),
    };
    let route = answer
        .chosen
        .map_or_else(|| format!("{method} {stripped}"), |o| o.name.clone());
    effects(answer, &route, shared)
}

/// The methods of the document path `path` matches, for a preflight.
fn allowed_methods(state: &MockState, path: &str) -> Option<String> {
    match state.router.route(http::Method::OPTIONS.as_str(), path) {
        Routed::Operation { route, .. } | Routed::MethodNotAllowed(route) => Some(route.allow()),
        Routed::NotFound => None,
    }
}

/// The latency and fault of an override (else the `[mock]` latency).
fn effects(answer: Answer<'_>, route: &str, shared: &Shared) -> Planned {
    let chosen = answer.chosen;
    let latency = chosen
        .map(|o| o.latency)
        .filter(|l| !l.is_zero())
        .unwrap_or(shared.config.latency);
    let fault = chosen.and_then(|o| o.fault.as_ref());
    if latency.is_zero() && fault.is_none() {
        return Planned::plain(answer.reply, answer.outcome);
    }
    let mut picks = shared.sequences.next(shared.config.seed, route);
    let injected = fault.map_or(Injected::None, |f| f.decide(&mut picks));
    let mut outcome = answer.outcome;
    if let Injected::Error(_) = injected {
        outcome.source = Source::Error;
    }
    Planned {
        delay: Latency::delay(&latency, &mut picks),
        injected,
        slow_bps: fault.and_then(|f| f.slow_body_bps),
        reply: answer.reply,
        outcome,
    }
}

/// `POST /__rustman/reset` clears the stateful store; every other path
/// under the prefix is reserved.
fn reserved(parts: &Parts, shared: &Shared) -> Answer<'static> {
    if parts.uri.path() == "/__rustman/reset" && parts.method == http::Method::POST {
        shared.store.reset();
        let mut reply = Response::new(Full::new(Bytes::new()));
        *reply.status_mut() = StatusCode::NO_CONTENT;
        return Answer::new(reply, Source::Stateful);
    }
    let detail = format!(
        "{} {} is reserved for the mock itself; POST /__rustman/reset clears the stateful store",
        parts.method,
        parts.uri.path()
    );
    Answer::problem(&Problem::new(
        StatusCode::NOT_FOUND,
        "reserved-path",
        "Reserved path",
        detail,
    ))
}

/// No operation: an override, the upstream, or 404/405.
fn unmatched_answer<'s>(
    routed: &Routed<'_>,
    request: &mut RequestView<'_>,
    stripped: &str,
    shared: &Shared,
    state: &'s MockState,
) -> Answer<'s> {
    if let Some((found, params)) = state.overrides.find(request, stripped) {
        request.params = params;
        return override_answer(found, request);
    }
    if shared.proxy.is_some() {
        let mut answer = Answer::new(Response::new(Full::new(Bytes::new())), Source::Proxy);
        answer.outcome.forward = true;
        return answer;
    }
    let (method, path) = (request.method, request.path);
    match routed {
        Routed::MethodNotAllowed(route) => {
            let allow = route.allow();
            let mut answer = Answer::problem(&Problem::method_not_allowed(method, path, &allow));
            if let Ok(value) = HeaderValue::from_str(&allow) {
                answer.reply.headers_mut().insert(ALLOW, value);
            }
            answer
        }
        _ => Answer::problem(&Problem::not_found(method, path)),
    }
}

/// Security, validation, then an override, the stateful store or the
/// document's response.
fn operation_answer<'s>(
    index: usize,
    request: &mut RequestView<'_>,
    stripped: &str,
    shared: &Shared,
    state: &'s MockState,
) -> Answer<'s> {
    let operation = &state.model.operations[index];
    let label = operation
        .operation_id
        .clone()
        .unwrap_or_else(|| format!("{} {}", operation.method, operation.path));
    if shared.config.enforce_security
        && let Err(denied) =
            security::check(&state.model, operation, request, &shared.config.security)
    {
        let mut answer = Answer::problem(&denied.problem);
        answer.reply = unauthorized(&denied);
        return answer.operation(label);
    }
    if shared.config.validate
        && let Err(problem) = state
            .checks
            .validate(index, request, &state.model.components)
    {
        return Answer::problem(&problem).operation(label);
    }
    if let Some((found, params)) = state.overrides.find(request, stripped) {
        if !params.is_empty() {
            request.params = params;
        }
        return override_answer(found, request).operation(label);
    }
    let chosen = stateful_reply(index, request, shared, state)
        .unwrap_or_else(|| document_reply(index, request, shared, state));
    Answer::chosen(chosen).operation(label)
}

fn unauthorized(denied: &security::Denied) -> Plain {
    let mut reply = denied.problem.response();
    for challenge in &denied.challenges {
        if let Ok(value) = HeaderValue::from_str(challenge) {
            reply.headers_mut().append(WWW_AUTHENTICATE, value);
        }
    }
    reply
}

fn override_answer<'s>(found: &'s Override, request: &RequestView<'_>) -> Answer<'s> {
    let rendered = found.reply(request);
    let mut outcome = Outcome::from(Source::Override);
    outcome.override_name = Some(found.name.clone());
    if !rendered.unresolved.is_empty() {
        tracing::warn!(
            "mock: override {}: unresolved template reference(s) rendered empty: {}",
            found.name,
            rendered.unresolved.join(", ")
        );
        outcome.errors = rendered
            .unresolved
            .iter()
            .map(|r| format!("unresolved template reference {r} rendered empty"))
            .collect();
    }
    let length = rendered.body.len();
    let mut reply = Response::new(Full::new(rendered.body));
    *reply.status_mut() = rendered.status;
    let headers = reply.headers_mut();
    for (name, value) in rendered.headers {
        headers.append(name, value);
    }
    headers.insert(CONTENT_LENGTH, HeaderValue::from(length));
    Answer {
        reply,
        outcome,
        chosen: Some(found),
    }
}

fn stateful_reply(
    index: usize,
    request: &RequestView<'_>,
    shared: &Shared,
    state: &MockState,
) -> Option<Result<Chosen, Problem>> {
    if !shared.config.stateful {
        return None;
    }
    let operation = &state.model.operations[index];
    let (resource, role) = state.resources.for_operation(operation)?;
    let id = request.param(&resource.param);
    stateful::answer(operation, resource, role, id, request.json(), &shared.store)
}

fn document_reply(
    index: usize,
    request: &RequestView<'_>,
    shared: &Shared,
    state: &MockState,
) -> Result<Chosen, Problem> {
    let prefer = shared
        .config
        .prefer
        .then(|| Prefer::parse(header_values(request.headers, "prefer")));
    let accept = request.headers.get(ACCEPT).and_then(|v| v.to_str().ok());
    let ask = Ask {
        path: request.path,
        accept,
        prefer: prefer.as_ref(),
        seed: shared.config.seed,
    };
    respond(
        &state.model.operations[index],
        &state.model.components,
        &ask,
    )
}

fn header_values<'h>(headers: &'h HeaderMap, name: &str) -> impl Iterator<Item = &'h str> {
    headers.get_all(name).iter().filter_map(|v| v.to_str().ok())
}

fn reply(chosen: Chosen) -> Plain {
    let length = chosen.body.len();
    let mut response = Response::new(Full::new(Bytes::from(chosen.body)));
    *response.status_mut() = chosen.status;
    let headers = response.headers_mut();
    if let Some(value) = chosen
        .content_type
        .and_then(|t| HeaderValue::from_str(&t).ok())
    {
        headers.insert(CONTENT_TYPE, value);
    }
    headers.insert(CONTENT_LENGTH, HeaderValue::from(length));
    response
}

/// Forward to the upstream, recording the exchange when asked.
async fn proxied(parts: &Parts, body: Bytes, shared: &Arc<Shared>) -> Plain {
    let Some(proxy) = &shared.proxy else {
        return Problem::not_found(parts.method.as_str(), parts.uri.path()).response();
    };
    let capture = Outgoing {
        method: parts.method.as_str(),
        path: parts.uri.path(),
        query: parts.uri.query(),
        version: parts.version,
        headers: &parts.headers,
        body,
    }
    .capture();
    match proxy.forward(&capture).await {
        Ok(reply) => {
            if let Some(recorder) = shared.recorder.get() {
                let url = proxy.upstream.url(&capture.target());
                let content_type = reply
                    .headers()
                    .get(CONTENT_TYPE)
                    .and_then(|v| v.to_str().ok())
                    .map(str::to_owned);
                record(shared, Arc::clone(recorder), capture, url, content_type).await;
            }
            reply
        }
        Err(problem) => problem.response(),
    }
}

/// Save the exchange off the async workers and report the outcome.
async fn record(
    shared: &Shared,
    recorder: Arc<Recorder>,
    capture: Capture,
    url: String,
    content_type: Option<String>,
) {
    let saved = tokio::task::spawn_blocking(move || {
        recorder
            .record(&capture, &url, content_type.as_deref())
            .map_err(|e| e.to_string())
    })
    .await
    .unwrap_or_else(|e| Err(e.to_string()));
    let event = match saved {
        Ok(Some(path)) => MockEvent::Recorded(path),
        Ok(None) => return,
        Err(message) => MockEvent::RecordFailed(shared.redactor.text(&message).into_owned()),
    };
    shared.emit(event);
}

/// A refused HTTP/1 request may leave unread bytes: close after replying.
fn close_http1(mut response: Plain, version: Version) -> Plain {
    if version < Version::HTTP_2 {
        response
            .headers_mut()
            .insert(CONNECTION, HeaderValue::from_static("close"));
    }
    response
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn header_limits_count_fields_and_bytes() {
        let limits = Limits {
            max_headers: 2,
            max_header_bytes: 16,
            ..Limits::default()
        };
        let headers = |pairs: &[(&'static str, &'static str)]| {
            let mut map = HeaderMap::new();
            for (name, value) in pairs {
                map.append(*name, HeaderValue::from_static(value));
            }
            map
        };
        assert!(check_headers(&headers(&[("a", "1"), ("b", "2")]), &limits).is_ok());
        let many = check_headers(&headers(&[("a", "1"), ("b", "2"), ("c", "3")]), &limits);
        assert_eq!(many.unwrap_err().status, 431);
        let big = check_headers(&headers(&[("a", "123456789012")]), &limits);
        assert_eq!(big.unwrap_err().status, 431);
    }

    #[test]
    fn http1_refusals_close_the_connection() {
        let reply = close_http1(too_large(1).response(), Version::HTTP_11);
        assert_eq!(reply.headers()[CONNECTION], "close");
        let h2 = close_http1(too_large(1).response(), Version::HTTP_2);
        assert!(h2.headers().get(CONNECTION).is_none());
    }
}
