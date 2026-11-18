//! Every message round-trips, matches the JSON Schema, and readers ignore
//! what they do not know. The examples in `docs/viz-protocol.md` are checked
//! the same way.

use rustman_viz_protocol::*;
use serde_json::Value;

fn samples() -> Vec<Message> {
    let blob = BlobRef {
        path: "/tmp/rustman-viz/body-01.json".into(),
        bytes: 209_715_200,
        media_type: Some("application/json".into()),
    };
    vec![
        Message::Hello(Hello::new("rustman 0.2.0")),
        Message::HelloAck(HelloAck {
            protocol: PROTOCOL.into(),
            viewer: "rustman-viz 0.2.0".into(),
            views: vec![View::RunDashboard, View::ResponseInspector],
        }),
        Message::HelloReject(HelloReject {
            supported: vec!["rustman-viz/1".into()],
        }),
        Message::OpenView(OpenView {
            view: View::ApiGraph,
            title: Some("Petstore".into()),
        }),
        Message::RunStarted(RunStarted {
            started_at: "2026-11-17T10:00:00Z".into(),
            environment: Some("staging".into()),
            total: Some(200),
        }),
        Message::ExchangeCompleted(Box::new(ExchangeCompleted {
            request: "users/get-user".into(),
            method: "GET".into(),
            url: "https://api.test/users/42".into(),
            outcome: Outcome::Passed,
            status: Some(200),
            duration_ms: 84,
            assertions: Assertions {
                passed: 3,
                failed: 0,
            },
            error: None,
            iteration: Some(1),
        })),
        Message::RunFinished(RunFinished {
            state: RunState::Completed,
            summary: RunSummary {
                total: 200,
                passed: 180,
                failed: 15,
                errored: 5,
                skipped: 0,
                duration_ms: 9_000,
            },
        }),
        Message::ExplorationStarted(ExplorationStarted {
            started_at: "2026-11-17T10:00:00Z".into(),
            operations: 24,
            base_url: None,
        }),
        Message::ProbeCompleted(ProbeCompleted {
            operation: "POST /users".into(),
            outcome: Outcome::Errored,
            status: None,
            duration_ms: 12,
        }),
        Message::Finding(Finding {
            severity: Severity::High,
            operation: "POST /users".into(),
            title: "500 on empty body".into(),
            detail: None,
        }),
        Message::ExplorationFinished(ExplorationFinished {
            state: RunState::Aborted,
            operations_total: 24,
            operations_exercised: 18,
            status_codes_documented: 40,
            status_codes_seen: 31,
        }),
        Message::LoadReport(LoadReport {
            path: "reports/run-2026-11-17.json".into(),
            kind: ReportKind::Run,
            view: None,
        }),
        Message::InspectResponse(Box::new(InspectResponse {
            request: Some("users/list".into()),
            status: Some(200),
            content_type: Some("application/json".into()),
            headers: vec![Header {
                name: "Authorization".into(),
                value: "Bearer «redacted:token»".into(),
            }],
            selector: Some("$.items".into()),
            body: Body::Inline {
                text: "[{\"id\":1}]".into(),
            },
        })),
        Message::InspectResponse(Box::new(InspectResponse {
            request: None,
            status: None,
            content_type: None,
            headers: Vec::new(),
            selector: None,
            body: Body::BlobRef(blob.clone()),
        })),
        Message::BlobRef(blob),
        Message::Error(ErrorMessage {
            code: VULKAN_UNAVAILABLE.into(),
            message: "Graphics unavailable (Vulkan)".into(),
        }),
        Message::Close(Close {}),
    ]
}

fn validator() -> jsonschema::Validator {
    let schema: Value = serde_json::from_str(SCHEMA).unwrap();
    jsonschema::validator_for(&schema).unwrap()
}

fn assert_valid(validator: &jsonschema::Validator, value: &Value) {
    let errors: Vec<String> = validator
        .iter_errors(value)
        .map(|e| e.to_string())
        .collect();
    assert!(errors.is_empty(), "{value}: {errors:?}");
}

#[test]
fn every_message_round_trips_and_matches_the_schema() {
    let validator = validator();
    let mut kinds = std::collections::HashSet::new();
    for message in samples() {
        let line = encode_line(&message).unwrap();
        assert_eq!(decode_line(&line).unwrap(), message, "{line}");
        let value: Value = serde_json::from_str(&line).unwrap();
        assert_eq!(value["type"], message.kind().unwrap());
        assert_valid(&validator, &value);
        kinds.insert(message.kind().unwrap());
    }
    assert_eq!(kinds.len(), 16, "every message type has a sample");
}

#[test]
fn the_schema_rejects_a_known_message_missing_a_field() {
    let bad: Value = serde_json::json!({"type": "hello", "protocol": "rustman-viz/1"});
    assert!(!validator().is_valid(&bad));
}

#[test]
fn unknown_fields_and_types_decode_without_error() {
    // Scenario "Unknown field ignored".
    let line = r#"{"type":"run_finished","state":"completed","future":true,
        "summary":{"total":1,"passed":1,"failed":0,"errored":0,"skipped":0,"duration_ms":5,"p99":3}}"#;
    let Message::RunFinished(finished) = decode_line(line).unwrap() else {
        panic!("expected run_finished");
    };
    assert_eq!(finished.state, RunState::Completed);
    assert_eq!(finished.summary.total, 1);

    let unknown = decode_line(r#"{"type":"heatmap_tile","x":1}"#).unwrap();
    assert_eq!(unknown, Message::Unknown);
    assert_eq!(unknown.kind(), None);
    assert!(
        encode_line(&unknown).is_err(),
        "unknown messages are never written"
    );

    let future = r#"{"type":"finding","severity":"catastrophic","operation":"GET /x","title":"t"}"#;
    let Message::Finding(finding) = decode_line(future).unwrap() else {
        panic!("expected finding");
    };
    assert_eq!(finding.severity, Severity::Unknown);
    assert_valid(&validator(), &serde_json::from_str(future).unwrap());
}

#[test]
fn findings_and_final_events_have_priority() {
    let priority: Vec<_> = samples()
        .into_iter()
        .filter(Message::is_priority)
        .map(|m| m.kind().unwrap())
        .collect();
    assert_eq!(
        priority,
        ["run_finished", "finding", "exploration_finished"]
    );
}

/// The JSON lines inside the ```json blocks of the protocol document.
fn doc_examples() -> Vec<String> {
    let doc = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/viz-protocol.md");
    let text = std::fs::read_to_string(doc).unwrap();
    let mut lines = Vec::new();
    let mut inside = false;
    for line in text.lines() {
        match line.trim() {
            "```json" => inside = true,
            "```" => inside = false,
            l if inside && !l.is_empty() => lines.push(l.to_owned()),
            _ => {}
        }
    }
    lines
}

#[test]
fn every_example_in_the_protocol_doc_parses() {
    let validator = validator();
    let examples = doc_examples();
    assert!(examples.len() >= 16, "{examples:?}");
    let mut kinds = std::collections::HashSet::new();
    for line in examples {
        let message = decode_line(&line).unwrap_or_else(|e| panic!("{line}: {e}"));
        assert_valid(&validator, &serde_json::from_str(&line).unwrap());
        kinds.extend(message.kind());
    }
    assert_eq!(
        kinds.len(),
        16,
        "the doc shows every message type: {kinds:?}"
    );
}
