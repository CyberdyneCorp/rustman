//! systemd timers and launchd agents. An `every` schedule maps to an
//! interval (`OnUnitActiveSec`, `StartInterval`); a cron schedule to a
//! calendar (`OnCalendar`, `StartCalendarInterval`). Cron fields that a
//! calendar cannot express are refused with a hint.

use std::fmt::Write as _;

use super::expand_macro;
use crate::definition::Monitor;
use crate::duration;
use crate::schedule::Schedule;

const DAYS: [&str; 7] = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];
/// launchd needs one dictionary per combination; past this, use `every`.
const MAX_INTERVALS: usize = 500;

/// A user service and timer, both in one text.
pub(super) fn systemd(monitor: &Monitor, root: &str, program: &str) -> Result<String, String> {
    let name = &monitor.name;
    let trigger = match &monitor.schedule {
        Schedule::Every(every) => {
            let every = duration::format(*every);
            format!("OnActiveSec=0\nOnUnitActiveSec={every}")
        }
        Schedule::Cron(_) => {
            let calendar = on_calendar(&monitor.schedule)?;
            format!("OnCalendar={calendar}\nPersistent=false")
        }
    };
    Ok(format!(
        "# ~/.config/systemd/user/rustman-monitor-{name}.service
[Unit]
Description=Rustman monitor {name}

[Service]
Type=oneshot
WorkingDirectory={root}
ExecStart={program} monitor run {name}

# ~/.config/systemd/user/rustman-monitor-{name}.timer
# Enable with: systemctl --user enable --now rustman-monitor-{name}.timer
[Unit]
Description=Run the Rustman monitor {name} ({schedule})

[Timer]
{trigger}

[Install]
WantedBy=timers.target
",
        schedule = monitor.schedule,
    ))
}

/// A LaunchAgent property list.
pub(super) fn launchd(monitor: &Monitor, root: &str, program: &str) -> Result<String, String> {
    let name = &monitor.name;
    let label = format!("dev.rustman.monitor.{name}");
    let trigger = match &monitor.schedule {
        Schedule::Every(every) => format!(
            "  <key>StartInterval</key>\n  <integer>{}</integer>\n  <key>RunAtLoad</key>\n  <true/>\n",
            every.as_secs().max(1)
        ),
        Schedule::Cron(_) => calendar_intervals(&monitor.schedule)?,
    };
    let tz_note = match monitor.schedule.tz_name() {
        Some(tz) => format!("\n     launchd uses the Mac's timezone; the monitor file says {tz}."),
        None => String::new(),
    };
    Ok(format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<!-- ~/Library/LaunchAgents/{label}.plist: runs the {name} monitor ({schedule}).
     Load with: launchctl bootstrap gui/$(id -u) ~/Library/LaunchAgents/{label}.plist{tz_note} -->
<plist version="1.0">
<dict>
  <key>Label</key>
  <string>{label}</string>
  <key>WorkingDirectory</key>
  <string>{root}</string>
  <key>ProgramArguments</key>
  <array>
    <string>{program}</string>
    <string>monitor</string>
    <string>run</string>
    <string>{name}</string>
  </array>
{trigger}</dict>
</plist>
"#,
        schedule = monitor.schedule,
        root = xml_escape(root),
        program = xml_escape(program),
    ))
}

/// The five fields of a cron schedule, macros expanded.
fn fields(schedule: &Schedule) -> Result<[String; 5], String> {
    let expression = expand_macro(schedule.cron_expression().unwrap_or_default());
    let parts: Vec<String> = expression.split_whitespace().map(str::to_owned).collect();
    parts
        .try_into()
        .map_err(|_| format!("cannot convert cron '{expression}'"))
}

/// `Mon..Fri *-*-* 9:0/15:00 Europe/Lisbon`.
fn on_calendar(schedule: &Schedule) -> Result<String, String> {
    let [minute, hour, dom, month, dow] = fields(schedule)?;
    if dom != "*" && dow != "*" {
        return Err(unsupported(
            "a cron with both a day of month and a day of week",
        ));
    }
    let mut out = String::new();
    if dow != "*" {
        let days = values(&dow, 0, 7, true)?;
        let names: Vec<&str> = days.iter().map(|d| DAYS[*d as usize % 7]).collect();
        let _ = write!(out, "{} ", names.join(","));
    }
    let _ = write!(
        out,
        "*-{}-{} {}:{}:00",
        calendar_field(&month, 1)?,
        calendar_field(&dom, 1)?,
        calendar_field(&hour, 0)?,
        calendar_field(&minute, 0)?
    );
    if let Some(tz) = schedule.tz_name() {
        let _ = write!(out, " {tz}");
    }
    Ok(out)
}

