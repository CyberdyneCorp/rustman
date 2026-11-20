//! A virtual user: a [`Runner`] of its own (its own variable and capture
//! scope, cookie jar and connection pool), running the plan's requests in
//! collection order, once per iteration, through the same executor as
//! `rustman run` (design D1).

use std::time::{Duration, Instant};

use rustman_core::run::{Plan, RequestRecord, RequestStatus};
use rustman_http::runner::Runner;
use tokio_util::sync::CancellationToken;

use crate::data::Feeder;
use crate::metrics::{Metrics, Sample, describe};

/// What every VU of a bench shares.
pub(crate) struct Shared<'a> {
    pub plan: &'a Plan<'a>,
    pub metrics: &'a Metrics,
    pub feeder: Option<&'a Feeder>,
    /// Cancelled when no new iteration may start (end of the profile,
    /// Ctrl+C); iterations in flight continue.
    pub stop: &'a CancellationToken,
}

/// How an iteration ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Ended {
    Done,
    /// Cancelled before its last request.
    Interrupted,
}

pub(crate) struct Vu<'a> {
    /// 0-based; VU 1 in messages is index 0.
    pub index: usize,
    pub runner: Runner<'a>,
}

impl Vu<'_> {
    /// The data row of this VU's next iteration: 1 without data (the row
    /// is then only a label), `None` when its rows ran out.
    pub fn next_row(&self, shared: &Shared<'_>) -> Option<u32> {
        match shared.feeder {
            Some(feeder) => feeder.next(self.index),
            None => Some(1),
        }
    }

    /// Run every request of the plan in order with data row `row`, and
    /// record each in the metrics. With `scheduled` (the open model),
    /// latency counts from that instant for the first request and from the
    /// end of the previous one after it.
    pub async fn iteration(&self, shared: &Shared<'_>, row: u32, scheduled: Option<Instant>) {
        let started = Instant::now();
        // The environment's secrets are known once and for all; those the
        // last iteration resolved or captured are dropped (bounded memory).
        self.runner.seed(&shared.plan.requests);
        self.runner.forget_iteration_secrets();
        match self.requests(shared, row, scheduled).await {
            Ended::Done => shared.metrics.iteration_done(started.elapsed()),
            Ended::Interrupted => shared.metrics.iteration_interrupted(),
        }
    }

    async fn requests(&self, shared: &Shared<'_>, row: u32, scheduled: Option<Instant>) -> Ended {
        let mut mark = scheduled;
        for (file, request) in shared.plan.requests.iter().enumerate() {
            let started = Instant::now();
            let sent = self.runner.send(request, row).await;
            let ended = Instant::now();
            if sent.record.status == RequestStatus::Skipped {
                return Ended::Interrupted;
            }
            let service = service_time(&sent.record, ended - started);
            let latency = corrected(service, ended, mark);
            let sample = Sample::of(file, latency, &sent.record);
            shared.metrics.record(&sample);
            if let Some(class) = sample.error {
                // The record is redacted; the exchange's redactor also
                // knows the secrets this very request resolved.
                let message = sent.redactor.text(&describe(&sent.record)).into_owned();
                shared.metrics.sample_error(class, file, message);
            }
            mark = mark.map(|_| ended);
        }
        Ended::Done
    }
}

/// The transport time of an exchange, else (no response) the time the
/// runner took.
pub(crate) fn service_time(record: &RequestRecord, elapsed: Duration) -> Duration {
    record
        .exchange
        .as_ref()
        .map(|e| e.timings.total_ms)
        .filter(|ms| ms.is_finite() && *ms >= 0.0)
        .map_or(elapsed, |ms| Duration::from_secs_f64(ms / 1000.0))
}

/// With `mark` (the scheduled start), the whole time from it to `ended`:
/// the queuing before the request started, any wait for the `max_rps`
/// limit inside the runner, and `service`. Without one, `service`.
pub(crate) fn corrected(service: Duration, ended: Instant, mark: Option<Instant>) -> Duration {
    match mark {
        Some(mark) => service.max(ended.saturating_duration_since(mark)),
        None => service,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn corrected_latency_counts_from_the_scheduled_start() {
        let scheduled = Instant::now();
        // Queued 2 s, then 500 ms waiting for the rate limit, then 10 ms
        // on the wire: everything since the schedule counts.
        let ended = scheduled + Duration::from_millis(2510);
        let service = Duration::from_millis(10);
        assert_eq!(
            corrected(service, ended, Some(scheduled)),
            Duration::from_millis(2510)
        );
        assert_eq!(corrected(service, ended, None), service);
        // Never less than the exchange itself (clock granularity).
        assert_eq!(corrected(service, scheduled, Some(scheduled)), service);
    }
}
