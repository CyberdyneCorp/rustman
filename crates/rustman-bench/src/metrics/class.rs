//! Error classes: why a finished request counts as an error.

use std::fmt;

use rustman_core::data_rows::secret_by_name;
use rustman_core::run::{RequestRecord, RequestStatus};
use rustman_core::secret::placeholder;
use serde::Serialize;

/// Why a request counts as an error.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
pub enum ErrorClass {
    #[serde(rename = "4xx")]
    Status4xx,
    #[serde(rename = "5xx")]
    Status5xx,
    #[serde(rename = "timeout")]
    Timeout,
    #[serde(rename = "connect")]
    Connect,
    #[serde(rename = "tls")]
    Tls,
    #[serde(rename = "blocked")]
    Blocked,
    #[serde(rename = "assertion")]
    Assertion,
    /// The request could not be built or resolved.
    #[serde(rename = "other")]
    Other,
}

impl ErrorClass {
    /// Every class, in report order.
    pub const ALL: [Self; 8] = [
        Self::Status4xx,
        Self::Status5xx,
        Self::Timeout,
        Self::Connect,
        Self::Tls,
        Self::Blocked,
        Self::Assertion,
        Self::Other,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Self::Status4xx => "4xx",
            Self::Status5xx => "5xx",
            Self::Timeout => "timeout",
            Self::Connect => "connect",
            Self::Tls => "tls",
            Self::Blocked => "blocked",
            Self::Assertion => "assertion",
            Self::Other => "other",
        }
    }

    /// The class of a finished request; `None` when it succeeded. A 4xx or
    /// 5xx response is an error even when its assertions passed.
    pub fn of(record: &RequestRecord) -> Option<Self> {
        let status = record.exchange.as_ref().map(|e| e.response.status);
        match (status, record.status) {
            (Some(500..), _) => Some(Self::Status5xx),
            (Some(400..), _) => Some(Self::Status4xx),
            (_, RequestStatus::Failed) => Some(Self::Assertion),
            (_, RequestStatus::Errored) => Some(Self::of_error(record.error.as_deref())),
            _ => None,
        }
    }

    /// The class of an error message, from the prefixes the engine uses:
    /// `cannot connect to <host>:<port>: ...` (refused, unreachable),
    /// `dns: ...`, `proxy: ...` and `network: ...` are connection failures.
    fn of_error(message: Option<&str>) -> Self {
        let message = message.unwrap_or_default();
        if message.starts_with("blocked:") {
            Self::Blocked
        } else if message.starts_with("timeout after") {
            Self::Timeout
        } else if message.starts_with("tls:") {
            Self::Tls
        } else if CONNECT_PREFIXES.iter().any(|p| message.starts_with(p)) {
            Self::Connect
        } else {
            Self::Other
        }
    }
}

/// The prefixes of the engine's connection failures.
const CONNECT_PREFIXES: [&str; 4] = ["cannot connect to ", "dns:", "network:", "proxy:"];

impl fmt::Display for ErrorClass {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

/// What an error sample says about a failed request: its error, else its
/// first failed assertion, else its exchange (`GET <url> -> 503`). The
/// record's texts are already redacted; the exchange's URL also has the
/// values of query parameters named like a secret scrubbed
/// ([`scrub_query`]), since a plain `--var` there is not a known secret.
pub fn describe(record: &RequestRecord) -> String {
    if let Some(error) = &record.error {
        return error.clone();
    }
    if let Some(failed) = record.assertions.iter().find(|a| !a.passed()) {
        return failed.to_string();
    }
    match &record.exchange {
        Some(e) => format!(
            "{} {} -> {}",
            e.request.method,
            scrub_query(&e.request.url),
            e.response.status
        ),
        None => "request failed".to_owned(),
    }
}

/// `url` with the value of each query parameter whose name looks like a
/// secret (`password`, `token`, `secret`, `api_key`, as for data columns:
/// [`secret_by_name`]) replaced by `«redacted:<name>»`. Everything else is
/// kept as written.
pub fn scrub_query(url: &str) -> String {
    let (head, fragment) = url
        .split_once('#')
        .map_or((url, None), |(h, f)| (h, Some(f)));
    let Some((base, query)) = head.split_once('?') else {
        return url.to_owned();
    };
    let pairs: Vec<String> = query.split('&').map(scrub_pair).collect();
    let mut out = format!("{base}?{}", pairs.join("&"));
    if let Some(fragment) = fragment {
        out.push('#');
        out.push_str(fragment);
    }
    out
}

fn scrub_pair(pair: &str) -> String {
    let Some((key, _)) = pair.split_once('=') else {
        return pair.to_owned();
    };
    let name = url::form_urlencoded::parse(key.as_bytes())
        .next()
        .map_or_else(|| key.to_owned(), |(k, _)| k.into_owned());
    if secret_by_name(&name) {
        format!("{key}={}", placeholder(&name))
    } else {
        pair.to_owned()
    }
}

#[cfg(test)]
mod tests {
    use rustman_core::run::{
        ExchangeRecord, RequestRecord, ResponseRecord, SentRequest, TimingsRecord,
    };

