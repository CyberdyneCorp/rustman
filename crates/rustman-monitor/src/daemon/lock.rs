//! One daemon per workspace: an exclusive lock on
//! `.rustman/monitors/daemon.lock`, held for the daemon's life, with its
//! process id in `daemon.pid` (a separate file, since Windows lets no other
//! process read a locked one).

use std::fs::{self, File, OpenOptions, TryLockError};
use std::path::{Path, PathBuf};

use rustman_core::ConfigError;

use crate::store::STORE_DIR;

pub const LOCK_FILE: &str = "daemon.lock";
pub const PID_FILE: &str = "daemon.pid";

/// Held while the daemon runs; released (and the pid file removed) when
/// dropped, or by the OS when the process dies.
#[derive(Debug)]
pub struct DaemonLock {
    _file: File,
    pid_file: PathBuf,
}

impl DaemonLock {
    /// Take the lock of the workspace at `root`, or fail with
    /// `monitor daemon already running (pid <n>)`.
    pub fn acquire(root: &Path) -> Result<Self, ConfigError> {
        let dir = root.join(STORE_DIR);
        let io = |path: &Path| {
            let path = path.to_path_buf();
            move |source| ConfigError::Io { path, source }
        };
        fs::create_dir_all(&dir).map_err(io(&dir))?;
        let path = dir.join(LOCK_FILE);
        let file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(&path)
            .map_err(io(&path))?;
        let pid_file = dir.join(PID_FILE);
        match file.try_lock() {
            Ok(()) => {}
            Err(TryLockError::WouldBlock) => {
                let pid = fs::read_to_string(&pid_file).unwrap_or_default();
                let pid = pid.trim();
                let pid = if pid.is_empty() { "unknown" } else { pid };
                return Err(ConfigError::Usage(format!(
                    "monitor daemon already running (pid {pid})"
                )));
            }
            Err(TryLockError::Error(e)) => return Err(io(&path)(e)),
        }
        fs::write(&pid_file, std::process::id().to_string()).map_err(io(&pid_file))?;
        Ok(Self {
            _file: file,
            pid_file,
        })
    }
}

impl Drop for DaemonLock {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.pid_file);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_second_lock_is_refused_with_the_pid_until_the_first_is_dropped() {
        let tmp = tempfile::tempdir().unwrap();
        let first = DaemonLock::acquire(tmp.path()).unwrap();
        let err = DaemonLock::acquire(tmp.path()).unwrap_err().to_string();
        assert_eq!(
            err,
            format!(
                "monitor daemon already running (pid {})",
                std::process::id()
            )
        );
        drop(first);
        assert!(!tmp.path().join(STORE_DIR).join(PID_FILE).exists());
        let _again = DaemonLock::acquire(tmp.path()).unwrap();
    }
}
