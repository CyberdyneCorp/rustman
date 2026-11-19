//! The examples of docs/tracing.md load as written (add-trace-propagation
//! task 2.4): every `trace:` YAML block parses as an environment, the
//! `rustman.toml` block parses without warnings, the documented error
//! messages are the real ones, and the preset table matches the code.

use std::fs;
use std::path::Path;

use rustman_core::environment;
use rustman_core::settings::Settings;
use rustman_core::trace::{LINK_PRESETS, TraceConfig, TraceRecord, TraceScope};

fn doc() -> String {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/tracing.md");
    fs::read_to_string(path).unwrap().replace("\r\n", "\n")
}

/// The bodies of the fenced blocks tagged `lang`.
fn blocks(doc: &str, lang: &str) -> Vec<String> {
    let fence = format!("```{lang}");
    let mut found = Vec::new();
    let mut lines = doc.lines();
    while let Some(line) = lines.next() {
        if line == fence {
            let body: Vec<&str> = lines.by_ref().take_while(|l| *l != "```").collect();
            found.push(body.join("\n") + "\n");
        }
    }
    found
}

#[test]
fn every_trace_yaml_example_is_a_valid_environment() {
    let doc = doc();
    let examples: Vec<String> = blocks(&doc, "yaml")
        .into_iter()
        .filter(|b| b.contains("trace:"))
        .collect();
    assert!(examples.len() >= 2);
    for text in &examples {
        let env = environment::parse(Path::new("environments/staging.yaml"), "staging", text)
            .unwrap_or_else(|e| panic!("{e}\n{text}"));
        assert!(env.trace.is_enabled(), "{text}");
    }
}

#[test]
fn the_rustman_toml_example_loads_without_warnings() {
    let doc = doc();
    let text = blocks(&doc, "toml")
        .into_iter()
        .find(|b| b.contains("[trace]"))
        .expect("a [trace] example");
    let (settings, warnings) = Settings::parse(Path::new("rustman.toml"), &text).unwrap();
    assert!(warnings.is_empty(), "{warnings:?}");
    assert!(settings.trace.is_enabled());
    assert_eq!(settings.trace.scope, None);
    assert_eq!(settings.trace.baggage.unwrap()["tenant"], "{{tenant_id}}");
}

#[test]
fn documented_messages_are_the_real_ones() {
    let doc = doc();
    let text = "variables: {}\ntrace:\n  link: \"https://x/{traceid}\"\n";
    let err = environment::parse(Path::new("environments/staging.yaml"), "staging", text)
        .unwrap_err()
        .to_string();
    assert!(doc.contains(&err), "{err}");
    let preset = TraceConfig {
        link: Some("jaeger".into()),
        ..TraceConfig::default()
    };
    let message = preset.check_merged().unwrap_err();
    assert!(doc.contains(&format!("`{message}`")), "{message}");
    let file = TraceConfig {
        link: Some("file:///tmp/x.command".into()),
        ..TraceConfig::default()
    };
    let message = file
        .validate()
        .unwrap_err()
        .replace("file:///tmp/x.command", "<link>");
    assert!(doc.contains(&format!("`{message}`")), "{message}");
    let scope: TraceConfig = serde_json::from_str(r#"{"scope": "run"}"#).unwrap();
    assert_eq!(scope.scope, Some(TraceScope::Run));
}

#[test]
fn the_preset_table_matches_the_code() {
    let doc = doc();
    for (name, template) in LINK_PRESETS {
        let row = doc
            .lines()
            .find(|l| l.starts_with(&format!("| `{name}` |")))
            .unwrap_or_else(|| panic!("no row for {name}"));
        if name != "tempo" {
            assert!(row.contains(&format!("`{template}`")), "{row}");
        }
    }
}

#[test]
fn the_recorded_trace_example_is_a_trace_record() {
    let doc = doc();
    let example = blocks(&doc, "json")
        .into_iter()
        .find(|b| b.starts_with("\"trace\""))
        .expect("a recorded trace example");
    let value: serde_json::Value = serde_json::from_str(&format!("{{{example}}}")).unwrap();
    let record: TraceRecord = serde_json::from_value(value["trace"].clone()).unwrap();
    assert_eq!(
        record.link_trace_id(),
        record.backend_trace_id.as_deref().unwrap()
    );
    assert!(
        record
            .link
            .unwrap()
            .ends_with(record.backend_trace_id.as_deref().unwrap())
    );
}

#[test]
fn the_export_example_loads_and_its_message_is_the_real_one() {
    let doc = doc();
    let text = blocks(&doc, "toml")
        .into_iter()
        .find(|b| b.contains("[trace.export]"))
        .expect("a [trace.export] example");
    let full = format!("[workspace]\nname = \"a\"\nversion = 1\n{text}");
    let (settings, warnings) = Settings::parse(Path::new("rustman.toml"), &full).unwrap();
    assert!(warnings.is_empty(), "{warnings:?}");
    assert!(settings.trace.is_exporting());
    let export = settings.trace.export.unwrap();
    assert_eq!(export.service_name.as_deref(), Some("api-tests"));
    assert!(!export.use_env);
    assert!(doc.contains(&format!("`{}`", rustman_core::trace::ENDPOINT_REQUIRED)));
}
