//! What runs around the load with single requests: the health request's
//! baseline before load, the recovery check once load stopped (after
//! every mode, and after a kill), then the minimization of crash-like
//! findings once the target has recovered.

use std::sync::{Mutex, PoisonError};
use std::time::Duration;

use bytes::Bytes;
use rustman_core::Redactor;
use rustman_http::BodyOverride;
use rustman_http::runner::Sent;
use serde_json::Value;
use tokio_util::sync::CancellationToken;

use crate::metrics::ErrorClass;
use crate::stress::fuzz::findings::{CrashInput, SERVER_ERROR, TIMEOUT};
use crate::stress::minimize::{MAX_REPLAYS, reduce};
use crate::stress::probe::Prober;
use crate::stress::recovery::{self, Probe, RecoveryResult, RecoveryTuning};
use crate::stress::{Minimization, StressFinding};

/// The largest input minimization reduces; bigger ones are replayed once.
pub const MAX_REDUCED: usize = 64 * 1024;

/// How the health request answered before load.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Before {
    /// No GET in the plan and no `--health`.
    NoHealth,
    /// It did not answer 2xx or 3xx: recovery cannot be judged.
    Unhealthy(Probe),
    /// Its median latency.
    Baseline(Duration),
}

impl Before {
    /// The health request is fit for the kill switch and recovery.
    pub fn healthy(&self) -> bool {
        matches!(self, Self::Baseline(_))
    }
}

/// Send the health request `tuning.baseline_probes` times, back to back.
pub(crate) async fn before_load(prober: &Prober<'_>, tuning: &RecoveryTuning) -> Before {
    let mut probes = Vec::new();
    for _ in 0..tuning.baseline_probes.max(1) {
        match prober.health().await {
            Some(probe) => probes.push(probe),
            None => return Before::NoHealth,
        }
    }
    match recovery::baseline(&probes) {
        Some(latency) => Before::Baseline(latency),
        None => Before::Unhealthy(probes.pop().expect("one probe at least")),
    }
}

/// The recovery check: skipped without a fit health request.
pub(crate) async fn recover(
    prober: &Prober<'_>,
    before: &Before,
    timeout: Duration,
    tuning: &RecoveryTuning,
    cancel: &CancellationToken,
) -> RecoveryResult {
    let health = prober.health_file();
    match before {
        Before::NoHealth => RecoveryResult::skipped(
            None,
            "no health request: the plan has no GET request (use --health <file>)",
        ),
        Before::Unhealthy(probe) => RecoveryResult::skipped(
            health,
            &format!(
                "the health request already failed before load ({})",
                probe.describe()
            ),
        ),
        Before::Baseline(latency) => {
            let mut result =
                recovery::monitor(prober, Some(*latency), timeout, tuning, cancel).await;
            result.health = health;
            result
        }
    }
}

/// Replay the input of each crash-like finding alone, reducing it, and
/// attach what was found to the finding with the same rule and family.
pub(crate) async fn minimize_all(
    prober: &Prober<'_>,
    files: &[String],
    inputs: Vec<CrashInput>,
    findings: &mut [StressFinding],
    tuning: &RecoveryTuning,
    cancel: &CancellationToken,
) {
    for input in inputs {
        if cancel.is_cancelled() {
            return;
        }
        let Some(file) = files.iter().position(|f| *f == input.file) else {
            continue;
        };
        let target = findings
            .iter_mut()
            .find(|f| f.rule == input.rule && f.family.as_deref() == Some(input.family.as_str()));
        if let Some(finding) = target {
            let replay = Replay {
                prober,
                file,
                rule: input.rule,
                interval: tuning.replay_interval,
                cancel,
                redactor: Mutex::new(None),
                sent: Mutex::new(0),
            };
            let minimization = replay.minimize(&input.body).await;
            // A minimization cut short says nothing about the input.
            if cancel.is_cancelled() {
                return;
            }
            finding.minimization = Some(minimization);
        }
    }
}

/// The replays of one finding's input.
struct Replay<'p, 'a> {
    prober: &'p Prober<'a>,
    file: usize,
    rule: &'static str,
    interval: Duration,
    /// Stops the replays (Ctrl+C): no replay is sent once it is cancelled.
    cancel: &'p CancellationToken,
    /// The last replay's redactor, for the minimized body.
    redactor: Mutex<Option<Redactor>>,
    sent: Mutex<u32>,
}

impl Replay<'_, '_> {
    async fn minimize(&self, body: &BodyOverride) -> Minimization {
        let json = (body.bytes.len() <= MAX_REDUCED && body.content_encoding.is_none())
            .then(|| serde_json::from_slice::<Value>(&body.bytes).ok())
            .flatten();
        let Some(original) = json else {
            let reproduced = self.reproduces(body.clone()).await;
            return Minimization {
                body: None,
                content_type: body.content_type.clone(),
                replays: 1,
                load_dependent: !reproduced,
            };
        };
        let content_type = body.content_type.clone();
        let reduced = reduce(&original, MAX_REPLAYS, |value| {
            let body = BodyOverride {
                bytes: Bytes::from(value.to_string()),
                content_type: content_type.clone(),
                content_encoding: None,
            };
            self.reproduces(body)
        })
        .await;
        let text = reduced.value.to_string();
        let redactor = self
            .redactor
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
            .unwrap_or_default();
        Minimization {
            body: Some(redactor.text(&text).into_owned()),
            content_type,
            replays: reduced.replays,
            load_dependent: reduced.load_dependent,
        }
    }

    /// Replay `body`, paced, and tell whether the finding's symptom came
    /// back.
    async fn reproduces(&self, body: BodyOverride) -> bool {
        let first = {
            let mut sent = self.sent.lock().unwrap_or_else(PoisonError::into_inner);
            *sent += 1;
            *sent == 1
        };
        if !first {
            tokio::select! {
                () = tokio::time::sleep(self.interval) => {}
                () = self.cancel.cancelled() => {}
            }
        }
        if self.cancel.is_cancelled() {
            return false;
        }
        let sent = tokio::select! {
            sent = self.prober.replay(self.file, body) => sent,
            () = self.cancel.cancelled() => None,
        };
        let Some(sent) = sent else {
            return false;
        };
        *self.redactor.lock().unwrap_or_else(PoisonError::into_inner) = Some(sent.redactor.clone());
        symptom(self.rule, &sent)
    }
}

/// The finding's symptom: a 5xx for a server error, a timeout for a
/// timeout.
fn symptom(rule: &str, sent: &Sent) -> bool {
    match rule {
        SERVER_ERROR => sent
            .record
            .exchange
            .as_ref()
            .is_some_and(|e| e.response.status >= 500),
        TIMEOUT => ErrorClass::of(&sent.record) == Some(ErrorClass::Timeout),
        _ => false,
    }
}
