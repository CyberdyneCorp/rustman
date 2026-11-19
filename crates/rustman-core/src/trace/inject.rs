//! Adding trace headers to one outgoing request.

use rand::Rng;

use super::b3::{self, B3};
use super::baggage::{self, BAGGAGE};
use super::ids::TraceContext;
use super::w3c::{self, TRACEPARENT, TRACESTATE};
use super::{PropagationFormat, TracePropagation};

/// Headers or metadata trace context is injected into: HTTP headers built
/// by `rustman-http`, or the metadata of transports that do not use its
/// request builder, such as native gRPC.
pub trait TraceContextInjector {
    /// The value of `name`, matched without regard to case.
    fn get(&self, name: &str) -> Option<String>;
    /// Add `name: value`; only called when [`Self::get`] found none.
    fn set(&mut self, name: &str, value: String);
}

/// Add the trace headers `propagation` asks for to `carrier`, with ids
/// drawn from `rng`, and return the context sent. A trace context the
/// request already carries (`traceparent`, `b3` or `X-B3-*`) wins: its
/// headers are left unchanged, its ids are returned, and any other format
/// is sent with those same ids. A header already present is never replaced.
///
/// A trace header the request sets that does not parse is sent as given
/// and nothing is added: `None`, since no id the request carries can be
/// recorded or linked, and a generated one would be a second, conflicting
/// trace. A B3 sampling decision sent without ids (`b3: 0`, or
/// `X-B3-Sampled`/`X-B3-Flags` without `X-B3-TraceId`/`X-B3-SpanId`) is
/// also sent as given with nothing added, so it is never overridden.
pub fn inject(
    carrier: &mut dyn TraceContextInjector,
    propagation: &TracePropagation,
    rng: &mut impl Rng,
) -> Option<TraceContext> {
    if b3_decision_only(carrier) || carries_invalid(carrier, propagation.sampled) {
        return None;
    }
    let ctx = supplied(carrier, propagation.sampled).unwrap_or_else(|| {
        TraceContext::generate(rng, propagation.run_trace_id, propagation.sampled)
    });
    for format in &propagation.formats {
        match format {
            PropagationFormat::W3c => set_absent(carrier, TRACEPARENT, || w3c::traceparent(&ctx)),
            PropagationFormat::B3 => set_absent(carrier, B3, || b3::single(&ctx)),
            PropagationFormat::B3Multi => set_multi(carrier, &ctx),
        }
    }
    if !propagation.tracestate.is_empty() {
        set_absent(carrier, TRACESTATE, || {
            w3c::tracestate(&propagation.tracestate)
        });
    }
    if !propagation.baggage.is_empty() {
        set_absent(carrier, BAGGAGE, || baggage::encode(&propagation.baggage));
    }
    Some(ctx)
}

/// [`inject`] with the thread-local CSPRNG.
pub fn inject_random(
    carrier: &mut dyn TraceContextInjector,
    propagation: &TracePropagation,
) -> Option<TraceContext> {
    inject(carrier, propagation, &mut rand::rng())
}

/// Whether the request sets a trace context header ([`TRACEPARENT`], `b3`
/// or `X-B3-TraceId`/`X-B3-SpanId`) and none of them parses. A B3
/// sampling decision without ids is valid B3, not an invalid header.
pub fn carries_invalid(carrier: &dyn TraceContextInjector, deferred: bool) -> bool {
    let names = [TRACEPARENT, B3, b3::TRACE_ID, b3::SPAN_ID];
    names.iter().any(|name| carrier.get(name).is_some())
        && supplied(carrier, deferred).is_none()
        && !b3_decision_only(carrier)
}

/// Whether the request's only trace context is a B3 sampling decision
/// without ids: `b3: 0|1|d`, or `X-B3-Sampled`/`X-B3-Flags` with neither
/// `X-B3-TraceId` nor `X-B3-SpanId`, and no `traceparent`.
fn b3_decision_only(carrier: &dyn TraceContextInjector) -> bool {
    let has = |name: &str| carrier.get(name).is_some();
    if has(TRACEPARENT) || has(b3::TRACE_ID) || has(b3::SPAN_ID) {
        return false;
    }
    match carrier.get(B3) {
        Some(value) => b3::decision_only(&value),
        None => has(b3::SAMPLED) || has(b3::FLAGS),
    }
}

/// The trace context the request already carries, if valid; a B3 header
/// that defers sampling counts as `deferred`.
pub fn supplied(carrier: &dyn TraceContextInjector, deferred: bool) -> Option<TraceContext> {
    if let Some(ctx) = carrier
        .get(TRACEPARENT)
        .and_then(|v| w3c::parse_traceparent(&v))
    {
        return Some(ctx);
    }
    if let Some(ctx) = carrier.get(B3).and_then(|v| b3::parse_single(&v, deferred)) {
        return Some(ctx);
    }
    let (trace_id, span_id) = (carrier.get(b3::TRACE_ID)?, carrier.get(b3::SPAN_ID)?);
    let sampled = carrier.get(b3::SAMPLED);
    let flags = carrier.get(b3::FLAGS);
    b3::parse_multi(
        &trace_id,
        &span_id,
        sampled.as_deref(),
        flags.as_deref(),
        deferred,
    )
}

