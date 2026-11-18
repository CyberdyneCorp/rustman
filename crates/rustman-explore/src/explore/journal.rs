//! `journal.ndjson` (design D4): one JSON line per finished probe and per
//! cleanup attempt, synced to disk as each line is written. Resume replays
//! it to skip the probes that already ran; a line torn by a crash is
//! ignored, so its probe runs again.

use std::fs::{File, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::Path;

use serde::{Deserialize, Serialize};

use super::exec::Outcome;

/// The file name of the journal in an exploration's directory.
pub const JOURNAL_FILE: &str = "journal.ndjson";

pub use rustman_report::exploration::Cleanup;

/// One journal line.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "kebab-case")]
pub enum Entry {
    Outcome(Outcome),
    Cleanup(Cleanup),
}

/// An open journal, appended to line by line.
#[derive(Debug)]
pub struct Journal {
    file: File,
}

impl Journal {
    /// Open the journal at `path` for appending, creating it if needed. A
    /// torn last line is ended first, so the next entry starts a line.
    pub fn open(path: &Path) -> io::Result<Self> {
        let mut file = OpenOptions::new()
            .create(true)
            .read(true)
            .append(true)
            .open(path)?;
        if file.metadata()?.len() > 0 {
            let mut last = [0u8];
            file.seek(SeekFrom::End(-1))?;
            file.read_exact(&mut last)?;
            if last != *b"\n" {
                file.write_all(b"\n")?;
            }
        }
        Ok(Self { file })
    }

    /// Append `entry` and sync it to disk.
    pub fn append(&mut self, entry: &Entry) -> io::Result<()> {
        let mut line = serde_json::to_string(entry).map_err(io::Error::other)?;
        line.push('\n');
        self.file.write_all(line.as_bytes())?;
        self.file.sync_data()
    }
}

/// Every entry of the journal at `path`, in order; none when it does not
/// exist. Lines that do not parse (a write torn by a crash) are skipped.
pub fn read(path: &Path) -> io::Result<Vec<Entry>> {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e),
    };
    Ok(text
        .lines()
        .filter_map(|line| serde_json::from_str(line).ok())
        .collect())
}

/// The outcomes of the probes that finished among `entries`. A probe cut
/// short by a cancellation is journaled only for what it created (see
/// [`Status::interrupted`](super::exec::Status::interrupted)); it is not finished, so it runs on resume.
pub fn outcomes(entries: &[Entry]) -> impl Iterator<Item = &Outcome> {
    entries.iter().filter_map(|e| match e {
        Entry::Outcome(o) if !o.status.is_interrupted() => Some(o),
        Entry::Outcome(_) | Entry::Cleanup(_) => None,
    })
}

/// The cleanups among `entries`.
pub fn cleanups(entries: &[Entry]) -> impl Iterator<Item = &Cleanup> {
    entries.iter().filter_map(|e| match e {
        Entry::Cleanup(c) => Some(c),
        Entry::Outcome(_) => None,
    })
}

/// How many requests the journaled probes and cleanups sent.
pub fn requests_sent(entries: &[Entry]) -> u64 {
    let sent: usize = entries
        .iter()
        .map(|e| match e {
            Entry::Outcome(o) => o.sent.len(),
            Entry::Cleanup(c) => usize::from(c.status.is_some()),
        })
        .sum();
    u64::try_from(sent).unwrap_or(u64::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::explore::exec::Status;

    #[test]
    fn entries_round_trip_and_torn_lines_are_skipped() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(JOURNAL_FILE);
        let mut journal = Journal::open(&path).unwrap();
        let outcome = Entry::Outcome(Outcome::new("p0001", Status::Done));
        let cleanup = Entry::Cleanup(Cleanup {
            path: "/users/2".into(),
            url: "http://h/users/2".into(),
            status: Some(204),
            error: None,
        });
        journal.append(&outcome).unwrap();
        journal.append(&cleanup).unwrap();
        drop(journal);
        let mut file = OpenOptions::new().append(true).open(&path).unwrap();
        file.write_all(b"{\"event\":\"outco").unwrap();
        let entries = read(&path).unwrap();
        assert_eq!(entries, [outcome.clone(), cleanup]);
        assert_eq!(outcomes(&entries).count(), 1);
        let cut = [Entry::Outcome(Outcome::new("p0002", Status::interrupted()))];
        assert_eq!(
            outcomes(&cut).count(),
            0,
            "an interrupted probe did not finish"
        );
        assert!(cleanups(&entries).all(Cleanup::deleted));
        assert_eq!(requests_sent(&entries), 1);
        assert!(read(&dir.path().join("none")).unwrap().is_empty());
        let mut journal = Journal::open(&path).unwrap();
        journal.append(&outcome).unwrap();
        assert_eq!(
            read(&path).unwrap().len(),
            3,
            "a new line after the torn one"
        );
    }
}
