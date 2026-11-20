//! The examples of docs/monitors.md: every monitor file, schedule and
//! alerts block loads, the stored result line parses, the TOML example is
//! a valid `rustman.toml` with a valid retention, the webhook template
//! renders JSON, and the GitHub Actions workflow is the recipe's output.

use std::path::Path;

use rustman_monitor::definition::parse;
use rustman_monitor::store::RunResult;

const DOC: &str = include_str!("../../../docs/monitors.md");

const HEAD: &str = "version: 1\nname: api-health\ntargets: [health/]\nenvironment: prod\n";

/// The fenced blocks of the doc with their language.
fn blocks() -> Vec<(String, String)> {
    let mut blocks = Vec::new();
    let mut open: Option<(String, String)> = None;
    for line in DOC.lines() {
        match (&mut open, line.strip_prefix("```")) {
            (None, Some(lang)) => open = Some((lang.trim().to_owned(), String::new())),
            (Some(_), Some(_)) => blocks.extend(open.take()),
            (Some((_, text)), None) => {
                text.push_str(line);
                text.push('\n');
            }
            (None, None) => {}
        }
    }
    blocks
}

/// A whole monitor file for a doc snippet.
fn monitor_file(snippet: &str) -> String {
    if snippet.contains("version: 1") {
        snippet.to_owned()
    } else if snippet.starts_with("schedule:") {
        format!("{HEAD}{snippet}")
    } else {
        format!("{HEAD}schedule: {{every: 5m}}\n{snippet}")
    }
}

#[test]
fn every_yaml_example_loads() {
    let file = Path::new("monitors/api-health.rman-monitor.yaml");
    let mut count = 0;
    for (lang, text) in blocks().iter().filter(|(lang, _)| lang == "yaml") {
        let monitor =
            parse(file, &monitor_file(text)).unwrap_or_else(|e| panic!("{lang}: {e}\n{text}"));
        assert_eq!(monitor.name, "api-health");
        count += 1;
    }
    assert!(count >= 4, "{count} yaml examples");
}

#[test]
fn the_stored_result_example_parses() {
    let (_, json) = blocks()
        .into_iter()
        .find(|(lang, _)| lang == "json")
        .unwrap();
    let result: RunResult = serde_json::from_str(json.trim()).unwrap();
    assert_eq!(result.outcome, rustman_monitor::Outcome::Fail);
    assert_eq!(result.failures.len(), 1);
}

#[test]
fn the_toml_example_is_a_valid_retention() {
    let (_, toml) = blocks()
        .into_iter()
        .find(|(lang, _)| lang == "toml")
        .unwrap();
    let text = format!("[workspace]\nname = \"t\"\nversion = 1\n{toml}");
    let (settings, warnings) =
        rustman_core::settings::Settings::parse(Path::new("rustman.toml"), &text).unwrap();
    assert!(warnings.is_empty(), "{warnings:?}");
    let retention = rustman_monitor::retention(&settings.monitors).unwrap();
    assert_eq!(retention.as_secs(), 90 * 86_400);
}

/// The GitHub Actions example is what `schedule-recipe` prints for it.
#[test]
fn the_github_actions_example_is_the_recipe_output() {
    use rustman_monitor::recipe::{Target, recipe};
    let (_, doc) = blocks()
        .into_iter()
        .find(|(lang, _)| lang == "yml")
        .unwrap();
    let file = format!("{HEAD}schedule: {{cron: \"*/15 * * * *\", tz: UTC}}\n");
    let monitor = parse(Path::new("monitors/api-health.rman-monitor.yaml"), &file).unwrap();
    let printed = recipe(&monitor, Path::new("/ws"), "rustman", Target::GithubActions).unwrap();
    assert_eq!(doc, printed);
}

/// The template override example renders JSON.
#[test]
fn the_template_example_renders_json() {
    use rustman_monitor::alerts::WebhookTemplate;
    use rustman_monitor::notify::{Alert, AlertKind, TEMPLATES_DIR, render};
    let (_, template) = blocks()
        .into_iter()
        .find(|(lang, _)| lang == "jinja")
        .unwrap();
    let root = tempfile::tempdir().unwrap();
    let dir = root.path().join(TEMPLATES_DIR);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("generic.json.j2"), template).unwrap();
    let alert = Alert {
        monitor: "api-health".into(),
        environment: "prod".into(),
        kind: AlertKind::Firing,
        rule: Some("consecutive_failures: 3".into()),
        at: "2026-03-01T10:00:00Z".parse().unwrap(),
        failure: Some("health/status: status: expected 200, got 503".into()),
        duration: None,
    };
    let payload = render(
        root.path(),
        WebhookTemplate::Generic,
        &alert,
        &rustman_core::Redactor::new(),
    )
    .unwrap();
    assert_eq!(
        payload["summary"],
        "[FIRING] api-health: consecutive_failures: 3"
    );
    assert_eq!(payload["env"], "prod");
}