    use super::*;

    fn record(status: RequestStatus, error: Option<&str>) -> RequestRecord {
        let mut record = RequestRecord::new("a".into(), "a".into(), "a.rman.yaml".into());
        record.status = status;
        record.error = error.map(str::to_owned);
        record
    }

    fn exchange(status: u16) -> ExchangeRecord {
        ExchangeRecord {
            request: SentRequest {
                method: "GET".into(),
                url: "http://h/".into(),
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
            timings: TimingsRecord::default(),
            redirects: Vec::new(),
            insecure: false,
            server_timings: Vec::new(),
        }
    }

    #[test]
    fn errors_are_classified() {
        let cases = [
            (RequestStatus::Passed, None, None),
            (RequestStatus::Failed, None, Some(ErrorClass::Assertion)),
            (
                RequestStatus::Errored,
                Some("blocked: host not allowed (x)"),
                Some(ErrorClass::Blocked),
            ),
            (
                RequestStatus::Errored,
                Some("timeout after 10 ms (response)"),
                Some(ErrorClass::Timeout),
            ),
            (
                RequestStatus::Errored,
                Some("tls: bad"),
                Some(ErrorClass::Tls),
            ),
            (
                RequestStatus::Errored,
                Some("network: connection refused"),
                Some(ErrorClass::Connect),
            ),
            (
                RequestStatus::Errored,
                Some("cannot connect to 127.0.0.1:9: Connection refused (os error 61)"),
                Some(ErrorClass::Connect),
            ),
            (
                RequestStatus::Errored,
                Some("dns: cannot resolve nowhere.invalid: failed to lookup address"),
                Some(ErrorClass::Connect),
            ),
            (
                RequestStatus::Errored,
                Some("proxy: tunnel refused (407)"),
                Some(ErrorClass::Connect),
            ),
            (
                RequestStatus::Errored,
                Some("unresolved"),
                Some(ErrorClass::Other),
            ),
        ];
        for (status, error, class) in cases {
            assert_eq!(ErrorClass::of(&record(status, error)), class, "{error:?}");
        }
    }

    #[test]
    fn error_statuses_win_over_assertions() {
        let mut failed = record(RequestStatus::Failed, None);
        failed.exchange = Some(exchange(503));
        assert_eq!(ErrorClass::of(&failed), Some(ErrorClass::Status5xx));
        let mut passed = record(RequestStatus::Passed, None);
        passed.exchange = Some(exchange(404));
        assert_eq!(ErrorClass::of(&passed), Some(ErrorClass::Status4xx));
        passed.exchange = Some(exchange(204));
        assert_eq!(ErrorClass::of(&passed), None);
    }

    #[test]
    fn samples_describe_the_error_then_the_exchange() {
        let errored = record(RequestStatus::Errored, Some("timeout after 5 ms"));
        assert_eq!(describe(&errored), "timeout after 5 ms");
        let mut status = record(RequestStatus::Passed, None);
        status.exchange = Some(exchange(503));
        assert_eq!(describe(&status), "GET http://h/ -> 503");
        assert_eq!(
            describe(&record(RequestStatus::Errored, None)),
            "request failed"
        );
    }

    #[test]
    fn samples_scrub_query_values_named_like_secrets() {
        let mut status = record(RequestStatus::Passed, None);
        let mut sent = exchange(500);
        sent.request.url = "http://h/err?password=VARSECRET&page=2&Api-Key=K1#top".into();
        status.exchange = Some(sent);
        let text = describe(&status);
        assert_eq!(
            text,
            "GET http://h/err?password=«redacted:password»&page=2\
             &Api-Key=«redacted:Api-Key»#top -> 500"
        );
        assert!(
            !text.contains("VARSECRET") && !text.contains("K1"),
            "{text}"
        );
        assert_eq!(scrub_query("http://h/x"), "http://h/x");
        assert_eq!(scrub_query("http://h/x?flag&q=1"), "http://h/x?flag&q=1");
    }
}