/// One field in systemd's syntax: `*`, `0/15`, `1..5`, `1,3`.
fn calendar_field(field: &str, first: u32) -> Result<String, String> {
    if field == "*" {
        return Ok("*".to_owned());
    }
    let parts: Result<Vec<String>, String> = field
        .split(',')
        .map(|part| match part.split_once('/') {
            Some(("*", step)) => Ok(format!("{first}/{step}")),
            Some(_) => Err(unsupported("a stepped range")),
            None => Ok(part.replace('-', "..")),
        })
        .collect();
    Ok(parts?.join(","))
}

/// `StartCalendarInterval`: one dictionary per combination of the fields
/// that are not `*`.
fn calendar_intervals(schedule: &Schedule) -> Result<String, String> {
    let [minute, hour, dom, month, dow] = fields(schedule)?;
    if dom != "*" && dow != "*" {
        return Err(unsupported(
            "a cron with both a day of month and a day of week",
        ));
    }
    let keys = [
        ("Minute", minute, 0, 59, false),
        ("Hour", hour, 0, 23, false),
        ("Day", dom, 1, 31, false),
        ("Month", month, 1, 12, false),
        ("Weekday", dow, 0, 7, true),
    ];
    let mut combos: Vec<Vec<(&str, u32)>> = vec![Vec::new()];
    for (key, field, min, max, days) in &keys {
        if field == "*" {
            continue;
        }
        let values = values(field, *min, *max, *days)?;
        combos = combos
            .iter()
            .flat_map(|combo| {
                values.iter().map(move |v| {
                    let mut next = combo.clone();
                    next.push((*key, *v));
                    next
                })
            })
            .collect();
        if combos.len() > MAX_INTERVALS {
            return Err(unsupported("this many calendar entries"));
        }
    }
    let mut out = String::from("  <key>StartCalendarInterval</key>\n  <array>\n");
    for combo in combos {
        out.push_str("    <dict>\n");
        for (key, value) in combo {
            let _ = writeln!(
                out,
                "      <key>{key}</key>\n      <integer>{value}</integer>"
            );
        }
        out.push_str("    </dict>\n");
    }
    out.push_str("  </array>\n");
    Ok(out)
}

/// The values a cron field matches: numbers, `a-b`, `*/n`, `a-b/n`, lists,
/// and day names for the day of week (7 is Sunday, 0).
fn values(field: &str, min: u32, max: u32, days: bool) -> Result<Vec<u32>, String> {
    let number = |text: &str| -> Result<u32, String> {
        let lower = text.to_ascii_lowercase();
        let named = DAYS
            .iter()
            .position(|d| d.eq_ignore_ascii_case(&lower))
            .filter(|_| days);
        let value = match named {
            Some(i) => u32::try_from(i).unwrap_or(0),
            None => text
                .parse()
                .map_err(|_| format!("cannot convert cron field '{field}'"))?,
        };
        Ok(if days { value % 7 } else { value })
    };
    let mut out = Vec::new();
    for part in field.split(',') {
        let (range, step) = match part.split_once('/') {
            Some((range, step)) => (range, number(step)?.max(1)),
            None => (part, 1),
        };
        let (from, to) = match range.split_once('-') {
            _ if range == "*" => (min, if days { 6 } else { max }),
            Some((a, b)) => (number(a)?, number(b)?),
            None if step > 1 => (number(range)?, max),
            None => {
                let v = number(range)?;
                (v, v)
            }
        };
        out.extend((from..=to).step_by(step as usize));
    }
    out.sort_unstable();
    out.dedup();
    Ok(out)
}

fn unsupported(what: &str) -> String {
    format!(
        "{what} has no calendar equivalent here; use every: in the monitor file, or the cron recipe"
    )
}

fn xml_escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}
