//! The local result store (monitors "Result store", design D3), under
//! `.rustman/monitors/<name>/` (git-ignored):
//!
//! - `raw/YYYY-MM-DD.jsonl`: one [`RunResult`] per line, one file per UTC
//!   day;
//! - `hourly.jsonl`: [`HourlyAggregate`]s of compacted days;
//! - `state.json`: the last run, the last compaction and the alert state.
//!
//! An append takes an exclusive lock on the day file and writes its line
//! with a single write, so concurrent writers (the daemon and a one-shot
//! run under cron) never interleave. `state.json` and `hourly.jsonl` are
//! replaced atomically (a temporary file renamed over them) under a lock
//! file. Locks are the standard library's advisory file locks (`flock` and
//! `LockFileEx`): `fs4` is not needed since Rust 1.89.
//!
//! Every function takes `now` instead of reading the clock, so retention
//! and compaction are tested with synthetic timestamps.

mod compact;
mod result;

use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};

use jiff::Timestamp;
use jiff::civil::Date;
use jiff::tz::TimeZone;
use serde::{Deserialize, Serialize};

use crate::evaluate::AlertState;

pub(crate) use compact::hour_of;
pub use compact::{Compaction, HourlyAggregate, RAW_RETENTION, compaction_due, percentile};
pub use result::{Counts, Outcome, RequestResult, RunResult};

/// Where monitor state lives, under the workspace root.
pub const STORE_DIR: &str = ".rustman/monitors";

const RAW_DIR: &str = "raw";
const HOURLY_FILE: &str = "hourly.jsonl";
const STATE_FILE: &str = "state.json";
/// Held while `state.json` is read, changed and replaced.
const STATE_LOCK: &str = "state.lock";
/// Held for a whole compaction.
const COMPACT_LOCK: &str = "compact.lock";

/// A store problem, with the file it concerns.
#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("{}: {source}", path.display())]
    Io {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("{}: invalid state: {message}", path.display())]
    InvalidState { path: PathBuf, message: String },
}

fn io_error(path: &Path) -> impl FnOnce(io::Error) -> StoreError + '_ {
    move |source| StoreError::Io {
        path: path.to_path_buf(),
        source,
    }
}

/// `state.json`: what a monitor remembers between runs, shared by the
/// daemon and one-shot runs. Fields this build does not know are kept.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct State {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_run: Option<LastRun>,
    /// When compaction last ran.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub compacted_at: Option<Timestamp>,
    /// The firing alert rules.
    #[serde(default, skip_serializing_if = "AlertState::is_empty")]
    pub alerts: AlertState,
    #[serde(flatten)]
    pub other: serde_json::Map<String, serde_json::Value>,
}

/// The outcome and time of the latest run.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct LastRun {
    pub at: Timestamp,
    pub outcome: Outcome,
    pub duration_ms: f64,
}

impl From<&RunResult> for LastRun {
    fn from(result: &RunResult) -> Self {
        Self {
            at: result.at,
            outcome: result.outcome,
            duration_ms: result.duration_ms,
        }
    }
}

/// The store of one workspace.
#[derive(Clone, Debug)]
pub struct Store {
    dir: PathBuf,
}

impl Store {
    /// The store of the workspace at `root`.
    pub fn new(root: &Path) -> Self {
        Self {
            dir: root.join(STORE_DIR),
        }
    }

    /// The folder of monitor `name`.
    pub fn monitor_dir(&self, name: &str) -> PathBuf {
        self.dir.join(name)
    }

    /// The raw file of the UTC day of `at`.
    pub fn day_file(&self, name: &str, at: Timestamp) -> PathBuf {
        self.monitor_dir(name)
            .join(RAW_DIR)
            .join(format!("{}.jsonl", utc_date(at)))
    }

    /// Append `result` to its day's raw file.
    pub fn append(&self, name: &str, result: &RunResult) -> Result<(), StoreError> {
        let path = self.day_file(name, result.at);
        append_line(&path, result)
    }

    /// The raw results at or after `since`, oldest first. A line that does
    /// not parse (written by a newer build, or cut by a full disk) is
    /// skipped.
    pub fn raw_since(&self, name: &str, since: Timestamp) -> Result<Vec<RunResult>, StoreError> {
        let first = utc_date(since);
        let mut results = Vec::new();
        for (date, path) in self.day_files(name)? {
            if date >= first {
                results.extend(read_lines::<RunResult>(&path)?);
            }
        }
        results.retain(|r| r.at >= since);
        results.sort_by_key(|r| r.at);
        Ok(results)
    }

    /// Every hourly aggregate, oldest first.
    pub fn hourly(&self, name: &str) -> Result<Vec<HourlyAggregate>, StoreError> {
        let mut hours = read_lines::<HourlyAggregate>(&self.hourly_file(name))?;
        hours.sort_by_key(|h| h.hour);
        Ok(hours)
    }

