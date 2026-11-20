//! `rustman monitor schedule-recipe` (monitors "External scheduler
//! recipes", design D6): the configuration an external scheduler needs to
//! run `rustman monitor run <name>` on the monitor's schedule. Recipes are
//! text; nothing is installed.

mod calendar;

use std::fmt::Write as _;
use std::path::Path;

use crate::definition::Monitor;
use crate::duration;
use crate::schedule::Schedule;

/// The schedulers a recipe is written for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Target {
    Cron,
    Systemd,
    Launchd,
    GithubActions,
}

/// The recipe for `monitor` of the workspace at `root`, running `program`
/// (the `rustman` executable). GitHub Actions always runs `rustman` from
/// the job's PATH, in the checkout.
pub fn recipe(
    monitor: &Monitor,
    root: &Path,
    program: &str,
    target: Target,
) -> Result<String, String> {
    let root = root.to_string_lossy();
    let root = without_verbatim_prefix(&root);
    match target {
        Target::Cron => crontab(monitor, &root, program),
        Target::Systemd => calendar::systemd(monitor, &root, program),
        Target::Launchd => calendar::launchd(monitor, &root, program),
        Target::GithubActions => github_actions(monitor),
    }
}

/// `C:\work` for the verbatim `\\?\C:\work` that `canonicalize` returns
/// on Windows (and `\\server\share` for `\\?\UNC\server\share`), which
/// schedulers and shells do not expect. Other paths are unchanged.
fn without_verbatim_prefix(root: &str) -> String {
    if let Some(unc) = root.strip_prefix(r"\\?\UNC\") {
        return format!(r"\\{unc}");
    }
    match root.strip_prefix(r"\\?\") {
        Some(rest) if rest.as_bytes().get(1) == Some(&b':') => rest.to_owned(),
        _ => root.to_owned(),
    }
}

/// A crontab line; `CRON_TZ` when the monitor names a timezone.
fn crontab(monitor: &Monitor, root: &str, program: &str) -> Result<String, String> {
    let expression = cron_expression(&monitor.schedule)?;
    let mut out = format!(
        "# Run the {name} monitor ({schedule}). Install with: crontab -e\n",
        name = monitor.name,
        schedule = monitor.schedule
    );
    if let Some(tz) = monitor.schedule.tz_name() {
        let _ = writeln!(out, "CRON_TZ={tz}");
    }
    let _ = writeln!(
        out,
        "{expression} cd {} && {} monitor run {}",
        shell_quote(root),
        shell_quote(program),
        monitor.name
    );
    Ok(out)
}

/// A workflow with `on.schedule.cron` and a step running the monitor.
fn github_actions(monitor: &Monitor) -> Result<String, String> {
    let expression = expand_macro(&cron_expression(&monitor.schedule)?);
    let name = &monitor.name;
    let tz_note = match monitor.schedule.tz_name() {
        Some("UTC" | "Etc/UTC") => String::new(),
        Some(tz) => format!("\n# GitHub evaluates schedules in UTC; the monitor file says {tz}."),
        None if matches!(monitor.schedule, Schedule::Cron(_)) => {
            "\n# GitHub evaluates schedules in UTC; the monitor file uses the local timezone."
                .to_owned()
        }
        None => String::new(),
    };
    let timeout_minutes = monitor.timeout.as_secs().div_ceil(60) + 15;
    Ok(format!(
        "# .github/workflows/monitor-{name}.yml: runs the {name} monitor ({schedule}).{tz_note}
# GitHub runs scheduled workflows at most every 5 minutes, sometimes late.
name: monitor {name}
on:
  schedule:
    - cron: \"{expression}\"
  workflow_dispatch: {{}}
jobs:
  monitor:
    runs-on: ubuntu-latest
    timeout-minutes: {timeout_minutes}
    env:
      # Secrets come from .rustman/secrets.yaml: add a step that runs
      # printf '%s' \"${{{{ secrets.NAME }}}}\" | rustman secret set NAME --env {env}
      RUSTMAN_SECRET_STORE: file
    steps:
      - uses: actions/checkout@v4
      - name: Install rustman
        run: cargo install --git https://github.com/CyberdyneCorp/rustman rustman-cli --locked
      - name: Run the monitor
        run: rustman monitor run {name}
",
        schedule = monitor.schedule,
        env = monitor.environment,
    ))
}

/// The cron expression of `schedule`: as written, or the one an `every`
/// interval maps to when it divides an hour or a day.
pub fn cron_expression(schedule: &Schedule) -> Result<String, String> {
    let every = match schedule {
        Schedule::Cron(_) => {
            return Ok(schedule.cron_expression().unwrap_or_default().to_owned());
        }
        Schedule::Every(every) => *every,
    };
    let secs = every.as_secs();
    let no_cron = || {
        format!(
            "every {} has no cron equivalent; use a cron schedule in the monitor file, or the systemd or launchd recipe",
            duration::format(every)
        )
    };
    if every.subsec_millis() != 0 || secs % 60 != 0 {
        return Err(no_cron());
    }
    let minutes = secs / 60;
    match minutes {
        1 => Ok("* * * * *".to_owned()),
        m if m < 60 && 60 % m == 0 => Ok(format!("*/{m} * * * *")),
        m if m % 60 == 0 && m < 1440 && 24 % (m / 60) == 0 => Ok(format!("0 */{} * * *", m / 60)),
        1440 => Ok("0 0 * * *".to_owned()),
        _ => Err(no_cron()),
    }
}

/// `@hourly`, `@daily` and `@weekly` as 5 fields.
pub(crate) fn expand_macro(expression: &str) -> String {
    match expression.to_ascii_lowercase().as_str() {
        "@hourly" => "0 * * * *".to_owned(),
        "@daily" => "0 0 * * *".to_owned(),
        "@weekly" => "0 0 * * 0".to_owned(),
        _ => expression.to_owned(),
    }
}

/// `text` in single quotes for a POSIX shell.
fn shell_quote(text: &str) -> String {
    if !text.is_empty()
        && text
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "/._-+:@".contains(c))
    {
        return text.to_owned();
    }
    format!("'{}'", text.replace('\'', "'\\''"))
}

#[cfg(test)]
mod tests;
