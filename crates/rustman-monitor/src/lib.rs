//! Scheduled monitors (openspec change `add-scheduled-monitors`): monitor
//! files in `monitors/*.rman-monitor.yaml`, their schedules, one-shot runs
//! on the request runner under a read-only safety policy, the local result
//! store in `.rustman/monitors/` with retention and compaction, alert rules
//! and notifications, the daemon, scheduler recipes and status views.
//!
//! Front-ends (the CLI, the TUI) wire these together; nothing here prints.

pub mod alerts;
pub mod cycle;
pub mod daemon;
pub mod definition;
pub mod duration;
pub mod evaluate;
pub mod gitignore;
pub mod notify;
pub mod policy;
pub mod recipe;
pub mod run;
pub mod schedule;
pub mod status;
pub mod store;

use std::path::Path;
use std::time::Duration;

use rustman_core::ConfigError;
use rustman_core::settings::MonitorSettings;

pub use definition::{Monitor, find, load_all};
pub use jiff::Timestamp;
pub use run::{MonitorError, MonitorRun, run};
pub use schedule::Schedule;
pub use store::{Outcome, RunResult, Store, StoreError};

/// How long hourly aggregates are kept when `[monitors].retention` is not
/// set.
pub const DEFAULT_RETENTION: Duration = Duration::from_secs(30 * 86_400);

/// `[monitors].retention`: at least the 7 days raw results are kept.
pub fn retention(settings: &MonitorSettings) -> Result<Duration, ConfigError> {
    let Some(text) = &settings.retention else {
        return Ok(DEFAULT_RETENTION);
    };
    let invalid = |message: String| {
        ConfigError::Usage(format!("rustman.toml [monitors] retention: {message}"))
    };
    let retention = duration::parse(text).map_err(invalid)?;
    if retention < store::RAW_RETENTION {
        return Err(invalid("must be at least 7d".to_owned()));
    }
    Ok(retention)
}

/// Store the result of a run of monitor `name` in the workspace at `root`,
/// then compact when it is overdue. Returns the warnings to show (such as
/// [`gitignore::NOT_IGNORED`]).
pub fn store_result(
    root: &Path,
    name: &str,
    result: &RunResult,
    now: Timestamp,
    retention: Duration,
) -> Result<Vec<String>, StoreError> {
    let store = Store::new(root);
    store.record(name, result)?;
    store.compact_if_due(name, now, retention)?;
    Ok(gitignore::warning(root)
        .map(str::to_owned)
        .into_iter()
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retention_defaults_to_30_days_and_is_at_least_7() {
        let mut settings = MonitorSettings::default();
        assert_eq!(retention(&settings).unwrap(), DEFAULT_RETENTION);
        settings.retention = Some("90d".into());
        assert_eq!(retention(&settings).unwrap().as_secs(), 90 * 86_400);
        settings.retention = Some("3d".into());
        let err = retention(&settings).unwrap_err().to_string();
        assert_eq!(
            err,
            "rustman.toml [monitors] retention: must be at least 7d"
        );
        settings.retention = Some("soon".into());
        assert!(retention(&settings).is_err());
    }

    #[test]
    fn storing_a_result_warns_when_the_store_is_tracked_and_compacts() {
        let tmp = tempfile::tempdir().unwrap();
        let now: Timestamp = "2026-03-01T00:00:00Z".parse().unwrap();
        let result = RunResult::skipped(now);
        let warnings = store_result(tmp.path(), "api", &result, now, DEFAULT_RETENTION).unwrap();
        assert_eq!(warnings, [gitignore::NOT_IGNORED]);
        let state = Store::new(tmp.path()).state("api").unwrap();
        assert_eq!(state.compacted_at, Some(now));
        std::fs::write(tmp.path().join(".gitignore"), ".rustman/\n").unwrap();
        let warnings = store_result(tmp.path(), "api", &result, now, DEFAULT_RETENTION).unwrap();
        assert!(warnings.is_empty());
    }
}
