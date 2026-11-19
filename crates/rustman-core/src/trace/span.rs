//! The client span of one exchange (trace-propagation spec "Exported
//! client spans", design D6), built after the exchange from its redacted
//! record: OpenTelemetry HTTP semantic-convention attributes, the URL with
//! every query value redacted, phase timings as events, and no headers.

use super::{SpanId, TraceId};
use crate::run::{ExchangeRecord, RequestRecord, TimingsRecord};
use crate::secret::placeholder;

/// An attribute value.
#[derive(Clone, Debug, PartialEq)]
pub enum SpanValue {
    Str(String),
    Int(i64),
    Double(f64),
}

/// A phase that ended at `unix_nanos` after `duration_ms`.
#[derive(Clone, Debug, PartialEq)]
pub struct SpanEvent {
    pub name: &'static str,
    pub unix_nanos: u64,
    pub duration_ms: f64,
}

/// One `CLIENT` span.
#[derive(Clone, Debug, PartialEq)]
pub struct ClientSpan {
    pub trace_id: TraceId,
    pub span_id: SpanId,
    /// The `tracestate` sent, already redacted.
    pub tracestate: Option<String>,
    /// `{method}`, as the HTTP conventions name client spans.
    pub name: String,
    pub start_unix_nanos: u64,
    pub end_unix_nanos: u64,
    pub attributes: Vec<(&'static str, SpanValue)>,
    pub events: Vec<SpanEvent>,
    /// The response status is 4xx or 5xx.
    pub error: bool,
}

const NANOS_PER_MS: u64 = 1_000_000;

impl ClientSpan {
    /// The span of `record`: `None` when it carried no sampled trace
    /// context or got no response. The record is already redacted.
    pub fn from_record(record: &RequestRecord) -> Option<Self> {
        let trace = record.trace.as_ref().filter(|t| t.sampled)?;
        let exchange = record.exchange.as_ref()?;
        let start = trace.start_ms.saturating_mul(NANOS_PER_MS);
        let end = trace.end_ms.saturating_mul(NANOS_PER_MS).max(start);
        let status = exchange.response.status;
        Some(Self {
            trace_id: TraceId::from_hex(&trace.trace_id)?,
            span_id: SpanId::from_hex(&trace.span_id)?,
            tracestate: trace.tracestate.clone(),
            name: exchange.request.method.clone(),
            start_unix_nanos: start,
            end_unix_nanos: end,
            attributes: attributes(record, exchange),
            events: events(start, &exchange.timings),
            error: status >= 400,
        })
    }
}

fn attributes(record: &RequestRecord, exchange: &ExchangeRecord) -> Vec<(&'static str, SpanValue)> {
    let str = |v: &str| SpanValue::Str(v.to_owned());
    let status = exchange.response.status;
    let mut attributes = vec![
        ("http.request.method", str(&exchange.request.method)),
        ("url.full", str(&url_full(&exchange.request.url))),
        (
            "http.response.status_code",
            SpanValue::Int(i64::from(status)),
        ),
        ("rustman.request.id", str(&record.id)),
    ];
    if let Ok(url) = url::Url::parse(&exchange.request.url) {
        attributes.push(("url.scheme", str(url.scheme())));
        if let Some(host) = url.host_str() {
            attributes.push(("server.address", str(host)));
        }
        if let Some(port) = url.port_or_known_default() {
            attributes.push(("server.port", SpanValue::Int(i64::from(port))));
        }
    }
    if let Some(version) = exchange.response.http_version.strip_prefix("HTTP/") {
        attributes.push(("network.protocol.version", str(version)));
    }
    if status >= 400 {
        attributes.push(("error.type", str(&status.to_string())));
    }
    attributes
}

/// Each phase that happened, as an event at its end.
fn events(start: u64, timings: &TimingsRecord) -> Vec<SpanEvent> {
    let phases = [
        ("dns", timings.dns_ms),
        ("connect", timings.connect_ms),
        ("tls", timings.tls_ms),
        ("waiting", Some(timings.ttfb_ms)),
        ("download", Some(timings.download_ms)),
    ];
    let mut elapsed_ms = 0.0;
    phases
        .into_iter()
        .filter_map(|(name, ms)| Some((name, ms?)))
        .map(|(name, duration_ms)| {
            elapsed_ms += duration_ms;
            SpanEvent {
                name,
                unix_nanos: start + ms_to_nanos(elapsed_ms),
                duration_ms,
            }
        })
        .collect()
}

#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
fn ms_to_nanos(ms: f64) -> u64 {
    (ms.max(0.0) * NANOS_PER_MS as f64) as u64
}

/// `url` without credentials or fragment, each query value replaced by
/// `«redacted:<key>»` and each component without `=` (a bare token) by
/// `«redacted»`.
pub fn url_full(url: &str) -> String {
    let url = url.split('#').next().unwrap_or_default();
    let (base, query) = match url.split_once('?') {
        Some((base, query)) => (base, Some(query)),
        None => (url, None),
    };
    let base = without_credentials(base);
    let Some(query) = query else {
        return base;
    };
    let pairs: Vec<String> = query
        .split('&')
        .map(|pair| match pair.split_once('=') {
            Some((key, _)) => format!("{key}={}", placeholder(key)),
            None if pair.is_empty() => String::new(),
            None => "«redacted»".to_owned(),
        })
        .collect();
    format!("{base}?{}", pairs.join("&"))
}

/// `user:password@` becomes `REDACTED:REDACTED@`, as the HTTP conventions ask.
fn without_credentials(base: &str) -> String {
    let Some((scheme, rest)) = base.split_once("://") else {
        return base.to_owned();
    };
    let authority_end = rest.find('/').unwrap_or(rest.len());
    match rest[..authority_end].rsplit_once('@') {
        Some((_, host)) => format!(
            "{scheme}://REDACTED:REDACTED@{host}{}",
            &rest[authority_end..]
        ),
        None => base.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::run::{ResponseRecord, SentRequest};
    use crate::trace::TraceRecord;

    const TRACE: &str = "4bf92f3577b34da6a3ce929d0e0e4736";

    fn record(status: u16, sampled: bool) -> RequestRecord {
        let mut record = RequestRecord::new("users/get".into(), "get".into(), "f".into());
        record.trace = Some(TraceRecord {
            trace_id: TRACE.into(),
            span_id: "00f067aa0ba902b7".into(),
            sampled,
            backend_trace_id: None,
            tracestate: Some("rojo=«redacted:k»".into()),
            baggage: None,
            start_ms: 1_000,
            end_ms: 1_050,
            link: None,
        });
        record.exchange = Some(ExchangeRecord {
            request: SentRequest {
                method: "GET".into(),
                url: "http://127.0.0.1:8080/users?token=abc&page=2".into(),
                headers: Vec::new(),
                body: None,
            },
            response: ResponseRecord {
                status,
                http_version: "HTTP/1.1".into(),
                headers: Vec::new(),
                body: None,
                body_bytes: 0,
                body_truncated: false,
                binary: false,
            },
            timings: TimingsRecord {
                dns_ms: None,
                connect_ms: Some(2.0),
                tls_ms: None,
                ttfb_ms: 40.0,
                download_ms: 3.0,
                total_ms: 45.0,
                auth_ms: None,
            },
            redirects: Vec::new(),
            insecure: false,
            server_timings: Vec::new(),
        });
        record
    }

    fn attribute<'a>(span: &'a ClientSpan, key: &str) -> Option<&'a SpanValue> {
        span.attributes
            .iter()
            .find(|(k, _)| *k == key)
            .map(|(_, v)| v)
    }

