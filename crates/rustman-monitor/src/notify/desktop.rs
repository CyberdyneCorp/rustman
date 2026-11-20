//! Desktop notifications. Rather than linking a notification library, the
//! system's own tool is run: `osascript` on macOS, `notify-send` on Linux
//! and the BSDs, and PowerShell's toast API on Windows. Nothing is
//! installed or linked, and the title and text are passed as arguments or
//! environment variables, never spliced into a script. A tool that hangs
//! (a stuck notification daemon) is killed after [`super::TIMEOUT`], so the
//! channel stays bounded even though the call is synchronous.

use std::io::Read;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// Shows a notification on the user's desktop.
pub trait Desktop: Sync {
    fn show(&self, title: &str, text: &str) -> Result<(), String>;
}

/// The operating system's notifications.
#[derive(Clone, Copy, Debug, Default)]
pub struct SystemDesktop;

impl Desktop for SystemDesktop {
    fn show(&self, title: &str, text: &str) -> Result<(), String> {
        run_bounded(command(title, text), super::TIMEOUT)
    }
}

/// Run `command`, killing it when it outlives `limit`; its first stderr
/// line explains a failure.
fn run_bounded(mut command: Command, limit: Duration) -> Result<(), String> {
    let program = command.get_program().to_string_lossy().into_owned();
    let mut child = command
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("cannot run {program}: {e}"))?;
    let deadline = Instant::now() + limit;
    let status = loop {
        if let Some(status) = child.try_wait().map_err(|e| e.to_string())? {
            break status;
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            return Err(format!(
                "{program} timed out after {}s",
                limit.as_secs_f32()
            ));
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    if status.success() {
        return Ok(());
    }
    let mut stderr = String::new();
    if let Some(mut pipe) = child.stderr.take() {
        let _ = pipe.read_to_string(&mut stderr);
    }
    let reason = stderr.lines().next().unwrap_or("no output").trim();
    Err(format!("{program} failed ({status}): {reason}"))
}

/// `osascript` reads its arguments through `on run argv`.
#[cfg(target_os = "macos")]
fn command(title: &str, text: &str) -> Command {
    let mut command = Command::new("osascript");
    command.args([
        "-e",
        "on run argv",
        "-e",
        "display notification (item 2 of argv) with title (item 1 of argv)",
        "-e",
        "end run",
        title,
        text,
    ]);
    command
}

/// Windows shows a toast only for a registered AppUserModelID, so the toast
/// is raised under Windows PowerShell's own, which every install has.
#[cfg_attr(not(windows), allow(dead_code))]
const POWERSHELL_APP_ID: &str =
    r"{1AC14E77-02E7-4E5D-B744-2EB1AE5198B7}\WindowsPowerShell\v1.0\powershell.exe";

/// A toast through the Windows Runtime API of Windows PowerShell; the
/// title and text come from environment variables.
#[cfg_attr(not(windows), allow(dead_code))]
const TOAST_SCRIPT: &str = "$ErrorActionPreference = 'Stop'; \
[Windows.UI.Notifications.ToastNotificationManager, Windows.UI.Notifications, ContentType = WindowsRuntime] > $null; \
$xml = [Windows.UI.Notifications.ToastNotificationManager]::GetTemplateContent([Windows.UI.Notifications.ToastTemplateType]::ToastText02); \
$texts = $xml.GetElementsByTagName('text'); \
$texts.Item(0).AppendChild($xml.CreateTextNode($env:RUSTMAN_NOTIFY_TITLE)) > $null; \
$texts.Item(1).AppendChild($xml.CreateTextNode($env:RUSTMAN_NOTIFY_TEXT)) > $null; \
$toast = [Windows.UI.Notifications.ToastNotification]::new($xml); \
[Windows.UI.Notifications.ToastNotificationManager]::CreateToastNotifier($env:RUSTMAN_NOTIFY_APP_ID).Show($toast)";

#[cfg(windows)]
fn command(title: &str, text: &str) -> Command {
    let mut command = Command::new("powershell");
    command
        .args(["-NoProfile", "-NonInteractive", "-Command", TOAST_SCRIPT])
        .env("RUSTMAN_NOTIFY_APP_ID", POWERSHELL_APP_ID)
        .env("RUSTMAN_NOTIFY_TITLE", title)
        .env("RUSTMAN_NOTIFY_TEXT", text);
    command
}

#[cfg(not(any(target_os = "macos", windows)))]
fn command(title: &str, text: &str) -> Command {
    let mut command = Command::new("notify-send");
    command.args(["--app-name=Rustman", "--", title, text]);
    command
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The title and text are separate arguments (or variables), never part
    /// of a script, so quotes in them cannot change what runs.
    #[test]
    fn the_title_and_text_are_passed_as_data() {
        let title = "[FIRING] api's \"health\"";
        let command = command(title, "line'1");
        let args: Vec<String> = command
            .get_args()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        let envs: Vec<String> = command
            .get_envs()
            .filter_map(|(_, v)| v.map(|v| v.to_string_lossy().into_owned()))
            .collect();
        let passed: Vec<&String> = args.iter().chain(&envs).collect();
        assert!(passed.iter().any(|a| *a == title), "{passed:?}");
        assert!(passed.iter().any(|a| *a == "line'1"), "{passed:?}");
        let script: Vec<&String> = args.iter().filter(|a| a.contains("notif")).collect();
        assert!(script.iter().all(|s| !s.contains("api's")), "{script:?}");
    }

    /// An unregistered app id makes Windows drop the toast silently.
    #[test]
    fn windows_toasts_use_a_registered_app_id() {
        assert!(TOAST_SCRIPT.contains("CreateToastNotifier($env:RUSTMAN_NOTIFY_APP_ID)"));
        assert!(!TOAST_SCRIPT.contains("'Rustman'"));
        assert!(POWERSHELL_APP_ID.ends_with(r"\WindowsPowerShell\v1.0\powershell.exe"));
    }

    /// A hung notification tool is killed at the limit, not waited on.
    #[test]
    fn a_hung_tool_is_killed_at_the_limit() {
        let hang = if cfg!(windows) {
            let mut c = Command::new("powershell");
            c.args(["-NoProfile", "-Command", "Start-Sleep -Seconds 30"]);
            c
        } else {
            let mut c = Command::new("sleep");
            c.arg("30");
            c
        };
        let started = Instant::now();
        let error = run_bounded(hang, Duration::from_millis(300)).unwrap_err();
        assert!(error.contains("timed out"), "{error}");
        assert!(started.elapsed() < Duration::from_secs(20), "{error}");
    }

    #[test]
    fn a_failing_tool_reports_its_stderr() {
        let fail = if cfg!(windows) {
            let mut c = Command::new("powershell");
            c.args([
                "-NoProfile",
                "-Command",
                "[Console]::Error.WriteLine('boom'); exit 3",
            ]);
            c
        } else {
            let mut c = Command::new("sh");
            c.args(["-c", "echo boom >&2; exit 3"]);
            c
        };
        let error = run_bounded(fail, Duration::from_secs(30)).unwrap_err();
        assert!(error.contains("boom"), "{error}");
    }
}
