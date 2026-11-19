//! Stopping a stdio server with every process it started. Servers often
//! run behind a launcher (`npx`, `uvx`, `sh -c ...`), and killing only the
//! launcher would leave the real server running. On Unix the server is
//! started in a process group of its own and the whole group is killed; on
//! Windows the process tree is killed with `taskkill /T` while its root is
//! still running. Program lookup on Windows also finds `.cmd` and `.bat`
//! launchers (`npx` is `npx.cmd`), which the standard library does not.
//!
//! Every running tree is also listed in a process-wide registry, so an exit
//! that skips destructors (`std::process::exit` on a second Ctrl+C) can
//! still stop them first with [`kill_all_servers`].

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, PoisonError};
use std::time::Duration;

use tokio::process::Command;

/// How long `taskkill` gets to stop a tree on Windows.
#[cfg_attr(not(windows), allow(dead_code))]
const TASKKILL_WAIT: Duration = Duration::from_secs(5);

/// Start the server in a process group of its own (Unix), so that killing
/// the group reaches everything it started.
pub(crate) fn isolate(command: &mut Command) {
    #[cfg(unix)]
    command.process_group(0);
    #[cfg(not(unix))]
    let _ = command;
}

/// The roots of the trees not stopped yet.
static LIVE: Mutex<BTreeSet<u32>> = Mutex::new(BTreeSet::new());

fn live() -> std::sync::MutexGuard<'static, BTreeSet<u32>> {
    LIVE.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Kill every stdio server this process started that has not been stopped
/// yet, with the processes they started, and wait for the kill to be sent.
/// For exits that skip destructors, such as `std::process::exit`.
pub fn kill_all_servers() {
    let roots = std::mem::take(&mut *live());
    for root in roots {
        kill_tree_blocking(root);
    }
}

/// The processes of one spawned server. Dropped while the server runs, it
/// kills them all.
#[derive(Debug)]
pub(crate) struct ProcessTree {
    /// The root's id until the tree has been stopped.
    root: Option<u32>,
}

impl ProcessTree {
    pub(crate) fn new(root: Option<u32>) -> Self {
        if let Some(root) = root {
            live().insert(root);
        }
        Self { root }
    }

    /// The root's id, no longer listed as live: the caller stops the tree.
    fn take(&mut self) -> Option<u32> {
        let root = self.root.take()?;
        live().remove(&root);
        Some(root)
    }

    /// Kill every process of the tree; the root is still running.
    pub(crate) async fn kill(&mut self) {
        if let Some(root) = self.take() {
            kill_tree(root).await;
        }
    }

    /// The root exited: kill what it left behind in its process group
    /// (Unix). On Windows nothing is done, as a dead root's id may already
    /// name another process.
    pub(crate) fn reap(&mut self) {
        if let Some(root) = self.take() {
            kill_group(root);
        }
    }
}

impl Drop for ProcessTree {
    fn drop(&mut self) {
        if let Some(root) = self.take() {
            kill_tree_now(root);
        }
    }
}

#[cfg(unix)]
fn kill_group(root: u32) {
    use rustix::process::{Pid, Signal, kill_process_group};
    if let Some(pid) = i32::try_from(root).ok().and_then(Pid::from_raw) {
        let _ = kill_process_group(pid, Signal::KILL);
    }
}

#[cfg(not(unix))]
fn kill_group(_root: u32) {}

#[cfg(unix)]
async fn kill_tree(root: u32) {
    kill_group(root);
}

#[cfg(unix)]
fn kill_tree_now(root: u32) {
    kill_group(root);
}

#[cfg(unix)]
fn kill_tree_blocking(root: u32) {
    kill_group(root);
}

#[cfg(windows)]
async fn kill_tree(root: u32) {
    let status = taskkill(root).status();
    let _ = tokio::time::timeout(TASKKILL_WAIT, status).await;
}

#[cfg(windows)]
fn kill_tree_now(root: u32) {
    let _ = taskkill(root).spawn();
}

