//! B3 propagation (openzipkin/b3-propagation): the single `b3` header and
//! the `X-B3-*` headers.

use super::ids::{SpanId, TraceContext, TraceId};

pub const B3: &str = "b3";
pub const TRACE_ID: &str = "X-B3-TraceId";
pub const SPAN_ID: &str = "X-B3-SpanId";
pub const SAMPLED: &str = "X-B3-Sampled";
pub const FLAGS: &str = "X-B3-Flags";
pub const PARENT_SPAN_ID: &str = "X-B3-ParentSpanId";

/// Every multi-header name, for detecting a user-supplied B3 context.
pub const MULTI: [&str; 5] = [TRACE_ID, SPAN_ID, SAMPLED, FLAGS, PARENT_SPAN_ID];

/// `<trace id>-<span id>-<1|0>`.
pub fn single(ctx: &TraceContext) -> String {
    format!("{}-{}-{}", ctx.trace_id, ctx.span_id, sampled(ctx))
}

/// The `X-B3-TraceId`, `X-B3-SpanId` and `X-B3-Sampled` headers.
pub fn multi(ctx: &TraceContext) -> [(&'static str, String); 3] {
    [
        (TRACE_ID, ctx.trace_id.to_string()),
        (SPAN_ID, ctx.span_id.to_string()),
        (SAMPLED, sampled(ctx).to_owned()),
    ]
}

fn sampled(ctx: &TraceContext) -> &'static str {
    if ctx.sampled { "1" } else { "0" }
}

/// Parse a single `b3` header. A deferred decision (no sampling field)
/// takes `deferred`. A deny-only value (`0`) carries no ids: `None`.
pub fn parse_single(value: &str, deferred: bool) -> Option<TraceContext> {
    let mut parts = value.trim().split('-');
    let trace_id = TraceId::from_b3(parts.next()?)?;
    let span_id = span(parts.next()?)?;
    let sampled = match parts.next() {
        None => deferred,
        Some(flag) => sampling(flag)?,
    };
    if let Some(parent) = parts.next() {
        span(parent)?;
    }
    if parts.next().is_some() {
        return None;
    }
    Some(TraceContext {
        trace_id,
        span_id,
        sampled,
    })
}

/// Whether a single `b3` value is only a sampling decision (`0`, `1` or
/// `d`), with no trace or span id.
pub fn decision_only(value: &str) -> bool {
    sampling(value.trim()).is_some()
}

/// Parse the multi-header form from its `X-B3-TraceId`, `X-B3-SpanId`,
/// `X-B3-Sampled` and `X-B3-Flags` values.
pub fn parse_multi(
    trace_id: &str,
    span_id: &str,
    sampled: Option<&str>,
    flags: Option<&str>,
    deferred: bool,
) -> Option<TraceContext> {
    let debug = flags.is_some_and(|f| f.trim() == "1");
    let sampled = match sampled {
        _ if debug => true,
        None => deferred,
        Some("true") => true,
        Some("false") => false,
        Some(flag) => sampling(flag.trim())?,
    };
    Some(TraceContext {
        trace_id: TraceId::from_b3(trace_id.trim())?,
        span_id: span(span_id.trim())?,
        sampled,
    })
}

fn span(hex: &str) -> Option<SpanId> {
    SpanId::from_hex(&hex.to_ascii_lowercase())
}

fn sampling(flag: &str) -> Option<bool> {
    match flag {
        "1" | "d" => Some(true),
        "0" => Some(false),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const TRACE: &str = "80f198ee56343ba864fe8b2a57d3eff7";
    const SPAN: &str = "e457b5a2e4d86bd1";

    #[test]
    fn single_header_examples_from_the_specification() {
        let full = format!("{TRACE}-{SPAN}-1-05e3ac9a4f6e3b90");
        let ctx = parse_single(&full, false).unwrap();
        assert_eq!(ctx.trace_id.to_string(), TRACE);
        assert_eq!(ctx.span_id.to_string(), SPAN);
        assert!(ctx.sampled);
        assert_eq!(single(&ctx), format!("{TRACE}-{SPAN}-1"));
        assert!(
            parse_single(&format!("{TRACE}-{SPAN}-d"), false)
                .unwrap()
                .sampled
        );
        assert!(
            !parse_single(&format!("{TRACE}-{SPAN}-0"), true)
                .unwrap()
                .sampled
        );
        assert!(
            parse_single(&format!("{TRACE}-{SPAN}"), true)
                .unwrap()
                .sampled
        );
        assert!(
            !parse_single(&format!("{TRACE}-{SPAN}"), false)
                .unwrap()
                .sampled
        );
        let short = parse_single(&format!("64fe8b2a57d3eff7-{SPAN}-1"), false).unwrap();
        assert_eq!(
            short.trace_id.to_string(),
            "000000000000000064fe8b2a57d3eff7"
        );
    }

    #[test]
    fn invalid_single_headers_are_rejected() {
        for bad in [
            "0".to_owned(),
            "d".to_owned(),
            format!("{TRACE}-{SPAN}-x"),
            format!("{TRACE}-{SPAN}-1-zz"),
            format!("{TRACE}-{SPAN}-1-{SPAN}-extra"),
            format!("{TRACE}-0000000000000000-1"),
            format!("{}-{SPAN}-1", "0".repeat(32)),
        ] {
            assert!(parse_single(&bad, true).is_none(), "{bad}");
        }
    }

    #[test]
    fn multi_headers_carry_the_same_ids() {
        let ctx = parse_multi(TRACE, SPAN, Some("1"), None, false).unwrap();
        let headers = multi(&ctx);
        assert_eq!(headers[0], (TRACE_ID, TRACE.to_owned()));
        assert_eq!(headers[1], (SPAN_ID, SPAN.to_owned()));
        assert_eq!(headers[2], (SAMPLED, "1".to_owned()));
        assert!(
            !parse_multi(TRACE, SPAN, Some("0"), None, true)
                .unwrap()
                .sampled
        );
        assert!(
            parse_multi(TRACE, SPAN, Some("0"), Some("1"), false)
                .unwrap()
                .sampled
        );
        assert!(
            parse_multi(TRACE, SPAN, Some("true"), None, false)
                .unwrap()
                .sampled
        );
        assert!(parse_multi(TRACE, SPAN, None, None, true).unwrap().sampled);
        assert!(parse_multi(TRACE, SPAN, Some("yes"), None, true).is_none());
        assert!(parse_multi("xyz", SPAN, None, None, true).is_none());
    }
}
