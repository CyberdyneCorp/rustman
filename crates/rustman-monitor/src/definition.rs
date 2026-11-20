//! Monitor files (monitors "Monitor definition files"):
//! `monitors/<name>.rman-monitor.yaml`, one monitor each.
//!
//! ```yaml
//! version: 1
//! name: api-health
//! targets: [health/]
//! environment: prod
//! schedule: {every: 5m}
//! timeout: 60s
//! alerts:
//!   rules:
//!     - consecutive_failures: 3
//!   notify:
//!     - webhook: {url: "{{slack_hook}}", template: slack}
//! ```
//!
//! An invalid file fails to load with `<path>:<line>: <reason>`; the CLI
//! exits 2 ([`ConfigError::Invalid`]).

use std::path::{Path, PathBuf};
use std::time::Duration;

use rustman_core::environment::is_valid_name;
use rustman_core::format::{Located, from_yaml};
use rustman_core::{ConfigError, LoadError, Position};
use serde::Deserialize;

use crate::alerts::{Alerts, AlertsFile};
use crate::schedule::Schedule;

/// The folder of monitor files, under the workspace root.
pub const MONITORS_DIR: &str = "monitors";
/// The suffix of a monitor file.
pub const SUFFIX: &str = ".rman-monitor.yaml";
/// The only monitor file version this build reads.
pub const VERSION: u32 = 1;
/// The whole-run deadline when `timeout` is not set.
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(60);

/// One loaded monitor.
#[derive(Clone, Debug)]
pub struct Monitor {
    pub name: String,
    /// The monitor file, relative to the workspace root.
    pub file: PathBuf,
    /// Request files or collection folders, relative to the workspace
    /// root, run in workspace order.
    pub targets: Vec<String>,
    pub environment: String,
    pub schedule: Schedule,
    /// The deadline of a whole run; per-request timeouts still apply.
    pub timeout: Duration,
    /// `allow_mutations: true`: POST, PUT and PATCH may be sent. DELETE is
    /// refused whatever this says.
    pub allow_mutations: bool,
    pub alerts: Alerts,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct MonitorFile {
    version: Located<u32>,
    name: Located<String>,
    targets: Located<Vec<String>>,
    environment: Located<String>,
    schedule: Located<ScheduleFile>,
    #[serde(default)]
    timeout: Option<Located<String>>,
    #[serde(default)]
    allow_mutations: bool,
    #[serde(default)]
    alerts: AlertsFile,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ScheduleFile {
    #[serde(default)]
    cron: Option<Located<String>>,
    #[serde(default)]
    every: Option<Located<String>>,
    #[serde(default)]
    tz: Option<Located<String>>,
}

/// Every monitor of the workspace at `root`, sorted by name. No
/// `monitors/` folder means no monitors. Every invalid file is reported.
pub fn load_all(root: &Path) -> Result<Vec<Monitor>, ConfigError> {
    let dir = root.join(MONITORS_DIR);
    let entries = match std::fs::read_dir(&dir) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => {
            return Err(ConfigError::Io {
                path: dir,
                source: e,
            });
        }
    };
    let mut names: Vec<String> = entries
        .filter_map(Result::ok)
        .filter(|e| e.file_type().is_ok_and(|t| t.is_file()))
        .filter_map(|e| e.file_name().into_string().ok())
        .filter(|name| name.ends_with(SUFFIX))
        .collect();
    names.sort();
    let mut monitors = Vec::new();
    let mut errors = Vec::new();
    for name in names {
        match load_file(root, &Path::new(MONITORS_DIR).join(&name)) {
            Ok(monitor) => monitors.push(monitor),
            Err(e) => errors.push(e),
        }
    }
    if errors.is_empty() {
        Ok(monitors)
    } else {
        Err(ConfigError::Invalid(errors))
    }
}

/// The monitor called `name` among `monitors`.
pub fn find<'a>(monitors: &'a [Monitor], name: &str) -> Result<&'a Monitor, ConfigError> {
    monitors.iter().find(|m| m.name == name).ok_or_else(|| {
        let available: Vec<&str> = monitors.iter().map(|m| m.name.as_str()).collect();
        let available = if available.is_empty() {
            "none".to_owned()
        } else {
            available.join(", ")
        };
        ConfigError::Usage(format!(
            "monitor '{name}' not found (available: {available})"
        ))
    })
}

/// Load the monitor file `file` (relative to `root`).
pub fn load_file(root: &Path, file: &Path) -> Result<Monitor, LoadError> {
    let text = std::fs::read_to_string(root.join(file))
        .map_err(|e| LoadError::new(file, None, e.to_string()))?;
    parse(file, &text)
}

