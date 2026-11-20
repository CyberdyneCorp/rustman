use std::path::Path;

use super::*;
use crate::definition::parse;

fn monitor(schedule: &str) -> Monitor {
    let text = format!(
        "version: 1\nname: api-health\ntargets: [health/]\nenvironment: prod\nschedule: {schedule}\n"
    );
    parse(Path::new("monitors/api-health.rman-monitor.yaml"), &text).unwrap()
}

fn all(schedule: &str) -> String {
    let monitor = monitor(schedule);
    let root = Path::new("/srv/ws");
    [
        Target::Cron,
        Target::Systemd,
        Target::Launchd,
        Target::GithubActions,
    ]
    .into_iter()
    .map(|target| {
        let text = recipe(&monitor, root, "/usr/local/bin/rustman", target).unwrap();
        format!("===== {target:?}\n{text}")
    })
    .collect()
}

#[test]
fn recipes_for_a_cron_schedule() {
    insta::assert_snapshot!(all("{cron: \"0,30 9 * * 1-5\", tz: Europe/Lisbon}"));
}

#[test]
fn recipes_for_an_interval() {
    insta::assert_snapshot!(all("{every: 5m}"));
}

/// monitors "GitHub Actions recipe": the workflow parses, with
/// `on.schedule.cron` and a step running the monitor.
#[test]
fn the_github_actions_recipe_is_a_valid_workflow() {
    let monitor = monitor("{cron: \"*/15 * * * *\"}");
    let text = recipe(&monitor, Path::new("/ws"), "rustman", Target::GithubActions).unwrap();
    let workflow: serde_json::Value =
        rustman_core::format::from_yaml(Path::new("monitor.yml"), &text).unwrap();
    assert_eq!(workflow["on"]["schedule"][0]["cron"], "*/15 * * * *");
    let steps = workflow["jobs"]["monitor"]["steps"].as_array().unwrap();
    assert!(
        steps
            .iter()
            .any(|s| s["run"] == "rustman monitor run api-health"),
        "{steps:?}"
    );
    assert!(workflow["jobs"]["monitor"]["runs-on"].is_string());
    // Macros are expanded: GitHub does not read them.
    let daily = recipe(
        &monitor_daily(),
        Path::new("/ws"),
        "rustman",
        Target::GithubActions,
    );
    assert!(daily.unwrap().contains("- cron: \"0 0 * * *\""));
}

fn monitor_daily() -> Monitor {
    monitor("{cron: \"@daily\"}")
}

#[test]
fn intervals_map_to_cron_when_they_can() {
    let cron = |s: &str| cron_expression(&monitor(s).schedule);
    assert_eq!(cron("{every: 1m}").unwrap(), "* * * * *");
    assert_eq!(cron("{every: 15m}").unwrap(), "*/15 * * * *");
    assert_eq!(cron("{every: 6h}").unwrap(), "0 */6 * * *");
    assert_eq!(cron("{every: 24h}").unwrap(), "0 0 * * *");
    for every in ["30s", "7m", "5h", "90s"] {
        let err = cron(&format!("{{every: {every}}}")).unwrap_err();
        assert!(err.contains("has no cron equivalent"), "{every}: {err}");
    }
}

#[test]
fn calendars_refuse_what_they_cannot_express() {
    let both = monitor("{cron: \"0 9 1 * 1\"}");
    for target in [Target::Systemd, Target::Launchd] {
        let err = recipe(&both, Path::new("/ws"), "rustman", target).unwrap_err();
        assert!(err.contains("no calendar equivalent"), "{err}");
    }
    let weekly = monitor("{cron: \"@weekly\"}");
    let text = recipe(&weekly, Path::new("/ws"), "rustman", Target::Systemd).unwrap();
    assert!(text.contains("OnCalendar=Sun *-*-* 0:0:00\n"), "{text}");
}

#[test]
fn paths_are_quoted_for_the_shell_and_xml() {
    let monitor = monitor("{every: 5m}");
    let root = Path::new("/home/o'neil/my ws & co");
    let text = recipe(&monitor, root, "rustman", Target::Cron).unwrap();
    assert!(
        text.contains("cd '/home/o'\\''neil/my ws & co' && rustman monitor run api-health"),
        "{text}"
    );
    let text = recipe(&monitor, root, "rustman", Target::Launchd).unwrap();
    assert!(
        text.contains("<string>/home/o'neil/my ws &amp; co</string>"),
        "{text}"
    );
}

/// Windows `canonicalize` returns verbatim paths; recipes show the plain
/// form cron, systemd and launchd users would type.
#[test]
fn verbatim_windows_paths_lose_their_prefix() {
    assert_eq!(without_verbatim_prefix(r"\\?\C:\work\ws"), r"C:\work\ws");
    assert_eq!(
        without_verbatim_prefix(r"\\?\UNC\server\share\ws"),
        r"\\server\share\ws"
    );
    assert_eq!(without_verbatim_prefix("/srv/ws"), "/srv/ws");
    assert_eq!(
        without_verbatim_prefix(r"\\?\Volume{x}\ws"),
        r"\\?\Volume{x}\ws"
    );
    let text = recipe(
        &monitor("{every: 5m}"),
        Path::new(r"\\?\C:\work\ws"),
        "rustman",
        Target::Cron,
    )
    .unwrap();
    assert!(text.contains(r"cd 'C:\work\ws'"), "{text}");
    assert!(!text.contains(r"\\?\"), "{text}");
}