    #[test]
    fn a_span_carries_the_http_conventions() {
        let span = ClientSpan::from_record(&record(200, true)).unwrap();
        assert_eq!(span.trace_id.to_string(), TRACE);
        assert_eq!(span.span_id.to_string(), "00f067aa0ba902b7");
        assert_eq!(span.name, "GET");
        assert_eq!(
            (span.start_unix_nanos, span.end_unix_nanos),
            (1_000_000_000, 1_050_000_000)
        );
        let str = |v: &str| Some(SpanValue::Str(v.to_owned()));
        assert_eq!(attribute(&span, "http.request.method").cloned(), str("GET"));
        assert_eq!(
            attribute(&span, "url.full").cloned(),
            str("http://127.0.0.1:8080/users?token=«redacted:token»&page=«redacted:page»")
        );
        assert_eq!(
            attribute(&span, "http.response.status_code"),
            Some(&SpanValue::Int(200))
        );
        assert_eq!(
            attribute(&span, "server.address").cloned(),
            str("127.0.0.1")
        );
        assert_eq!(attribute(&span, "server.port"), Some(&SpanValue::Int(8080)));
        assert_eq!(
            attribute(&span, "network.protocol.version").cloned(),
            str("1.1")
        );
        assert_eq!(attribute(&span, "error.type"), None);
        assert!(!span.error);
        let names: Vec<_> = span.events.iter().map(|e| e.name).collect();
        assert_eq!(names, ["connect", "waiting", "download"]);
        assert_eq!(span.events[1].unix_nanos, 1_000_000_000 + 42_000_000);
    }

    #[test]
    fn headers_are_never_attributes() {
        let mut r = record(200, true);
        let exchange = r.exchange.as_mut().unwrap();
        exchange.request.headers.push(crate::run::HeaderRecord {
            name: "Authorization".into(),
            value: "Bearer «redacted:token»".into(),
        });
        let span = ClientSpan::from_record(&r).unwrap();
        assert!(
            span.attributes
                .iter()
                .all(|(k, _)| !k.starts_with("http.request.header"))
        );
    }

    #[test]
    fn errors_unsampled_and_untraced_exchanges() {
        let span = ClientSpan::from_record(&record(503, true)).unwrap();
        assert!(span.error);
        assert_eq!(
            attribute(&span, "error.type"),
            Some(&SpanValue::Str("503".into()))
        );
        assert_eq!(ClientSpan::from_record(&record(200, false)), None);
        let mut untraced = record(200, true);
        untraced.trace = None;
        assert_eq!(ClientSpan::from_record(&untraced), None);
        let mut no_response = record(200, true);
        no_response.exchange = None;
        assert_eq!(ClientSpan::from_record(&no_response), None);
    }

    #[test]
    fn urls_lose_credentials_fragments_and_query_values() {
        assert_eq!(
            url_full("https://u:p@h/x?a=1&flag#frag"),
            "https://REDACTED:REDACTED@h/x?a=«redacted:a»&«redacted»"
        );
        // Regression: a bare-token query reached `url.full` verbatim.
        assert_eq!(url_full("http://h/p?s3cr3tTOKEN"), "http://h/p?«redacted»");
        assert_eq!(url_full("http://h/p"), "http://h/p");
        assert_eq!(url_full("not a url?k=v"), "not a url?k=«redacted:k»");
    }
}
