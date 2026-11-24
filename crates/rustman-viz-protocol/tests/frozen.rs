//! `rustman-viz/1` is a frozen public contract (`docs/stability.md`).
//!
//! The protocol's surface, one line per protocol name, message type, message
//! field with its JSON type, required field, and enum value, must contain
//! every line of `schema/frozen/rustman-viz-1.txt`. New message types, new
//! optional fields and new enum values are fine; removing or renaming
//! anything frozen, changing a field's type, or making a new field required
//! on a frozen message (which older producers do not send) fails this test.
//! Every message type must also decode from its example in
//! `docs/viz-protocol.md`, so the schema and the Rust types cannot part.
//!
//! After an additive edit, refresh the frozen file with
//! `RUSTMAN_FREEZE_VIZ=1 cargo test -p rustman-viz-protocol --test frozen`;
//! never remove lines from it within a major.

use std::collections::BTreeSet;
use std::path::PathBuf;

use rustman_viz_protocol::{
    MAJOR, MAX_LINE_BYTES, Outcome, PROTOCOL, ReportKind, RunState, SCHEMA, Severity,
    VULKAN_UNAVAILABLE, View, decode_line,
};
use serde::Serialize;
use serde_json::Value;

fn frozen_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("schema/frozen/rustman-viz-1.txt")
}

/// The JSON type of a schema property.
fn json_type(property: &Value) -> String {
    match property.get("type") {
        Some(Value::String(t)) => t.clone(),
        Some(Value::Array(types)) => {
            let names: Vec<&str> = types.iter().filter_map(Value::as_str).collect();
            names.join("|")
        }
        _ => "any".to_owned(),
    }
}

/// The message types of the schema, each with its `then` branch.
fn messages(schema: &Value) -> Vec<(String, &Value)> {
    schema["allOf"]
        .as_array()
        .expect("the schema has one allOf branch per message")
        .iter()
        .map(|branch| {
            let kind = branch["if"]["properties"]["type"]["const"]
                .as_str()
                .expect("each branch is selected by a type const");
            (kind.to_owned(), &branch["then"])
        })
        .collect()
}

fn name<T: Serialize>(value: T) -> String {
    serde_json::to_value(value)
        .unwrap()
        .as_str()
        .unwrap()
        .to_owned()
}

/// The surface of this build: the schema's messages and the Rust enums.
fn surface() -> BTreeSet<String> {
    let schema: Value = serde_json::from_str(SCHEMA).unwrap();
    let mut lines = BTreeSet::from([
        format!("protocol {PROTOCOL}"),
        format!("major {MAJOR}"),
        format!("max-line-bytes {MAX_LINE_BYTES}"),
        format!("error-code {VULKAN_UNAVAILABLE}"),
    ]);
    for (kind, then) in messages(&schema) {
        lines.insert(format!("message {kind}"));
        if let Some(properties) = then["properties"].as_object() {
            for (field, property) in properties {
                lines.insert(format!(
                    "message {kind} field {field}: {}",
                    json_type(property)
                ));
            }
        }
        for field in then["required"].as_array().into_iter().flatten() {
            lines.insert(format!(
                "message {kind} requires {}",
                field.as_str().unwrap()
            ));
        }
    }
    let views = [
        View::RunDashboard,
        View::ExplorationDashboard,
        View::ApiGraph,
        View::ResponseInspector,
        View::Bench,
        View::Stress,
        View::MonitorTrend,
        View::ChainProbe,
    ];
    lines.extend(views.map(|v| format!("view {}", name(v))));
    let kinds = [
        ReportKind::Run,
        ReportKind::Exploration,
        ReportKind::Bench,
        ReportKind::Stress,
    ];
    lines.extend(kinds.map(|k| format!("report-kind {}", name(k))));
    let outcomes = [
        Outcome::Passed,
        Outcome::Failed,
        Outcome::Errored,
        Outcome::Cancelled,
        Outcome::Skipped,
    ];
    lines.extend(outcomes.map(|o| format!("outcome {}", name(o))));
    let severities = [
        Severity::Critical,
        Severity::High,
        Severity::Medium,
        Severity::Low,
        Severity::Info,
    ];
    lines.extend(severities.map(|s| format!("severity {}", name(s))));
    lines
        .extend([RunState::Completed, RunState::Aborted].map(|s| format!("run-state {}", name(s))));
    lines
}

/// The message types that decode from the examples of the protocol doc.
fn documented_kinds() -> BTreeSet<&'static str> {
    let doc = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../docs/viz-protocol.md");
    let text = std::fs::read_to_string(doc).unwrap();
    let mut inside = false;
    let mut kinds = BTreeSet::new();
    for line in text.lines().map(str::trim) {
        match line {
            "```json" => inside = true,
            "```" => inside = false,
            l if inside && !l.is_empty() => kinds.extend(decode_line(l).unwrap().kind()),
            _ => {}
        }
    }
    kinds
}

#[test]
fn edits_within_major_1_are_additive() {
    let current = surface();
    if std::env::var_os("RUSTMAN_FREEZE_VIZ").is_some() {
        let mut text = String::from(
            "# The frozen surface of rustman-viz/1 (see tests/frozen.rs). Never remove lines.\n",
        );
        for line in &current {
            text.push_str(line);
            text.push('\n');
        }
        std::fs::write(frozen_path(), text).unwrap();
    }
    let frozen = std::fs::read_to_string(frozen_path()).unwrap();
    let frozen: BTreeSet<&str> = frozen
        .lines()
        .filter(|line| !line.trim().is_empty() && !line.starts_with('#'))
        .collect();
    let missing: Vec<&&str> = frozen
        .iter()
        .filter(|line| !current.contains(**line))
        .collect();
    assert!(
        missing.is_empty(),
        "breaking changes to rustman-viz/1: {missing:#?}"
    );
    let new_required: Vec<&String> = current
        .iter()
        .filter(|line| line.contains(" requires ") && !frozen.contains(line.as_str()))
        .filter(|line| {
            let kind = line.split(' ').nth(1).unwrap();
            frozen.contains(format!("message {kind}").as_str())
        })
        .collect();
    assert!(
        new_required.is_empty(),
        "new required fields on frozen messages: {new_required:#?}"
    );
}

#[test]
fn every_message_type_decodes_from_its_documented_example() {
    let schema: Value = serde_json::from_str(SCHEMA).unwrap();
    let documented = documented_kinds();
    for (kind, _) in messages(&schema) {
        assert!(
            documented.contains(kind.as_str()),
            "{kind} has no example in docs/viz-protocol.md that this build decodes"
        );
    }
}