#[cfg(windows)]
fn kill_tree_blocking(root: u32) {
    let _ = std::process::Command::new("taskkill")
        .args(["/T", "/F", "/PID", &root.to_string()])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status();
}

#[cfg(windows)]
fn taskkill(root: u32) -> Command {
    let mut command = Command::new("taskkill");
    command
        .args(["/T", "/F", "/PID", &root.to_string()])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    command
}

#[cfg(not(any(unix, windows)))]
async fn kill_tree(_root: u32) {}

#[cfg(not(any(unix, windows)))]
fn kill_tree_now(_root: u32) {}

#[cfg(not(any(unix, windows)))]
fn kill_tree_blocking(_root: u32) {}

/// The program to run for `command`: on Windows a bare name (no directory,
/// no extension) is looked up in `PATH` with every `PATHEXT` extension, so
/// `npx` finds `npx.cmd`; elsewhere, and when nothing matches, `command`
/// itself.
pub(crate) fn program(command: &str) -> PathBuf {
    #[cfg(windows)]
    {
        let path = std::env::var_os("PATH");
        let pathext = std::env::var("PATHEXT").ok();
        if let Some(found) = find_program(command, path.as_deref(), pathext.as_deref(), |p| {
            p.is_file()
        }) {
            return found;
        }
    }
    PathBuf::from(command)
}

/// The Windows `PATH` lookup of [`program`]: the first `dir/name.ext` that
/// `is_file` accepts, trying each directory of `path` in order and each
/// extension of `pathext` (default `.COM;.EXE;.BAT;.CMD`) in order.
#[cfg_attr(not(windows), allow(dead_code))]
pub(crate) fn find_program(
    name: &str,
    path: Option<&std::ffi::OsStr>,
    pathext: Option<&str>,
    is_file: impl Fn(&Path) -> bool,
) -> Option<PathBuf> {
    let bare = Path::new(name);
    let has_dir = name.contains(['/', '\\']);
    if name.is_empty() || has_dir || bare.extension().is_some() {
        return None;
    }
    let pathext = pathext.unwrap_or(".COM;.EXE;.BAT;.CMD");
    let extensions: Vec<&str> = pathext.split(';').filter(|e| !e.is_empty()).collect();
    std::env::split_paths(path?).find_map(|dir| {
        extensions
            .iter()
            .map(|ext| dir.join(format!("{name}{}", ext.to_ascii_lowercase())))
            .find(|candidate| is_file(candidate))
    })
}

#[cfg(test)]
mod tests {
    use std::ffi::OsString;

    use super::*;

    fn path_of(dirs: &[&str]) -> OsString {
        std::env::join_paths(dirs).unwrap()
    }

    #[test]
    fn a_bare_name_finds_a_cmd_launcher_in_path_order() {
        let path = path_of(&["first", "second"]);
        let exists = |p: &Path| {
            p == Path::new("second").join("npx.cmd") || p == Path::new("first").join("npx.bat")
        };
        let found = find_program("npx", Some(&path), Some(".EXE;.BAT;.CMD"), exists);
        assert_eq!(found, Some(Path::new("first").join("npx.bat")));
        let found = find_program("npx", Some(&path), None, |p| {
            p == Path::new("second").join("npx.cmd")
        });
        assert_eq!(found, Some(Path::new("second").join("npx.cmd")));
    }

    #[test]
    fn names_with_a_directory_or_extension_are_left_alone() {
        let path = path_of(&["bin"]);
        let any = |_: &Path| true;
        assert_eq!(find_program("node.exe", Some(&path), None, any), None);
        assert_eq!(find_program("./npx", Some(&path), None, any), None);
        assert_eq!(find_program("tools\\npx", Some(&path), None, any), None);
        assert_eq!(find_program("npx", None, None, any), None);
        assert_eq!(find_program("npx", Some(&path), None, |_| false), None);
    }

    #[cfg(not(windows))]
    #[test]
    fn elsewhere_the_command_is_run_as_written() {
        assert_eq!(program("npx"), PathBuf::from("npx"));
    }
}
