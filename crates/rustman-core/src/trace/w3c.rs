//! W3C Trace Context: the `traceparent` and `tracestate` headers.

use super::ids::{SpanId, TraceContext, TraceId, is_lower_hex};

pub const TRACEPARENT: &str = "traceparent";
pub const TRACESTATE: &str = "tracestate";

/// The most list members a `tracestate` may carry.
const MAX_MEMBERS: usize = 32;

/// `00-<trace id>-<span id>-<flags>`.
pub fn traceparent(ctx: &TraceContext) -> String {
    let flags = if ctx.sampled { "01" } else { "00" };
    format!("00-{}-{}-{flags}", ctx.trace_id, ctx.span_id)
}

/// Parse a `traceparent` value. Versions after `00` are read by their
/// first four fields, as the specification asks; version `ff` is invalid.
pub fn parse_traceparent(value: &str) -> Option<TraceContext> {
    let value = value.trim();
    let head = value.get(..55)?;
    let rest = &value[55..];
    let mut fields = head.split('-');
    let (version, trace, span, flags) = (
        fields.next()?,
        fields.next()?,
        fields.next()?,
        fields.next()?,
    );
    let shape_ok = version.len() == 2 && flags.len() == 2 && is_lower_hex(version);
    if !shape_ok || !is_lower_hex(flags) || version == "ff" {
        return None;
    }
    if (version == "00" && !rest.is_empty()) || !(rest.is_empty() || rest.starts_with('-')) {
        return None;
    }
    let flags = u8::from_str_radix(flags, 16).ok()?;
    Some(TraceContext {
        trace_id: TraceId::from_hex(trace)?,
        span_id: SpanId::from_hex(span)?,
        sampled: flags & 1 == 1,
    })
}

/// Join `members` into a `tracestate` value; every key and value must be
/// valid ([`check_member`]).
pub fn tracestate(members: &[(String, String)]) -> String {
    members
        .iter()
        .map(|(k, v)| format!("{k}={v}"))
        .collect::<Vec<_>>()
        .join(",")
}

/// Parse a `tracestate` value into its members, or `None` when one is
/// invalid, a key repeats, or there are more than 32.
pub fn parse_tracestate(value: &str) -> Option<Vec<(String, String)>> {
    let mut members: Vec<(String, String)> = Vec::new();
    for member in value.split(',').map(|m| m.trim_matches([' ', '\t'])) {
        if member.is_empty() {
            continue;
        }
        let (key, val) = member.split_once('=')?;
        check_member(key, val).ok()?;
        if members.iter().any(|(k, _)| k == key) {
            return None;
        }
        members.push((key.to_owned(), val.to_owned()));
    }
    (members.len() <= MAX_MEMBERS).then_some(members)
}

/// Check one `tracestate` list member, with the reason it is invalid.
pub fn check_member(key: &str, value: &str) -> Result<(), String> {
    if !is_key(key) {
        return Err(format!("invalid tracestate key '{key}'"));
    }
    if !is_value(value) {
        return Err(format!("invalid tracestate value for '{key}'"));
    }
    Ok(())
}

fn is_key_char(c: u8) -> bool {
    matches!(c, b'a'..=b'z' | b'0'..=b'9' | b'_' | b'-' | b'*' | b'/')
}

fn is_key(key: &str) -> bool {
    match key.split_once('@') {
        None => key.len() <= 256 && key.starts_with(|c: char| c.is_ascii_lowercase()) && tail(key),
        Some((tenant, system)) => {
            let tenant_ok = (1..=241).contains(&tenant.len())
                && tenant.starts_with(|c: char| c.is_ascii_lowercase() || c.is_ascii_digit())
                && tail(tenant);
            let system_ok = (1..=14).contains(&system.len())
                && system.starts_with(|c: char| c.is_ascii_lowercase())
                && tail(system);
            tenant_ok && system_ok
        }
    }
}

fn tail(part: &str) -> bool {
    part.bytes().all(is_key_char)
}

fn is_value(value: &str) -> bool {
    let chars_ok = value
        .bytes()
        .all(|c| (0x20..=0x7e).contains(&c) && c != b',' && c != b'=');
    chars_ok && (1..=256).contains(&value.len()) && !value.ends_with(' ')
}

#[cfg(test)]
mod tests {
    use super::*;