/// Parse and validate the text of the monitor file `file`.
pub fn parse(file: &Path, text: &str) -> Result<Monitor, LoadError> {
    let raw: MonitorFile = from_yaml(file, text)?;
    let at = |position: Option<Position>, message: String| LoadError::new(file, position, message);
    if *raw.version != VERSION {
        let message = format!(
            "unsupported monitor version {} (this rustman reads version {VERSION})",
            *raw.version
        );
        return Err(at(raw.version.position(), message));
    }
    check_name(file, &raw.name)?;
    if raw.targets.is_empty() {
        let message = "targets must list at least one request file or folder".to_owned();
        return Err(at(raw.targets.position(), message));
    }
    if !is_valid_name(&raw.environment) {
        let message = format!("invalid environment name '{}'", *raw.environment);
        return Err(at(raw.environment.position(), message));
    }
    let schedule = schedule(file, &raw.schedule)?;
    let timeout = match &raw.timeout {
        Some(t) => crate::duration::parse(t).map_err(|m| at(t.position(), m))?,
        None => DEFAULT_TIMEOUT,
    };
    let alerts = raw.alerts.validate(file)?;
    Ok(Monitor {
        name: raw.name.into_inner(),
        file: file.to_path_buf(),
        targets: raw.targets.into_inner(),
        environment: raw.environment.into_inner(),
        schedule,
        timeout,
        allow_mutations: raw.allow_mutations,
        alerts,
    })
}

/// A valid name, the same as the file's: the result store is named after it.
fn check_name(file: &Path, name: &Located<String>) -> Result<(), LoadError> {
    let stem = file
        .file_name()
        .and_then(|n| n.to_str())
        .and_then(|n| n.strip_suffix(SUFFIX))
        .unwrap_or_default();
    let message = if !is_valid_name(name) {
        format!(
            "invalid monitor name '{}' (use letters, digits, - and _)",
            **name
        )
    } else if is_reserved_on_windows(name) {
        format!(
            "invalid monitor name '{}' (a reserved device name on Windows; its store folder could not be created)",
            **name
        )
    } else if name.as_str() != stem {
        format!(
            "name '{}' does not match the file name (expected '{stem}')",
            **name
        )
    } else {
        return Ok(());
    };
    Err(LoadError::new(file, name.position(), message))
}

/// `con`, `nul`, `com1`, ... in any case: Windows cannot create a folder
/// with that name, on every platform so a workspace stays portable.
fn is_reserved_on_windows(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    if matches!(lower.as_str(), "con" | "prn" | "aux" | "nul") {
        return true;
    }
    let numbered = lower
        .strip_prefix("com")
        .or_else(|| lower.strip_prefix("lpt"));
    numbered.is_some_and(|n| n.len() == 1 && matches!(n.as_bytes()[0], b'1'..=b'9'))
}

fn schedule(file: &Path, raw: &Located<ScheduleFile>) -> Result<Schedule, LoadError> {
    let tz = raw.tz.as_ref();
    let result = match (&raw.cron, &raw.every) {
        (Some(cron), None) => {
            Schedule::cron(cron, tz.map(|t| t.as_str())).map_err(|m| (cron.position(), m))
        }
        (None, Some(every)) if tz.is_some() => Err((
            tz.and_then(Located::position),
            "tz applies to cron schedules only".to_owned(),
        )),
        (None, Some(every)) => Schedule::every(every).map_err(|m| (every.position(), m)),
        _ => Err((
            raw.position(),
            "schedule must set exactly one of cron or every".to_owned(),
        )),
    };
    result.map_err(|(position, message)| LoadError::new(file, position, message))
}

/// Whether `value` is exactly one `{{name}}` reference: how notifier
/// credentials are written, so their values stay in environments and the
/// keychain, never in monitor files.
pub(crate) fn is_reference(value: &str) -> bool {
    let value = value.trim();
    value
        .strip_prefix("{{")
        .and_then(|rest| rest.strip_suffix("}}"))
        .is_some_and(|name| !name.trim().is_empty() && !name.contains(['{', '}']))
}

#[cfg(test)]
mod tests {
    use super::*;

    const FILE: &str = "monitors/api-health.rman-monitor.yaml";

    fn load(text: &str) -> Result<Monitor, String> {
        parse(Path::new(FILE), text).map_err(|e| e.to_string().replace('\\', "/"))
    }

    const VALID: &str = "version: 1
name: api-health
targets: [health/]
environment: prod
schedule: {every: 5m}
";

    #[test]
    fn a_valid_monitor_with_defaults() {
        let monitor = load(VALID).unwrap();
        assert_eq!(monitor.name, "api-health");
        assert_eq!(monitor.targets, ["health/"]);
        assert_eq!(monitor.environment, "prod");
        assert_eq!(monitor.schedule.to_string(), "every 5m");
        assert_eq!(monitor.timeout, Duration::from_secs(60));
        assert!(!monitor.allow_mutations);
        assert_eq!(monitor.alerts, Alerts::default());
    }

