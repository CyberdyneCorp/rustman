//! The JSON Schema of `rustman.report/v1`, published at
//! `docs/schemas/report.schema.json` and printed by `rustman report schema`.
//!
//! The `kind: run` part is the contract `bootstrap-core-engine` published,
//! kept verbatim in `run.schema.json` so every old run report stays valid.
//! The `kind: exploration`, `kind: contract` and `kind: bench` parts are
//! generated with `schemars` from [`ExplorationReport`], [`ContractReport`]
//! and [`BenchReport`], so they cannot drift from the Rust types.

use schemars::JsonSchema;
use schemars::generate::SchemaSettings;
use serde_json::{Map, Value, json};

use crate::bench::{BenchReport, KIND_BENCH};
use crate::contract::{ContractReport, KIND_CONTRACT};
use crate::exploration::{ExplorationReport, KIND_EXPLORATION};
use crate::report::SCHEMA;

const RUN_DEFS: &str = include_str!("run.schema.json");
const ID: &str = "https://github.com/CyberdyneCorp/rustman/docs/schemas/report.schema.json";

/// The report JSON Schema (draft 2020-12), pretty-printed, ending in a newline.
pub fn report_schema() -> String {
    let mut defs: Map<String, Value> =
        serde_json::from_str(RUN_DEFS).expect("run.schema.json is a JSON object");
    let exploration = generated::<ExplorationReport>(KIND_EXPLORATION, &mut defs);
    defs.insert("exploration_report".to_owned(), exploration);
    let contract = generated::<ContractReport>(KIND_CONTRACT, &mut defs);
    defs.insert("contract_report".to_owned(), contract);
    let bench = generated::<BenchReport>(KIND_BENCH, &mut defs);
    defs.insert("bench_report".to_owned(), bench);
    let schema = json!({
        "$schema": "https://json-schema.org/draft/2020-12/schema",
        "$id": ID,
        "title": SCHEMA,
        "description": "A Rustman report: a run (kind: run), an exploration (kind: exploration), a contract check (kind: contract) or a load test (kind: bench). Every text field is redacted: secret values appear as «redacted:<name>».",
        "oneOf": [
            { "$ref": "#/$defs/run_report" },
            { "$ref": "#/$defs/exploration_report" },
            { "$ref": "#/$defs/contract_report" },
            { "$ref": "#/$defs/bench_report" }
        ],
        "$defs": defs,
    });
    let mut text = serde_json::to_string_pretty(&schema).expect("schemas serialize");
    text.push('\n');
    text
}

/// The generated schema of a report of `kind`, with `schema` and `kind`
/// pinned to their constants; its definitions are moved into `defs`.
fn generated<T: JsonSchema>(kind: &str, defs: &mut Map<String, Value>) -> Value {
    let generator = SchemaSettings::draft2020_12().into_generator();
    let root = generator.into_root_schema_for::<T>();
    let Value::Object(mut map) = root.to_value() else {
        unreachable!("a struct's schema is an object")
    };
    if let Some(Value::Object(props)) = map.get_mut("properties") {
        props.insert("schema".to_owned(), json!({ "const": SCHEMA }));
        props.insert("kind".to_owned(), json!({ "const": kind }));
    }
    if let Some(Value::Object(generated)) = map.remove("$defs") {
        defs.extend(generated);
    }
    map.remove("$schema");
    map.remove("title");
    Value::Object(map)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_schema_holds_every_kind() {
        let schema: Value = serde_json::from_str(&report_schema()).unwrap();
        let defs = schema["$defs"].as_object().unwrap();
        assert!(defs.contains_key("run_report"));
        assert!(defs.contains_key("exploration_report"));
        assert!(defs.contains_key("contract_report"));
        assert!(defs.contains_key("ContractItem"));
        assert_eq!(
            defs["contract_report"]["properties"]["kind"]["const"],
            "contract"
        );
        assert!(defs.contains_key("ReportFinding"));
        assert_eq!(defs["bench_report"]["properties"]["kind"]["const"], "bench");
        assert!(defs.contains_key("ThresholdResult"));
        assert_eq!(
            defs["exploration_report"]["properties"]["kind"]["const"],
            "exploration"
        );
    }
}
