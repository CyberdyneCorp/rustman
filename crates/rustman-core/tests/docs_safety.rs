//! The examples in docs/safety.md are accurate (task 2.3).

use std::fs;
use std::path::Path;

use rustman_core::safety::{
    BudgetExhausted, EnvironmentInfo, Limits, PolicyOptions, SafetyPolicy, SafetySettings,
};
use rustman_core::settings::Settings;

fn doc() -> String {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/safety.md");
    fs::read_to_string(path).unwrap()
}

/// The `rustman.toml` example, without its `# rustman.toml` path line.
fn toml_example(doc: &str) -> String {
    let start = doc.find("```toml\n# rustman.toml\n").expect("toml example") + 23;
    let len = doc[start..].find("```").unwrap();
    doc[start..start + len].to_owned()
}

fn parse(text: &str) -> Result<Settings, String> {
    Settings::parse(Path::new("rustman.toml"), text)
        .map(|(settings, warnings)| {
            assert!(warnings.is_empty(), "{warnings:?}");
            settings
        })
        .map_err(|e| e.to_string())
}

#[test]
fn the_example_parses_with_the_documented_values() {
    let doc = doc();
    let settings = parse(&toml_example(&doc)).unwrap();
    let safety = settings.safety;
    assert_eq!(safety.allowed_hosts.len(), 2);
    assert_eq!(safety.production_hosts.len(), 2);
    let defaults = SafetySettings::default();
    assert_eq!(safety.rate, defaults.rate);
    assert_eq!(safety.concurrency, defaults.concurrency);
}

#[test]
fn documented_config_errors_are_the_real_ones() {
    let doc = doc();
    let example = toml_example(&doc);
    let bad_origin = example.replace(
        r#"["https://auth.example.com", "http://127.0.0.1:9000"]"#,
        r#"["auth.example.com"]"#,
    );
    let err = parse(&bad_origin).unwrap_err();
    assert!(doc.contains(&format!("{err}\n")), "{err}");
    let zero_rate = example.replace("rate = 10 ", "rate = 0 ");
    let err = parse(&zero_rate).unwrap_err();
    assert!(doc.contains(&format!("``{err}``")), "{err}");
}

#[test]
fn documented_refusals_are_the_real_ones() {
    let doc = doc();
    let options = PolicyOptions {
        target: "https://api.example.com/",
        environment: EnvironmentInfo {
            name: Some("production"),
            marked_production: false,
        },
        ..PolicyOptions::default()
    };
    let settings = SafetySettings::default();
    let refusals = [
        options,
        PolicyOptions {
            production: Some("api.example.com"),
            permission: rustman_core::safety::Permission::Destructive,
            ..options
        },
        PolicyOptions {
            production: Some("example.com"),
            ..options
        },
    ];
    for options in refusals {
        let err = SafetyPolicy::new(&settings, options).unwrap_err();
        assert!(doc.contains(&err.to_string()), "{err}");
    }
}

#[test]
fn documented_limits_and_budget_reason_are_the_real_ones() {
    let doc = doc();
    let limits = Limits::default();
    let rows = [
        format!("| Rate | {} requests/second |", limits.rate),
        format!("| Concurrency | {} |", limits.concurrency),
        format!("| Probe budget | {} probes |", limits.max_probes),
        format!(
            "| Time budget | {} minutes |",
            limits.max_duration.as_secs() / 60
        ),
        format!("`{}`", BudgetExhausted::Probes),
    ];
    for row in rows {
        assert!(doc.contains(&row), "{row}");
    }
}