fn set_absent(carrier: &mut dyn TraceContextInjector, name: &str, value: impl FnOnce() -> String) {
    if carrier.get(name).is_none() {
        carrier.set(name, value());
    }
}

/// The `X-B3-*` headers, unless the request sets any of them.
fn set_multi(carrier: &mut dyn TraceContextInjector, ctx: &TraceContext) {
    if b3::MULTI.iter().any(|name| carrier.get(name).is_some()) {
        return;
    }
    for (name, value) in b3::multi(ctx) {
        carrier.set(name, value);
    }
}

#[cfg(test)]
mod tests {
    use rand::SeedableRng;
    use rand::rngs::StdRng;

    use super::*;
    use crate::trace::{TraceId, ids::tests::ZerosFirst};

    #[derive(Default)]
    struct Headers(Vec<(String, String)>);

    impl TraceContextInjector for Headers {
        fn get(&self, name: &str) -> Option<String> {
            self.0
                .iter()
                .find(|(n, _)| n.eq_ignore_ascii_case(name))
                .map(|(_, v)| v.clone())
        }

        fn set(&mut self, name: &str, value: String) {
            self.0.push((name.to_owned(), value));
        }
    }

    impl Headers {
        fn with(pairs: &[(&str, &str)]) -> Self {
            Self(
                pairs
                    .iter()
                    .map(|(n, v)| ((*n).to_owned(), (*v).to_owned()))
                    .collect(),
            )
        }

        fn value(&self, name: &str) -> String {
            self.get(name)
                .unwrap_or_else(|| panic!("no {name} in {:?}", self.0))
        }
    }

    fn formats(formats: &[PropagationFormat]) -> TracePropagation {
        TracePropagation {
            formats: formats.to_vec(),
            ..TracePropagation::default()
        }
    }

    const EXPLICIT: &str = "00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01";

    #[test]
    fn w3c_by_default_with_seeded_ids() {
        let mut headers = Headers::default();
        let ctx = inject(
            &mut headers,
            &TracePropagation::default(),
            &mut StdRng::seed_from_u64(9),
        )
        .unwrap();
        assert_eq!(headers.0.len(), 1);
        assert_eq!(headers.value("traceparent"), w3c::traceparent(&ctx));
        let again = inject(
            &mut Headers::default(),
            &TracePropagation::default(),
            &mut StdRng::seed_from_u64(9),
        )
        .unwrap();
        assert_eq!(ctx, again);
        let zeros = inject(
            &mut Headers::default(),
            &TracePropagation::default(),
            &mut ZerosFirst { zeros: 40 },
        )
        .unwrap();
        assert_ne!(zeros.trace_id.bytes(), [0; 16]);
        assert_ne!(zeros.span_id.bytes(), [0; 8]);
    }

    #[test]
    fn every_format_carries_the_same_ids() {
        use PropagationFormat::{B3, B3Multi, W3c};
        let mut headers = Headers::default();
        let ctx = inject(
            &mut headers,
            &formats(&[W3c, B3, B3Multi]),
            &mut rand::rng(),
        )
        .unwrap();
        let trace = ctx.trace_id.to_string();
        assert!(headers.value("traceparent").contains(&trace));
        assert!(
            headers
                .value("b3")
                .starts_with(&format!("{trace}-{}-1", ctx.span_id))
        );
        assert_eq!(headers.value("X-B3-TraceId"), trace);
        assert_eq!(headers.value("X-B3-SpanId"), ctx.span_id.to_string());
        assert_eq!(headers.value("X-B3-Sampled"), "1");
    }

    #[test]
    fn an_explicit_traceparent_wins_and_its_ids_are_reused() {
        use PropagationFormat::{B3Multi, W3c};
        let mut headers = Headers::with(&[("TraceParent", EXPLICIT)]);
        let ctx = inject(&mut headers, &formats(&[W3c, B3Multi]), &mut rand::rng()).unwrap();
        assert_eq!(ctx.trace_id.to_string(), "4bf92f3577b34da6a3ce929d0e0e4736");
        assert_eq!(headers.value("traceparent"), EXPLICIT);
        assert_eq!(
            headers
                .0
                .iter()
                .filter(|(n, _)| n.eq_ignore_ascii_case("traceparent"))
                .count(),
            1
        );
        assert_eq!(headers.value("X-B3-SpanId"), "00f067aa0ba902b7");
    }