    const VALID: &str = "00-0af7651916cd43dd8448eb211c80319c-b7ad6b7169203331-01";

    #[test]
    fn the_specification_example_parses_and_round_trips() {
        let ctx = parse_traceparent(VALID).unwrap();
        assert_eq!(ctx.trace_id.to_string(), "0af7651916cd43dd8448eb211c80319c");
        assert_eq!(ctx.span_id.to_string(), "b7ad6b7169203331");
        assert!(ctx.sampled);
        assert_eq!(traceparent(&ctx), VALID);
        let unsampled = "00-0af7651916cd43dd8448eb211c80319c-b7ad6b7169203331-00";
        assert!(!parse_traceparent(unsampled).unwrap().sampled);
        assert_eq!(
            traceparent(&parse_traceparent(unsampled).unwrap()),
            unsampled
        );
    }

    #[test]
    fn invalid_traceparents_from_the_test_suite_are_rejected() {
        let invalid = [
            "",
            "00-0af7651916cd43dd8448eb211c80319c-b7ad6b7169203331",
            "00-00000000000000000000000000000000-b7ad6b7169203331-01",
            "00-0af7651916cd43dd8448eb211c80319c-0000000000000000-01",
            "00-0AF7651916CD43DD8448EB211C80319C-B7AD6B7169203331-01",
            "ff-0af7651916cd43dd8448eb211c80319c-b7ad6b7169203331-01",
            "00-0af7651916cd43dd8448eb211c80319c-b7ad6b7169203331-01-extra",
            "00-0af7651916cd43dd8448eb211c80319-b7ad6b7169203331-01-",
            "00-0af7651916cd43dd8448eb211c80319c-b7ad6b716920333-01-",
            "00-0af7651916cd43dd8448eb211c80319c-b7ad6b7169203331-0g",
            "0g-0af7651916cd43dd8448eb211c80319c-b7ad6b7169203331-01",
            "00_0af7651916cd43dd8448eb211c80319c-b7ad6b7169203331-01",
        ];
        for value in invalid {
            assert!(parse_traceparent(value).is_none(), "{value}");
        }
    }

    #[test]
    fn future_versions_are_read_by_their_known_fields() {
        let future =
            "cc-0af7651916cd43dd8448eb211c80319c-b7ad6b7169203331-01-what-the-future-holds";
        assert!(parse_traceparent(future).unwrap().sampled);
        let glued = "cc-0af7651916cd43dd8448eb211c80319c-b7ad6b7169203331-01x";
        assert!(parse_traceparent(glued).is_none());
        let flags = "01-0af7651916cd43dd8448eb211c80319c-b7ad6b7169203331-09";
        assert!(parse_traceparent(flags).unwrap().sampled);
    }

    #[test]
    fn tracestate_members_follow_the_grammar() {
        let parsed = parse_tracestate("rojo=00f067aa0ba902b7, congo=t61rcWkgMzE").unwrap();
        assert_eq!(
            parsed,
            [
                ("rojo".to_owned(), "00f067aa0ba902b7".to_owned()),
                ("congo".to_owned(), "t61rcWkgMzE".to_owned()),
            ]
        );
        assert_eq!(
            tracestate(&parsed),
            "rojo=00f067aa0ba902b7,congo=t61rcWkgMzE"
        );
        assert!(parse_tracestate("fw529a3039@dt=abc,a=1").is_some());
        assert!(parse_tracestate("a=1,,b=2").is_some());
        for bad in [
            "Rojo=1", "1a=1", "a=", "a=b,a=c", "a=x=y", "a@=1", "@b=1", "a=x\ty",
        ] {
            assert!(parse_tracestate(bad).is_none(), "{bad}");
        }
        let too_many: Vec<String> = (0..33).map(|i| format!("k{i}=v")).collect();
        assert!(parse_tracestate(&too_many.join(",")).is_none());
        assert!(parse_tracestate(&too_many[..32].join(",")).is_some());
    }

    #[test]
    fn member_errors_name_the_key() {
        assert_eq!(
            check_member("Bad", "v").unwrap_err(),
            "invalid tracestate key 'Bad'"
        );
        assert_eq!(
            check_member("ok", "a,b").unwrap_err(),
            "invalid tracestate value for 'ok'"
        );
        assert!(check_member("vendor@sys", "x y").is_ok());
    }
}