    #[test]
    fn both_schedule_kinds_fail_with_the_line() {
        let text = "version: 1
name: api-health
targets: [health/]
environment: prod
schedule: {every: 5m, cron: \"* * * * *\"}
";
        assert_eq!(
            load(text).unwrap_err(),
            format!("{FILE}:5: schedule must set exactly one of cron or every")
        );
        let neither = VALID.replace("{every: 5m}", "{}");
        assert_eq!(
            load(&neither).unwrap_err(),
            format!("{FILE}:5: schedule must set exactly one of cron or every")
        );
    }

    #[test]
    fn an_interval_below_30s_is_refused() {
        let text = VALID.replace("every: 5m", "every: 5s");
        assert_eq!(
            load(&text).unwrap_err(),
            format!("{FILE}:5: every must be at least 30s")
        );
    }

    #[test]
    fn cron_with_a_timezone_and_a_timeout() {
        let text = VALID.replace(
            "schedule: {every: 5m}",
            "schedule:\n  cron: \"*/15 * * * *\"\n  tz: UTC\ntimeout: 2m\nallow_mutations: true",
        );
        let monitor = load(&text).unwrap();
        assert_eq!(monitor.schedule.to_string(), "cron */15 * * * * (UTC)");
        assert_eq!(monitor.timeout, Duration::from_secs(120));
        assert!(monitor.allow_mutations);
        let bad_cron = text.replace("*/15 * * * *", "nope");
        assert!(
            load(&bad_cron)
                .unwrap_err()
                .starts_with(&format!("{FILE}:6: invalid cron")),
            "{}",
            load(&bad_cron).unwrap_err()
        );
        let tz_with_every = VALID.replace("{every: 5m}", "{every: 5m, tz: UTC}");
        assert_eq!(
            load(&tz_with_every).unwrap_err(),
            format!("{FILE}:5: tz applies to cron schedules only")
        );
    }

    #[test]
    fn invalid_fields_name_their_line() {
        let cases = [
            (
                "version: 1",
                "version: 2",
                "2: unsupported monitor version 2",
            ),
            (
                "name: api-health",
                "name: other",
                "3: name 'other' does not match",
            ),
            (
                "name: api-health",
                "name: \"a b\"",
                "3: invalid monitor name 'a b'",
            ),
            ("targets: [health/]", "targets: []", "4: targets must list"),
            (
                "environment: prod",
                "environment: \"p d\"",
                "5: invalid environment",
            ),
            (
                "schedule: {every: 5m}",
                "schedule: {every: 5m}\ntimeout: 5",
                "7: invalid duration",
            ),
        ];
        for (from, to, want) in cases {
            let text = format!("\n{}", VALID.replace(from, to));
            let err = load(&text).unwrap_err();
            assert!(err.starts_with(&format!("{FILE}:{want}")), "{to}: {err}");
        }
        let err = load(&format!("{VALID}extra: 1\n")).unwrap_err();
        assert!(err.contains("unknown field"), "{err}");
        let err = load("version: 1\nname: api-health\n").unwrap_err();
        assert!(err.contains("missing field"), "{err}");
    }

    /// `con`, `NUL`, `com1`, `lpt9`: the store folder `.rustman/monitors/<name>/`
    /// cannot be created on Windows, so the name is refused everywhere.
    #[test]
    fn windows_device_names_are_refused() {
        for name in ["con", "NUL", "Aux", "prn", "com1", "LPT9"] {
            let err = check_name(
                Path::new(&format!("monitors/{name}{SUFFIX}")),
                &Located::new(name.to_owned()),
            )
            .unwrap_err()
            .to_string();
            assert!(err.contains("reserved device name on Windows"), "{err}");
        }
        for name in ["console", "com", "com10", "lpt0", "nul-check", "auxiliary"] {
            assert!(!is_reserved_on_windows(name), "{name}");
        }
    }

    #[test]
    fn references_are_single_placeholders() {
        assert!(is_reference("{{slack_hook}}"));
        assert!(is_reference(" {{ slack_hook }} "));
        assert!(!is_reference("https://hooks.slack.com/x"));
        assert!(!is_reference("https://{{host}}/x"));
        assert!(!is_reference("{{a}}{{b}}"));
        assert!(!is_reference("{{}}"));
    }

    #[test]
    fn workspace_monitors_load_sorted_and_report_every_bad_file() {
        let tmp = tempfile::tempdir().unwrap();
        assert!(load_all(tmp.path()).unwrap().is_empty());
        let dir = tmp.path().join(MONITORS_DIR);
        std::fs::create_dir_all(dir.join("templates")).unwrap();
        let write = |name: &str, text: &str| {
            std::fs::write(dir.join(format!("{name}{SUFFIX}")), text).unwrap();
        };
        write("b-mon", &VALID.replace("api-health", "b-mon"));
        write("a-mon", &VALID.replace("api-health", "a-mon"));
        std::fs::write(dir.join("notes.yaml"), "ignored").unwrap();
        let monitors = load_all(tmp.path()).unwrap();
        let names: Vec<&str> = monitors.iter().map(|m| m.name.as_str()).collect();
        assert_eq!(names, ["a-mon", "b-mon"]);
        assert_eq!(find(&monitors, "b-mon").unwrap().name, "b-mon");
        let err = find(&monitors, "c").unwrap_err().to_string();
        assert_eq!(err, "monitor 'c' not found (available: a-mon, b-mon)");
        write("x", "version: 1\n");
        write("y", "version: 1\n");
        let ConfigError::Invalid(errors) = load_all(tmp.path()).unwrap_err() else {
            panic!("not an invalid-file error");
        };
        assert_eq!(errors.len(), 2);
    }
}