    #[test]
    fn explicit_b3_headers_win() {
        use PropagationFormat::W3c;
        let single = "80f198ee56343ba864fe8b2a57d3eff7-e457b5a2e4d86bd1-0";
        let mut headers = Headers::with(&[("b3", single)]);
        let ctx = inject(&mut headers, &formats(&[W3c]), &mut rand::rng()).unwrap();
        assert_eq!(
            headers.value("traceparent"),
            "00-80f198ee56343ba864fe8b2a57d3eff7-e457b5a2e4d86bd1-00"
        );
        assert!(!ctx.sampled);
        let mut headers = Headers::with(&[
            ("x-b3-traceid", "80f198ee56343ba864fe8b2a57d3eff7"),
            ("x-b3-spanid", "e457b5a2e4d86bd1"),
        ]);
        let ctx = inject(
            &mut headers,
            &formats(&[PropagationFormat::B3Multi]),
            &mut rand::rng(),
        )
        .unwrap();
        assert_eq!(ctx.span_id.to_string(), "e457b5a2e4d86bd1");
        assert_eq!(headers.0.len(), 2);
    }

    /// Regression: `b3: 0` was reported as an invalid header, while
    /// `X-B3-Sampled: 0` alone got a sampled `traceparent` that overrode
    /// the deny decision. Both are now sent as given with nothing added.
    #[test]
    fn b3_sampling_decisions_without_ids_are_sent_as_given() {
        use PropagationFormat::{B3, B3Multi, W3c};
        let all = formats(&[W3c, B3, B3Multi]);
        for pairs in [
            &[("b3", "0")][..],
            &[("b3", "d")],
            &[("X-B3-Sampled", "0")],
            &[("x-b3-flags", "1")],
        ] {
            let mut headers = Headers::with(pairs);
            assert!(!carries_invalid(&headers, true), "{pairs:?}");
            assert!(inject(&mut headers, &all, &mut rand::rng()).is_none());
            assert_eq!(headers.0.len(), 1, "{pairs:?} gained headers");
        }
        let mut bad = Headers::with(&[("b3", "2")]);
        assert!(carries_invalid(&bad, true));
        assert!(inject(&mut bad, &all, &mut rand::rng()).is_none());
    }

    /// Regression: an invalid explicit `traceparent` was sent beside `b3`
    /// headers with a generated id, which was recorded and linked although
    /// the W3C backend never saw it.
    #[test]
    fn an_invalid_explicit_header_is_kept_and_nothing_is_added() {
        let upper = "00-4BF92F3577B34DA6A3CE929D0E0E4736-00F067AA0BA902B7-01";
        let ff = "ff-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01";
        for (name, value) in [
            ("traceparent", "garbage"),
            ("traceparent", upper),
            ("traceparent", ff),
            ("b3", "nope"),
            ("X-B3-TraceId", "xyz"),
        ] {
            let mut headers = Headers::with(&[(name, value)]);
            assert!(carries_invalid(&headers, true), "{name}: {value}");
            let ctx = inject(
                &mut headers,
                &formats(&[PropagationFormat::W3c, PropagationFormat::B3]),
                &mut rand::rng(),
            );
            assert_eq!(ctx, None, "{name}: {value}");
            assert_eq!(headers.0, [(name.to_owned(), value.to_owned())]);
        }
        assert!(!carries_invalid(&Headers::default(), true));
        assert!(!carries_invalid(
            &Headers::with(&[("traceparent", EXPLICIT)]),
            true
        ));
    }

    #[test]
    fn run_scope_unsampled_tracestate_and_baggage() {
        let run = TraceId::from_hex("4bf92f3577b34da6a3ce929d0e0e4736").unwrap();
        let propagation = TracePropagation {
            sampled: false,
            run_trace_id: Some(run),
            tracestate: vec![("rojo".into(), "00f067aa0ba902b7".into())],
            baggage: vec![("tenant".into(), "acme".into())],
            ..TracePropagation::default()
        };
        let mut a = Headers::default();
        let mut b = Headers::default();
        let ca = inject(&mut a, &propagation, &mut rand::rng()).unwrap();
        let cb = inject(&mut b, &propagation, &mut rand::rng()).unwrap();
        assert_eq!((ca.trace_id, cb.trace_id), (run, run));
        assert_ne!(ca.span_id, cb.span_id);
        assert!(a.value("traceparent").ends_with("-00"));
        assert_eq!(a.value("tracestate"), "rojo=00f067aa0ba902b7");
        assert_eq!(a.value("baggage"), "tenant=acme");
        let mut own = Headers::with(&[("baggage", "mine=1")]);
        inject(&mut own, &propagation, &mut rand::rng()).unwrap();
        assert_eq!(own.value("baggage"), "mine=1");
    }
}