    /// The monitor's state; the default before its first run.
    pub fn state(&self, name: &str) -> Result<State, StoreError> {
        read_state(&self.monitor_dir(name).join(STATE_FILE))
    }

    /// Change the state with `change` under the state lock, and replace
    /// `state.json` atomically.
    pub fn update_state<T>(
        &self,
        name: &str,
        change: impl FnOnce(&mut State) -> T,
    ) -> Result<T, StoreError> {
        let dir = self.monitor_dir(name);
        let _lock = lock_file(&dir.join(STATE_LOCK))?;
        let path = dir.join(STATE_FILE);
        let mut state = read_state(&path)?;
        let out = change(&mut state);
        let text = serde_json::to_string_pretty(&state).expect("state serializes");
        replace_file(&path, text.as_bytes())?;
        Ok(out)
    }

    /// Store `result`: append it and record it as the last run.
    pub fn record(&self, name: &str, result: &RunResult) -> Result<(), StoreError> {
        self.append(name, result)?;
        self.update_state(name, |state| state.last_run = Some(LastRun::from(result)))
    }

    fn hourly_file(&self, name: &str) -> PathBuf {
        self.monitor_dir(name).join(HOURLY_FILE)
    }

    /// The raw day files of `name` with their dates, oldest first.
    fn day_files(&self, name: &str) -> Result<Vec<(Date, PathBuf)>, StoreError> {
        let dir = self.monitor_dir(name).join(RAW_DIR);
        let entries = match fs::read_dir(&dir) {
            Ok(entries) => entries,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(io_error(&dir)(e)),
        };
        let mut files: Vec<(Date, PathBuf)> = entries
            .filter_map(Result::ok)
            .filter_map(|e| {
                let name = e.file_name().into_string().ok()?;
                let date = name.strip_suffix(".jsonl")?.parse::<Date>().ok()?;
                Some((date, e.path()))
            })
            .collect();
        files.sort();
        Ok(files)
    }
}

/// The UTC calendar day of `at`.
pub(crate) fn utc_date(at: Timestamp) -> Date {
    at.to_zoned(TimeZone::UTC).date()
}

/// Append `value` as one line, with one write under an exclusive lock.
fn append_line<T: Serialize>(path: &Path, value: &T) -> Result<(), StoreError> {
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir).map_err(io_error(dir))?;
    }
    let mut line = serde_json::to_string(value).expect("results serialize");
    line.push('\n');
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .map_err(io_error(path))?;
    file.lock().map_err(io_error(path))?;
    file.write_all(line.as_bytes()).map_err(io_error(path))?;
    // Closing the file releases the lock.
    Ok(())
}

/// The lines of `path` that parse as `T`, under a shared lock; none when
/// the file does not exist.
fn read_lines<T: for<'de> Deserialize<'de>>(path: &Path) -> Result<Vec<T>, StoreError> {
    let mut file = match File::open(path) {
        Ok(file) => file,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(io_error(path)(e)),
    };
    file.lock_shared().map_err(io_error(path))?;
    let mut text = String::new();
    file.read_to_string(&mut text).map_err(io_error(path))?;
    Ok(text
        .lines()
        .filter_map(|line| serde_json::from_str(line).ok())
        .collect())
}

fn read_state(path: &Path) -> Result<State, StoreError> {
    match fs::read_to_string(path) {
        Ok(text) => serde_json::from_str(&text).map_err(|e| StoreError::InvalidState {
            path: path.to_path_buf(),
            message: e.to_string(),
        }),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(State::default()),
        Err(e) => Err(io_error(path)(e)),
    }
}

/// Open (creating its folder) and exclusively lock `path`; the lock is
/// released when the returned file is dropped.
fn lock_file(path: &Path) -> Result<File, StoreError> {
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir).map_err(io_error(dir))?;
    }
    let file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(path)
        .map_err(io_error(path))?;
    file.lock().map_err(io_error(path))?;
    Ok(file)
}

/// Replace `path` with `contents` atomically: readers see the old file or
/// the new one, never a partial write.
fn replace_file(path: &Path, contents: &[u8]) -> Result<(), StoreError> {
    let dir = path.parent().expect("store files live in a folder");
    fs::create_dir_all(dir).map_err(io_error(dir))?;
    let mut tmp = tempfile::NamedTempFile::new_in(dir).map_err(io_error(dir))?;
    tmp.write_all(contents).map_err(io_error(path))?;
    tmp.as_file().sync_all().map_err(io_error(path))?;
    tmp.persist(path).map_err(|e| io_error(path)(e.error))?;
    Ok(())
}

#[cfg(test)]
mod tests;
