//! Where the companion is (design D3): `RUSTMAN_VIZ`, then `PATH`, then
//! next to the running executable. The first hit is probed with
//! `--protocol-check` before use, and the probe's result is kept for the
//! rest of the process (one CLI command or one TUI session).

use std::ffi::OsString;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Mutex, mpsc};
use std::time::Duration;

use rustman_viz_protocol::{PROTOCOL, is_compatible, major};

use crate::connect::wait_for_exit;
use crate::{COMPANION, OVERRIDE_VAR, PROTOCOL_CHECK, VizError};

/// How long `--protocol-check` may take.
const PROBE_TIMEOUT: Duration = Duration::from_secs(5);

/// How much of the `--protocol-check` output is read.
const PROBE_OUTPUT_LIMIT: u64 = 64 * 1024;

/// The probes of this process, by executable.
static PROBED: Mutex<Vec<(PathBuf, Result<(), VizError>)>> = Mutex::new(Vec::new());

/// Where the companion is looked for.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Discovery {
    /// `RUSTMAN_VIZ`, when set and not empty.
    pub override_path: Option<PathBuf>,
    /// The `PATH` value.
    pub path: Option<OsString>,
    /// The directory of the running `rustman` executable.
    pub exe_dir: Option<PathBuf>,
}

impl Discovery {
    /// The process environment and executable.
    pub fn from_env() -> Self {
        let exe_dir = std::env::current_exe()
            .ok()
            .and_then(|exe| exe.parent().map(Path::to_path_buf));
        Self {
            override_path: std::env::var_os(OVERRIDE_VAR)
                .filter(|v| !v.is_empty())
                .map(PathBuf::from),
            path: std::env::var_os("PATH"),
            exe_dir,
        }
    }

    /// The companion to start, without probing it. An override that is not
    /// an executable file is an error naming it; nothing else is tried then.
    pub fn find(&self) -> Result<PathBuf, VizError> {
        if let Some(path) = &self.override_path {
            return if is_executable(path) {
                Ok(path.clone())
            } else {
                Err(VizError::NotFound(format!(
                    "{OVERRIDE_VAR}={} is not an executable file",
                    path.display()
                )))
            };
        }
        let on_path = self
            .path
            .iter()
            .flat_map(std::env::split_paths)
            .map(|dir| dir.join(COMPANION));
        let sibling = self.exe_dir.iter().map(|dir| dir.join(COMPANION));
        on_path
            .chain(sibling)
            .find(|c| is_executable(c))
            .ok_or_else(|| {
                VizError::NotFound(format!(
                    "no {COMPANION} on PATH or next to rustman; set {OVERRIDE_VAR}"
                ))
            })
    }

    /// The companion to start, once `--protocol-check` says it speaks this
    /// build's protocol. Each executable is probed once per process.
    pub fn locate(&self) -> Result<PathBuf, VizError> {
        let program = self.find()?;
        probe_cached(&program)?;
        Ok(program)
    }
}

fn probe_cached(program: &Path) -> Result<(), VizError> {
    let cached = PROBED
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .iter()
        .find(|(path, _)| path == program)
        .map(|(_, result)| result.clone());
    if let Some(result) = cached {
        return result;
    }
    let result = probe(program);
    PROBED
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .push((program.to_path_buf(), result.clone()));
    result
}

/// Run `program --protocol-check` and check that one of the protocols it
/// prints (one per line) is this build's.
pub fn probe(program: &Path) -> Result<(), VizError> {
    let mut child = Command::new(program)
        .arg(PROTOCOL_CHECK)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| VizError::failed(format!("cannot start {}: {e}", program.display())))?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| VizError::failed("no stdout"))?;
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let mut text = String::new();
        let _ = stdout.take(PROBE_OUTPUT_LIMIT).read_to_string(&mut text);
        let _ = tx.send(text);
    });
    let Ok(output) = rx.recv_timeout(PROBE_TIMEOUT) else {
        let _ = child.kill();
        let _ = child.wait();
        return Err(VizError::failed(format!(
            "{PROTOCOL_CHECK} did not answer within 5 s"
        )));
    };
    let status = wait_for_exit(&mut child, PROBE_TIMEOUT);
    if status.is_none() {
        let _ = child.kill();
        let _ = child.wait();
    }
    match status.and_then(|s| s.code()) {
        Some(0) => {}
        Some(crate::EXIT_NO_GRAPHICS) => return Err(VizError::Graphics),
        Some(code) => {
            return Err(VizError::failed(format!(
                "{PROTOCOL_CHECK} exited with code {code}"
            )));
        }
        None => return Err(VizError::failed(format!("{PROTOCOL_CHECK} did not exit"))),
    }
    let spoken: Vec<&str> = output
        .lines()
        .map(str::trim)
        .filter(|line| major(line).is_some())
        .collect();
    if spoken.iter().any(|p| is_compatible(p)) {
        Ok(())
    } else if spoken.is_empty() {
        Err(VizError::failed(format!(
            "{PROTOCOL_CHECK} named no protocol (expected {PROTOCOL})"
        )))
    } else {
        Err(VizError::Rejected {
            theirs: spoken.join(", "),
        })
    }
}

#[cfg(unix)]
fn is_executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path).is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
}

