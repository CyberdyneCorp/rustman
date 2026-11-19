//! Trace context injection into the headers `rustman-http` builds
//! (trace-propagation spec, design D2).

use http::header::{HeaderMap, HeaderName, HeaderValue};
use rustman_core::trace::{
    self, SentTrace, TraceContextInjector, TracePropagation,
    baggage::{self, BAGGAGE},
    w3c::TRACESTATE,
};

/// Add the trace headers of `propagation` to `headers`, unless the request
/// already carries its own, and return what was sent. Without propagation,
/// a trace context the request sets itself is still returned, so it is
/// recorded.
pub(crate) fn inject(
    headers: &mut HeaderMap,
    propagation: Option<&TracePropagation>,
) -> Option<SentTrace> {
    let mut carrier = Headers(headers);
    let context = match propagation {
        Some(propagation) => trace::inject_random(&mut carrier, propagation)?,
        None => trace::supplied(&carrier, true)?,
    };
    Some(SentTrace {
        context,
        tracestate: carrier.get(TRACESTATE),
        baggage: carrier.get(BAGGAGE),
    })
}

/// The warning for a request that sets a trace header which does not
/// parse while propagation is on: it is sent as given, and no trace
/// context is added or recorded.
pub(crate) fn invalid_context_warning(
    headers: &mut HeaderMap,
    propagation: Option<&TracePropagation>,
) -> Option<String> {
    let propagation = propagation?;
    trace::carries_invalid(&Headers(headers), propagation.sampled).then(|| {
        "trace: the request's own trace header is not valid; it is sent as given and no \
         trace context is added or recorded"
            .to_owned()
    })
}

/// The warning for a `baggage` header over the W3C size limits.
pub(crate) fn baggage_warning(sent: &SentTrace) -> Option<String> {
    sent.baggage.as_deref().and_then(baggage::limit_warning)
}

/// A [`HeaderMap`] as a trace carrier.
struct Headers<'a>(&'a mut HeaderMap);

impl TraceContextInjector for Headers<'_> {
    fn get(&self, name: &str) -> Option<String> {
        let value = self.0.get(name)?;
        Some(String::from_utf8_lossy(value.as_bytes()).into_owned())
    }

    /// Every value is ASCII (hex ids, checked `tracestate`, encoded
    /// `baggage`), so a value that is not a valid header is never built.
    fn set(&mut self, name: &str, value: String) {
        let name = HeaderName::try_from(name);
        let value = HeaderValue::try_from(value);
        if let (Ok(name), Ok(value)) = (name, value) {
            self.0.insert(name, value);
        }
    }
}
