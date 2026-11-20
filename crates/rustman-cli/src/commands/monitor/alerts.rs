//! Alerts as the CLI shows them, and `rustman monitor test-alert`.

use std::io::Write;

use rustman_core::{ConfigError, Workspace};
use rustman_monitor::Monitor;
use rustman_monitor::notify::{Alert, Delivery};

use super::{Session, monitor_error};
use crate::commands::exec::runtime;
use crate::commands::print_warnings;
use crate::exit::{CliError, REQUEST, SUCCESS};

/// `alert: <title>`, then `  <channel>: ok` or `  <channel>: error: <why>`
/// for each alert sent.
pub(super) fn lines(alerts: &[(Alert, Vec<Delivery>)]) -> Vec<String> {
    let mut lines = Vec::new();
    for (alert, deliveries) in alerts {
        lines.push(format!("alert: {}", alert.title()));
        lines.extend(deliveries.iter().map(|d| format!("  {}", delivery(d))));
    }
    lines
}

fn delivery(delivery: &Delivery) -> String {
    match &delivery.error {
        None => format!("{}: ok", delivery.channel),
        Some(error) => format!("{}: error: {error}", delivery.channel),
    }
}

/// Send `[TEST] <name>` through every channel; exit 0 only when every
/// channel succeeded, 3 otherwise.
pub(super) fn test_alert(ws: &Workspace, monitors: &[Monitor], name: &str) -> Result<u8, CliError> {
    let monitor = rustman_monitor::find(monitors, name)?;
    if monitor.alerts.notify.is_empty() {
        return Err(ConfigError::Usage(format!(
            "monitor '{name}' has no alerts.notify channels to test"
        ))
        .into());
    }
    let session = Session::open(ws)?;
    let deliveries = runtime()?
        .block_on(session.context(ws).test_alert(monitor))
        .map_err(monitor_error)?;
    let mut out = anstream::stdout().lock();
    for d in &deliveries {
        let _ = writeln!(out, "{}", delivery(d));
    }
    print_warnings(&session.secrets.take_warnings());
    Ok(if deliveries.iter().all(Delivery::is_ok) {
        SUCCESS
    } else {
        REQUEST
    })
}