#[cfg(not(unix))]
fn is_executable(path: &Path) -> bool {
    path.is_file()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_executable(path: &Path) {
        std::fs::write(path, "").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
    }

    #[test]
    fn the_override_wins_over_path() {
        let tmp = tempfile::tempdir().unwrap();
        let (opt, bin) = (tmp.path().join("opt"), tmp.path().join("bin"));
        std::fs::create_dir_all(&opt).unwrap();
        std::fs::create_dir_all(&bin).unwrap();
        make_executable(&opt.join(COMPANION));
        make_executable(&bin.join(COMPANION));
        let discovery = Discovery {
            override_path: Some(opt.join(COMPANION)),
            path: Some(bin.clone().into_os_string()),
            exe_dir: None,
        };
        assert_eq!(discovery.find(), Ok(opt.join(COMPANION)));
        let discovery = Discovery {
            override_path: None,
            ..discovery
        };
        assert_eq!(discovery.find(), Ok(bin.join(COMPANION)));
    }

    #[test]
    fn the_sibling_is_used_last() {
        let tmp = tempfile::tempdir().unwrap();
        make_executable(&tmp.path().join(COMPANION));
        let discovery = Discovery {
            override_path: None,
            path: Some(OsString::new()),
            exe_dir: Some(tmp.path().to_path_buf()),
        };
        assert_eq!(discovery.find(), Ok(tmp.path().join(COMPANION)));
    }

    #[test]
    fn an_invalid_override_names_the_path_and_does_not_fall_back() {
        let tmp = tempfile::tempdir().unwrap();
        make_executable(&tmp.path().join(COMPANION));
        let nope = tmp.path().join("nope").join(COMPANION);
        let discovery = Discovery {
            override_path: Some(nope.clone()),
            path: Some(tmp.path().as_os_str().to_owned()),
            exe_dir: Some(tmp.path().to_path_buf()),
        };
        let err = discovery.locate().unwrap_err();
        assert!(
            err.to_string().contains(&nope.display().to_string()),
            "{err}"
        );
        assert!(err.to_string().starts_with("Viz companion not found"));
    }

    #[test]
    fn nothing_found_is_not_found() {
        let err = Discovery::default().locate().unwrap_err();
        assert!(matches!(err, VizError::NotFound(_)), "{err}");
    }

    /// A companion whose `--protocol-check` prints `out` and exits `code`;
    /// each probe appends a line to `<script>.count`.
    #[cfg(unix)]
    fn checker(dir: &Path, out: &str, code: u8) -> PathBuf {
        use std::os::unix::fs::PermissionsExt;
        let script = dir.join(COMPANION);
        let text = format!("#!/bin/sh\necho probe >> \"$0.count\"\nprintf '{out}'\nexit {code}\n");
        std::fs::write(&script, text).unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        script
    }

    #[cfg(unix)]
    #[test]
    fn the_protocol_check_accepts_a_compatible_companion_and_is_cached() {
        let tmp = tempfile::tempdir().unwrap();
        let script = checker(tmp.path(), "rustman-viz/1\\n", 0);
        let discovery = Discovery {
            override_path: Some(script.clone()),
            ..Discovery::default()
        };
        assert_eq!(discovery.locate(), Ok(script.clone()));
        assert_eq!(discovery.locate(), Ok(script.clone()));
        let count = std::fs::read_to_string(format!("{}.count", script.display())).unwrap();
        assert_eq!(count.lines().count(), 1, "probed once per process");
    }

    #[cfg(unix)]
    #[test]
    fn the_protocol_check_rejects_other_versions() {
        let tmp = tempfile::tempdir().unwrap();
        let script = checker(tmp.path(), "rustman-viz/2\\nrustman-viz/3\\n", 0);
        assert_eq!(
            probe(&script),
            Err(VizError::Rejected {
                theirs: "rustman-viz/2, rustman-viz/3".into()
            })
        );
        let err = probe(&script).unwrap_err().to_string();
        assert_eq!(
            err,
            "Viz companion speaks rustman-viz/2, rustman-viz/3, rustman needs rustman-viz/1; upgrade rustman-viz"
        );
    }

    #[cfg(unix)]
    #[test]
    fn the_protocol_check_reports_graphics_and_failures() {
        let tmp = tempfile::tempdir().unwrap();
        let no_gpu = tmp.path().join("gpu");
        std::fs::create_dir(&no_gpu).unwrap();
        assert_eq!(probe(&checker(&no_gpu, "", 3)), Err(VizError::Graphics));
        let silent = tmp.path().join("silent");
        std::fs::create_dir(&silent).unwrap();
        let err = probe(&checker(&silent, "hello\\n", 0)).unwrap_err();
        assert!(err.to_string().contains("named no protocol"), "{err}");
        let crash = tmp.path().join("crash");
        std::fs::create_dir(&crash).unwrap();
        let err = probe(&checker(&crash, "", 101)).unwrap_err();
        assert!(err.to_string().contains("exited with code 101"), "{err}");
    }

    #[cfg(unix)]
    #[test]
    fn a_hanging_protocol_check_times_out() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempfile::tempdir().unwrap();
        let script = tmp.path().join(COMPANION);
        std::fs::write(&script, "#!/bin/sh\nexec sleep 60\n").unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        let started = std::time::Instant::now();
        let err = probe(&script).unwrap_err();
        assert!(
            err.to_string().contains("did not answer within 5 s"),
            "{err}"
        );
        assert!(started.elapsed() < Duration::from_secs(15));
    }
}
