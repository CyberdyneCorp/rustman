//! One whole monitor cycle, the same for `rustman monitor run`, the daemon
//! and the TUI's "run now": run the targets, store the result (compacting
//! when due), evaluate the alert rules against the stored state, and send
//! the notices through the monitor's channels.

use std::time::Duration;

use jiff::Timestamp;
use rustman_core::{SecretStore, Workspace};
use rustman_http::HttpEngine;
use tokio_util::sync::CancellationToken;

use crate::definition::Monitor;
use crate::evaluate;
use crate::notify::{Alert, Delivery, Desktop, Notifier};
use crate::run::{MonitorError, MonitorRun, environment, run};
use crate::store::{RunResult, Store};

/// What a cycle needs besides the monitor.
pub struct CycleContext<'a> {
    pub ws: &'a Workspace,
    pub secrets: &'a SecretStore,
    pub desktop: &'a dyn Desktop,
    pub engine: &'a HttpEngine,
    /// `[monitors].retention`.
    pub retention: Duration,
    /// [`crate::notify::TIMEOUT`] outside tests.
    pub notify_timeout: Duration,
}

/// A finished cycle.
#[derive(Debug)]
pub struct Cycle {
    pub run: MonitorRun,
    /// Store warnings, such as `.rustman/ is not git-ignored`.
    pub warnings: Vec<String>,
    /// Each alert sent, with how every channel did.
    pub alerts: Vec<(Alert, Vec<Delivery>)>,
}

impl CycleContext<'_> {
    /// Run `monitor` once and store, evaluate and notify. `cancel` stops
    /// the run early.
    pub async fn run(
        &self,
        monitor: &Monitor,
        cancel: CancellationToken,
    ) -> Result<Cycle, MonitorError> {
        let started = Timestamp::now();
        let run = run(self.ws, monitor, self.secrets, started, cancel).await?;
        let warnings = crate::store_result(
            &self.ws.root,
            &monitor.name,
            &run.result,
            Timestamp::now(),
            self.retention,
        )?;
        let alerts = if run.interrupted {
            Vec::new()
        } else {
            self.alert(monitor, &run.result).await?
        };
        Ok(Cycle {
            run,
            warnings,
            alerts,
        })
    }

    /// Store a run of `monitor` due at `at` as skipped, because the
    /// previous one is still going. Skipped runs change no alert.
    pub fn skip(&self, monitor: &Monitor, at: Timestamp) -> Result<Vec<String>, MonitorError> {
        let result = RunResult::skipped(at);
        let warnings =
            crate::store_result(&self.ws.root, &monitor.name, &result, at, self.retention)?;
        Ok(warnings)
    }

    /// Evaluate the rules after `latest` and send what they say.
    async fn alert(
        &self,
        monitor: &Monitor,
        latest: &RunResult,
    ) -> Result<Vec<(Alert, Vec<Delivery>)>, MonitorError> {
        let store = Store::new(&self.ws.root);
        let notices = evaluate::after_run(&store, &monitor.name, &monitor.alerts, latest.at)?;
        if notices.is_empty() {
            return Ok(Vec::new());
        }
        let notifier = Notifier {
            root: &self.ws.root,
            settings: &self.ws.settings,
            environment: environment(self.ws, &monitor.environment)?,
            secrets: self.secrets,
            desktop: self.desktop,
            engine: self.engine,
            timeout: self.notify_timeout,
        };
        let mut sent = Vec::new();
        for notice in &notices {
            let alert = Alert::from_notice(monitor, notice, latest);
            let deliveries = notifier.send(&monitor.alerts.notify, &alert).await;
            sent.push((alert, deliveries));
        }
        Ok(sent)
    }

    /// Send the `[TEST] <name>` notification through every channel of
    /// `monitor` (`rustman monitor test-alert`).
    pub async fn test_alert(&self, monitor: &Monitor) -> Result<Vec<Delivery>, MonitorError> {
        let notifier = Notifier {
            root: &self.ws.root,
            settings: &self.ws.settings,
            environment: environment(self.ws, &monitor.environment)?,
            secrets: self.secrets,
            desktop: self.desktop,
            engine: self.engine,
            timeout: self.notify_timeout,
        };
        let alert = Alert::test(monitor, Timestamp::now());
        Ok(notifier.send(&monitor.alerts.notify, &alert).await)
    }
}
