//! The JSON Schema of `rustman.report/v1`, published at
//! `docs/schemas/report.schema.json` and printed by `rustman report schema`.
//!
//! The `kind: run` part is the contract `bootstrap-core-engine` published,
//! kept verbatim in `run.schema.json` so every old run report stays valid.
//! The `kind: exploration` part is generated with `schemars` from
//! [`ExplorationReport`], so it cannot drift from the Rust types.

use schemars::generate::SchemaSettings;
use serde_json::{Map, Value, json};

use crate::exploration::ExplorationReport;
use crate::report::SCHEMA;

const RUN_DEFS: &str = include_str!("run.schema.json");
const ID: &str = "https://github.com/CyberdyneCorp/rustman/docs/schemas/report.schema.json";

/// The report JSON Schema (draft 2020-12), pretty-printed, ending in a newline.
pub fn report_schema() -> String {
    let mut defs: Map<String, Value> =
        serde_json::from_str(RUN_DEFS).expect("run.schema.json is a JSON object");
    let mut exploration = exploration_schema();
    if let Some(Value::Object(generated)) = exploration.remove("$defs") {
        defs.extend(generated);
    }
    exploration.remove("$schema");
    exploration.remove("title");
    let exploration = Value::Object(exploration);
    defs.insert("exploration_report".to_owned(), exploration);
    let schema = json!({
        "$schema": "https://json-schema.org/draft/2020-12/schema",
        "$id": ID,
        "title": SCHEMA,
        "description": "A Rustman report: a run (kind: run) or an exploration (kind: exploration). Every text field is redacted: secret values appear as «redacted:<name>».",
        "oneOf": [
            { "$ref": "#/$defs/run_report" },
            { "$ref": "#/$defs/exploration_report" }
        ],
        "$defs": defs,
    });
    let mut text = serde_json::to_string_pretty(&schema).expect("schemas serialize");
    text.push('\n');
    text
}

/// The generated schema of an exploration report, with `schema` and `kind`
/// pinned to their constants.
fn exploration_schema() -> Map<String, Value> {
    let generator = SchemaSettings::draft2020_12().into_generator();
    let root = generator.into_root_schema_for::<ExplorationReport>();
    let Value::Object(mut map) = root.to_value() else {
        unreachable!("a struct's schema is an object")
    };
    if let Some(Value::Object(props)) = map.get_mut("properties") {
        props.insert("schema".to_owned(), json!({ "const": SCHEMA }));
        props.insert(
            "kind".to_owned(),
            json!({ "const": crate::exploration::KIND_EXPLORATION }),
        );
    }
    map
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_schema_holds_both_kinds() {
        let schema: Value = serde_json::from_str(&report_schema()).unwrap();
        let defs = schema["$defs"].as_object().unwrap();
        assert!(defs.contains_key("run_report"));
        assert!(defs.contains_key("exploration_report"));
        assert!(defs.contains_key("ReportFinding"));
        assert_eq!(
            defs["exploration_report"]["properties"]["kind"]["const"],
            "exploration"
        );
    }
}
