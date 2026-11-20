//! Running a provider's program (`op`, `gpg`, a `cmd` argv) with no shell.

use std::io::ErrorKind;
use std::process::Stdio;

use tokio::io::AsyncWriteExt;
use tokio::process::Command;
use zeroize::Zeroizing;

use crate::context::Context;

/// What a finished program wrote.
pub struct Output {
    pub success: bool,
    pub code: Option<i32>,
    /// Holds the secret: zeroized when dropped.
    pub stdout: Zeroizing<Vec<u8>>,
    pub stderr: String,
}

/// Why a program could not run.
pub enum RunError {
    /// The program is not installed (not on `PATH`).
    NotFound,
    Failed(String),
}

/// Run `argv` with `stdin` written to it, the `PATH` of `ctx`, and no
/// shell. The child is killed if the fetch is dropped (timed out).
pub async fn run(ctx: &Context, argv: &[String], stdin: Option<&[u8]>) -> Result<Output, RunError> {
    spawn_and_wait(ctx, argv, stdin, false).await
}

/// [`run`] for a user's `cmd` program, which may start programs of its
/// own: on Unix it leads a process group, and a timed-out fetch kills the
/// whole group. (Only `cmd`: `op` may prompt on the terminal, which a
/// background process group cannot read.)
pub async fn run_group(ctx: &Context, argv: &[String]) -> Result<Output, RunError> {
    spawn_and_wait(ctx, argv, None, true).await
}

async fn spawn_and_wait(
    ctx: &Context,
    argv: &[String],
    stdin: Option<&[u8]>,
    own_group: bool,
) -> Result<Output, RunError> {
    let (program, args) = argv.split_first().ok_or(RunError::NotFound)?;
    // Windows also searches the parent's PATH and the system folders after
    // the child's, so a program the context's PATH lacks could still run.
    // Look it up in the context's PATH only.
    #[cfg(windows)]
    let program = &locate(
        ctx.var("PATH").as_deref().unwrap_or_default(),
        program,
        &[".exe", ".cmd", ".bat", ".com"],
    )
    .ok_or(RunError::NotFound)?;
    let mut command = Command::new(program);
    command
        .args(args)
        .stdin(if stdin.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    #[cfg(unix)]
    if own_group {
        command.process_group(0);
    }
    // The program search path and gpg's home come from the context, so
    // tests can point them at fakes.
    for name in ["PATH", "GNUPGHOME"] {
        if let Some(value) = ctx.var(name) {
            command.env(name, value);
        }
    }
    let mut child = command.spawn().map_err(|e| match e.kind() {
        ErrorKind::NotFound => RunError::NotFound,
        _ => RunError::Failed(e.to_string()),
    })?;
    let mut group = GroupGuard(child.id().filter(|_| own_group));
    if let (Some(input), Some(mut pipe)) = (stdin, child.stdin.take()) {
        pipe.write_all(input)
            .await
            .map_err(|e| RunError::Failed(e.to_string()))?;
    }
    let output = child
        .wait_with_output()
        .await
        .map_err(|e| RunError::Failed(e.to_string()))?;
    group.0 = None;
    Ok(Output {
        success: output.status.success(),
        code: output.status.code(),
        stdout: Zeroizing::new(output.stdout),
        stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
    })
}

/// `program` found in the `;`-separated `path` folders (trying each of
/// `extensions` when it has none), or itself when it names a path.
#[cfg_attr(not(windows), allow(dead_code))]
fn locate(path: &str, program: &str, extensions: &[&str]) -> Option<std::path::PathBuf> {
    let named = std::path::Path::new(program);
    if named.components().count() > 1 || named.is_absolute() {
        return Some(named.to_path_buf());
    }
    let candidates = |dir: &str| {
        let base = std::path::Path::new(dir).join(program);
        let mut all = vec![base.clone()];
        if named.extension().is_none() {
            all.extend(extensions.iter().map(|ext| {
                let mut with = base.clone().into_os_string();
                with.push(ext);
                std::path::PathBuf::from(with)
            }));
        }
        all
    };
    path.split(';')
        .filter(|dir| !dir.is_empty())
        .flat_map(candidates)
        .find(|candidate| candidate.is_file())
}

/// The process group of a running program, killed when dropped before
/// the program finished (a timed-out fetch drops the future).
/// (Windows has no process groups here: the field is only read on Unix.)
#[cfg_attr(not(unix), allow(dead_code))]
struct GroupGuard(Option<u32>);

impl Drop for GroupGuard {
    fn drop(&mut self) {
        #[cfg(unix)]
        if let Some(pid) = self
            .0
            .and_then(|id| i32::try_from(id).ok())
            .and_then(rustix::process::Pid::from_raw)
        {
            let _ = rustix::process::kill_process_group(pid, rustix::process::Signal::KILL);
        }
    }
}

/// A program's stdout as text without the trailing line break(s) and
/// surrounding whitespace.
pub fn trimmed(stdout: &[u8]) -> Result<String, std::str::Utf8Error> {
    std::str::from_utf8(stdout).map(|s| s.trim().to_owned())
}

#[cfg(test)]
mod tests {
    use super::locate;

    /// Regression: on Windows a program missing from the context's PATH
    /// still ran from the parent's PATH (the runner's `az`), so the
    /// credential chain timed out instead of finding no credentials.
    #[test]
    fn programs_are_found_only_in_the_given_path() {
        let dir = tempfile::tempdir().unwrap();
        let empty = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("az.cmd"), "").unwrap();
        let exts = [".exe", ".cmd"];
        let path = format!("{};{}", empty.path().display(), dir.path().display());
        assert_eq!(locate(&path, "az", &exts), Some(dir.path().join("az.cmd")));
        assert_eq!(
            locate(&path, "az.cmd", &exts),
            Some(dir.path().join("az.cmd"))
        );
        assert_eq!(
            locate(&empty.path().display().to_string(), "az", &exts),
            None
        );
        assert_eq!(locate("", "az", &exts), None);
    }
}
