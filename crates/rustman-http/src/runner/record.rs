//! Turning an [`Exchange`] into the redacted record reporters write.

use std::fs::File;
use std::io::Read;
use std::path::Path;

use rustman_core::Redactor;
use rustman_core::captures::CaptureResult;
use rustman_core::run::{
    ExchangeRecord, HeaderRecord, RedirectRecord, ResponseRecord, SentRequest, TimingsRecord,
};
use rustman_core::vars::ResolvedRequest;

use crate::exchange::Exchange;
use crate::timing::{Timings, millis};

/// `exchange`, sent for `request`, with every secret `redactor` knows replaced.
pub fn exchange_record(
    request: &ResolvedRequest,
    exchange: &Exchange,
    redactor: &Redactor,
) -> ExchangeRecord {
    ExchangeRecord {
        request: SentRequest::new(request, redactor),
        response: response_record(exchange, redactor),
        timings: timings_record(&exchange.timings),
        redirects: exchange
            .redirects
            .iter()
            .map(|hop| RedirectRecord {
                status: hop.status,
                url: redactor.text(&hop.url).into_owned(),
            })
            .collect(),
        insecure: exchange.insecure,
    }
}

fn response_record(exchange: &Exchange, redactor: &Redactor) -> ResponseRecord {
    let headers = exchange
        .headers
        .iter()
        .map(|(name, value)| {
            let value = String::from_utf8_lossy(value.as_bytes());
            HeaderRecord {
                name: name.as_str().to_owned(),
                value: redactor.header_value(name.as_str(), &value),
            }
        })
        .collect();
    let body = (!exchange.binary).then(|| recorded_body(exchange, redactor));
    ResponseRecord {
        status: exchange.status,
        http_version: exchange.http_version.to_string(),
        headers,
        body,
        body_bytes: exchange.size_bytes,
        body_truncated: exchange.spill.is_some(),
        binary: exchange.binary,
    }
}

/// The body as recorded, redacted. For a spilled body, its preview: redacted
/// over a window running past the cut by the longest secret, then cut, so a
/// secret the cut splits is replaced whole instead of leaving its start.
fn recorded_body(exchange: &Exchange, redactor: &Redactor) -> String {
    let bytes = match &exchange.spill {
        None => redactor.bytes(&exchange.body).into_owned(),
        Some(path) => {
            let keep = exchange.body.len();
            match read_prefix(path, keep + redactor.longest()) {
                Some(window) => redactor.bytes_prefix(&window, keep),
                // Without the bytes past the cut, drop the bytes a secret
                // split by it could occupy.
                None => {
                    redactor.bytes_prefix(&exchange.body, keep.saturating_sub(redactor.longest()))
                }
            }
        }
    };
    String::from_utf8_lossy(&bytes).into_owned()
}

/// The first `len` bytes of the file at `path`.
fn read_prefix(path: &Path, len: usize) -> Option<Vec<u8>> {
    let mut window = Vec::with_capacity(len);
    let file = File::open(path).ok()?;
    file.take(len as u64).read_to_end(&mut window).ok()?;
    Some(window)
}

/// Redact the failure message of every capture.
pub fn redact_capture_messages(captures: &mut [CaptureResult], redactor: &Redactor) {
    for message in captures.iter_mut().filter_map(|c| c.message.as_mut()) {
        *message = redactor.text(message).into_owned();
    }
}

/// Milliseconds with microsecond precision, as in [`Timings`]' JSON form.
pub fn timings_record(timings: &Timings) -> TimingsRecord {
    TimingsRecord {
        dns_ms: timings.dns.map(millis),
        connect_ms: timings.connect.map(millis),
        tls_ms: timings.tls.map(millis),
        ttfb_ms: millis(timings.ttfb),
        download_ms: millis(timings.download),
        total_ms: millis(timings.total),
        auth_ms: timings.auth.map(millis),
    }
}

#[cfg(test)]
mod tests {
    use rustman_core::captures::CaptureOutcome;

    use super::*;

    #[test]
    fn capture_messages_are_redacted() {
        let mut redactor = Redactor::new();
        redactor.add_value("token", "s3cr3t-canary");
        let capture = |message: Option<&str>| CaptureResult {
            var: "id".into(),
            outcome: CaptureOutcome::NoMatch,
            message: message.map(str::to_owned),
            required: false,
            persisted: false,
            secret: false,
        };
        let mut captures = [capture(Some("no match in s3cr3t-canary")), capture(None)];
        redact_capture_messages(&mut captures, &redactor);
        let first = captures[0].message.as_deref().unwrap();
        assert!(!first.contains("s3cr3t-canary"), "{first}");
        assert!(first.contains("redacted"), "{first}");
        assert!(captures[1].message.is_none());
    }
}
