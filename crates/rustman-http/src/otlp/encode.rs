//! [`ClientSpan`]s as an OTLP `ExportTraceServiceRequest`.

use opentelemetry_proto::tonic::collector::trace::v1::ExportTraceServiceRequest;
use opentelemetry_proto::tonic::common::v1::{AnyValue, InstrumentationScope, KeyValue, any_value};
use opentelemetry_proto::tonic::resource::v1::Resource;
use opentelemetry_proto::tonic::trace::v1::span::{Event, SpanKind};
use opentelemetry_proto::tonic::trace::v1::status::StatusCode;
use opentelemetry_proto::tonic::trace::v1::{ResourceSpans, ScopeSpans, Span, Status};
use prost::Message;
use rustman_core::trace::{ClientSpan, SpanValue};

/// The `flags` bit of a sampled span.
const SAMPLED: u32 = 0x01;

/// The protobuf body exporting `spans` as service `service_name`.
pub fn encode(spans: &[ClientSpan], service_name: &str) -> Vec<u8> {
    let resource = Resource {
        attributes: vec![key_value(
            "service.name",
            &SpanValue::Str(service_name.to_owned()),
        )],
        ..Resource::default()
    };
    let scope = InstrumentationScope {
        name: "rustman".to_owned(),
        version: env!("CARGO_PKG_VERSION").to_owned(),
        ..InstrumentationScope::default()
    };
    let request = ExportTraceServiceRequest {
        resource_spans: vec![ResourceSpans {
            resource: Some(resource),
            scope_spans: vec![ScopeSpans {
                scope: Some(scope),
                spans: spans.iter().map(span).collect(),
                ..ScopeSpans::default()
            }],
            ..ResourceSpans::default()
        }],
    };
    request.encode_to_vec()
}

fn span(span: &ClientSpan) -> Span {
    let status = span.error.then(|| Status {
        code: StatusCode::Error as i32,
        ..Status::default()
    });
    Span {
        trace_id: span.trace_id.bytes().to_vec(),
        span_id: span.span_id.bytes().to_vec(),
        trace_state: span.tracestate.clone().unwrap_or_default(),
        flags: SAMPLED,
        name: span.name.clone(),
        kind: SpanKind::Client as i32,
        start_time_unix_nano: span.start_unix_nanos,
        end_time_unix_nano: span.end_unix_nanos,
        attributes: span
            .attributes
            .iter()
            .map(|(k, v)| key_value(k, v))
            .collect(),
        events: span
            .events
            .iter()
            .map(|e| Event {
                time_unix_nano: e.unix_nanos,
                name: e.name.to_owned(),
                attributes: vec![key_value(
                    "rustman.phase.duration_ms",
                    &SpanValue::Double(e.duration_ms),
                )],
                ..Event::default()
            })
            .collect(),
        status,
        ..Span::default()
    }
}

fn key_value(key: &str, value: &SpanValue) -> KeyValue {
    let value = match value {
        SpanValue::Str(s) => any_value::Value::StringValue(s.clone()),
        SpanValue::Int(i) => any_value::Value::IntValue(*i),
        SpanValue::Double(d) => any_value::Value::DoubleValue(*d),
    };
    KeyValue {
        key: key.to_owned(),
        value: Some(AnyValue { value: Some(value) }),
        ..KeyValue::default()
    }
}

#[cfg(test)]
mod tests {
    use rustman_core::trace::{SpanEvent, SpanId, TraceId};

    use super::*;

    #[test]
    fn spans_round_trip_through_the_protobuf() {
        let client = ClientSpan {
            trace_id: TraceId::from_hex("4bf92f3577b34da6a3ce929d0e0e4736").unwrap(),
            span_id: SpanId::from_hex("00f067aa0ba902b7").unwrap(),
            tracestate: Some("rojo=1".into()),
            name: "GET".into(),
            start_unix_nanos: 10,
            end_unix_nanos: 20,
            attributes: vec![("http.response.status_code", SpanValue::Int(500))],
            events: vec![SpanEvent {
                name: "waiting",
                unix_nanos: 15,
                duration_ms: 0.005,
            }],
            error: true,
        };
        let body = encode(&[client], "svc");
        let back = ExportTraceServiceRequest::decode(body.as_slice()).unwrap();
        let resource = &back.resource_spans[0];
        let service = &resource.resource.as_ref().unwrap().attributes[0];
        assert_eq!(service.key, "service.name");
        let span = &resource.scope_spans[0].spans[0];
        assert_eq!(span.kind, SpanKind::Client as i32);
        assert_eq!(span.trace_id.len(), 16);
        assert_eq!(span.trace_state, "rojo=1");
        assert_eq!(span.status.as_ref().unwrap().code, StatusCode::Error as i32);
        assert_eq!(span.events[0].name, "waiting");
        assert_eq!(span.attributes[0].key, "http.response.status_code");
    }
}
