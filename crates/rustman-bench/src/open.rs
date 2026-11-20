//! The open-model executor (design D4): `constant-arrival-rate`. A ticker
//! schedules `rate` iterations per second whatever the response times, and
//! hands each scheduled instant to a free VU of the pool. With no VU free,
//! the iteration is `dropped`. Latency counts from the scheduled instant
//! (coordinated-omission correction).

use std::time::{Duration, Instant};

use futures_util::StreamExt;
use futures_util::stream::FuturesUnordered;
use tokio::time::{MissedTickBehavior, interval_at};

use crate::vu::{Shared, Vu};

pub(crate) struct Open<'a> {
    pub shared: Shared<'a>,
    pub rate: u32,
    pub duration: Duration,
}

impl Open<'_> {
    /// Schedule iterations until the duration ends (or the run stops), then
    /// wait for those in flight.
    pub async fn run(&self, vus: &[Vu<'_>]) {
        let mut free: Vec<usize> = (0..vus.len()).rev().collect();
        let mut running = FuturesUnordered::new();
        let period = Duration::from_secs_f64(1.0 / f64::from(self.rate.max(1)));
        let start = tokio::time::Instant::now();
        let end = start + self.duration;
        let mut ticker = interval_at(start, period);
        ticker.set_missed_tick_behavior(MissedTickBehavior::Burst);
        let stop = self.shared.stop;
        loop {
            tokio::select! {
                biased;
                () = stop.cancelled() => break,
                Some(index) = running.next(), if !running.is_empty() => free.push(index),
                at = ticker.tick() => {
                    if at >= end {
                        break;
                    }
                    let Some(vu) = self.take(&mut free, vus) else {
                        continue;
                    };
                    running.push(self.iteration(vu, at.into_std()));
                }
            }
        }
        while running.next().await.is_some() {}
    }

    /// A free VU for an iteration scheduled now, or `None` (counted as
    /// dropped).
    fn take<'v, 'r>(&self, free: &mut Vec<usize>, vus: &'v [Vu<'r>]) -> Option<&'v Vu<'r>> {
        let metrics = self.shared.metrics;
        metrics.scheduled();
        let Some(index) = free.pop() else {
            metrics.dropped();
            return None;
        };
        Some(&vus[index])
    }

    /// Run one iteration on `vu`; out of data rows (under `stop`), stop
    /// the run instead. Returns the VU, free again. A VU counts as active
    /// while it runs an iteration (a busy VU of the pool).
    async fn iteration(&self, vu: &Vu<'_>, scheduled: Instant) -> usize {
        let metrics = self.shared.metrics;
        match vu.next_row(&self.shared) {
            Some(row) => {
                metrics.vu_started();
                vu.iteration(&self.shared, row, Some(scheduled)).await;
                metrics.vu_stopped();
            }
            None => self.shared.stop.cancel(),
        }
        vu.index
    }
}
