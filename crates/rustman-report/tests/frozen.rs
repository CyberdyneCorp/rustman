//! `rustman.report/v1` is a frozen public contract (`docs/stability.md`).
//!
//! `docs/schemas/report.schema.json` is generated from the report types
//! (`rustman report schema` prints it and `tests/report.rs` in rustman-cli
//! checks the file matches), so a renamed or removed field would change both
//! together. This test pins the schema's surface instead: one line per
//! property, required field, JSON type, `const` and `enum` value at each
//! path of each report kind, with `$ref`s followed (so renaming a Rust type
//! behind a definition is not a change). Every line of
//! `tests/frozen/report-v1.txt` must still be there; new properties, new
//! report kinds and new enum values are fine.
//!
//! After an additive edit, refresh the frozen file with
//! `RUSTMAN_FREEZE_REPORT=1 cargo test -p rustman-report --test frozen`;
//! never remove lines from it within a major.

use std::collections::BTreeSet;
use std::path::PathBuf;

use rustman_report::report_schema;
use serde_json::Value;

fn frozen_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/frozen/report-v1.txt")
}

fn scalar(value: &Value) -> String {
    match value {
        Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

/// Walks a schema, following `$ref`s into its `$defs`.
struct Walker<'a> {
    defs: &'a serde_json::Map<String, Value>,
    lines: BTreeSet<String>,
    /// The definitions being expanded, to stop at recursive types.
    stack: Vec<&'a str>,
}

impl<'a> Walker<'a> {
    /// Adds the surface lines of `node`, found at `path`.
    fn walk(&mut self, node: &'a Value, path: &str) {
        let Value::Object(map) = node else { return };
        if let Some(Value::String(target)) = map.get("$ref") {
            self.follow(target, path);
        }
        match map.get("type") {
            Some(Value::Array(types)) => types.iter().for_each(|t| self.add(path, "type", t)),
            Some(t) => self.add(path, "type", t),
            None => {}
        }
        if let Some(value) = map.get("const") {
            self.add(path, "const", value);
        }
        for value in list(map, "enum") {
            self.add(path, "enum", value);
        }
        for field in list(map, "required") {
            self.add(path, "required", field);
        }
        if let Some(Value::Object(properties)) = map.get("properties") {
            for (name, child) in properties {
                self.lines.insert(format!("{path} property {name}"));
                self.walk(child, &format!("{path}.{name}"));
            }
        }
        if let Some(items) = map.get("items") {
            self.walk(items, &format!("{path}[]"));
        }
        for (i, item) in list(map, "prefixItems").enumerate() {
            self.walk(item, &format!("{path}[{i}]"));
        }
        if let Some(extra @ Value::Object(_)) = map.get("additionalProperties") {
            self.walk(extra, &format!("{path}{{}}"));
        }
        // Alternatives share their parent's path: a variant may be added.
        for key in ["oneOf", "anyOf", "allOf"] {
            for branch in list(map, key) {
                self.walk(branch, path);
            }
        }
    }

    fn follow(&mut self, target: &str, path: &str) {
        let name = target.trim_start_matches("#/$defs/");
        let (name, def) = self.defs.get_key_value(name).expect("refs resolve");
        if self.stack.contains(&name.as_str()) {
            return;
        }
        self.stack.push(name);
        self.walk(def, path);
        self.stack.pop();
    }

    fn add(&mut self, path: &str, what: &str, value: &Value) {
        self.lines
            .insert(format!("{path} {what} {}", scalar(value)));
    }
}

fn list<'a>(map: &'a serde_json::Map<String, Value>, key: &str) -> impl Iterator<Item = &'a Value> {
    map.get(key).and_then(Value::as_array).into_iter().flatten()
}

/// The surface of each report kind, under the name of its definition
/// (`run_report`, `exploration_report`, ...: names chosen in `schema.rs`).
fn surface() -> BTreeSet<String> {
    let schema: Value = serde_json::from_str(&report_schema()).unwrap();
    let mut walker = Walker {
        defs: schema["$defs"].as_object().unwrap(),
        lines: BTreeSet::new(),
        stack: Vec::new(),
    };
    walker
        .lines
        .insert(format!("title {}", schema["title"].as_str().unwrap()));
    for kind in schema["oneOf"].as_array().unwrap() {
        let name = kind["$ref"]
            .as_str()
            .unwrap()
            .trim_start_matches("#/$defs/");
        walker.lines.insert(format!("report {name}"));
        walker.follow(kind["$ref"].as_str().unwrap(), name);
    }
    walker.lines
}

#[test]
fn edits_within_v1_are_additive() {
    let current = surface();
    if std::env::var_os("RUSTMAN_FREEZE_REPORT").is_some() {
        let mut text = String::from(
            "# The frozen surface of rustman.report/v1 (see tests/frozen.rs). Never remove lines.\n",
        );
        for line in &current {
            text.push_str(line);
            text.push('\n');
        }
        std::fs::write(frozen_path(), text).unwrap();
    }
    let frozen = std::fs::read_to_string(frozen_path()).unwrap();
    let missing: Vec<&str> = frozen
        .lines()
        .filter(|line| !line.trim().is_empty() && !line.starts_with('#'))
        .filter(|line| !current.contains(*line))
        .collect();
    assert!(
        missing.is_empty(),
        "breaking changes to rustman.report/v1: {missing:#?}"
    );
}

#[test]
fn the_schema_name_is_v1() {
    assert_eq!(rustman_report::SCHEMA, "rustman.report/v1");
}
