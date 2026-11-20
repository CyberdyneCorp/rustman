//! Closed-model executors (design D4): `constant-vus` and `ramping-vus`.
//! Each VU loops over iterations, pausing for the think time between them,
//! until the profile ends or the iterations run out. Under `ramping-vus` a
//! controller moves the target VU count every 100 ms; VU `n` runs while the
//! target is above `n`, and a VU the target drops finishes its iteration
//! (graceful stop) or has it cancelled once the grace period is over.

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use futures_util::future::join_all;
use tokio::sync::watch;
use tokio::time::{MissedTickBehavior, interval, sleep};

use crate::profile::{Stage, ThinkTime, ramp_target};
use crate::vu::{Shared, Vu};

/// How often the ramping controller moves the target.
const RAMP_TICK: Duration = Duration::from_millis(100);

/// What the VUs of a closed-model run share.
pub(crate) struct Closed<'a> {
    pub shared: Shared<'a>,
    pub think: ThinkTime,
    /// Iterations left to start, under an iterations limit.
    pub remaining: Option<AtomicU64>,
    /// How long a VU removed by a ramp-down may finish its iteration.
    pub grace: Duration,
}

impl Closed<'_> {
    /// Run every VU until the profile ends. `target` (ramping) is the VU
    /// count the controller asks for; without it every VU runs.
    pub async fn run(&self, vus: &[Vu<'_>], target: Option<&watch::Receiver<u32>>) {
        join_all(vus.iter().map(|vu| self.vu(vu, target.cloned()))).await;
    }

    async fn vu(&self, vu: &Vu<'_>, mut target: Option<watch::Receiver<u32>>) {
        let metrics = self.shared.metrics;
        while self.wait_until_active(vu.index, target.as_mut()).await {
            metrics.vu_started();
            let removed = self.active(vu, target.as_ref()).await;
            metrics.vu_stopped();
            if !removed {
                return;
            }
        }
    }

    /// Run iterations until the VU is removed (`true`) or the run is over
    /// for it (`false`). The next iteration is claimed before the think
    /// time, so a VU never thinks once the iterations are used up.
    async fn active(&self, vu: &Vu<'_>, target: Option<&watch::Receiver<u32>>) -> bool {
        let mut first = true;
        loop {
            if self.shared.stop.is_cancelled() || !self.claim() {
                return false;
            }
            if !first {
                self.think().await;
                if self.shared.stop.is_cancelled() {
                    return false;
                }
            }
            first = false;
            let Some(row) = vu.next_row(&self.shared) else {
                return false;
            };
            tokio::select! {
                () = vu.iteration(&self.shared, row, None) => {}
                () = removed_for(vu.index, target.cloned(), self.grace) => {
                    self.shared.metrics.iteration_interrupted();
                }
            }
            if is_removed(vu.index, target) {
                return true;
            }
        }
    }

    /// Take one iteration from the iterations limit, if there is one.
    fn claim(&self) -> bool {
        self.remaining.as_ref().is_none_or(|left| {
            left.fetch_update(Ordering::SeqCst, Ordering::SeqCst, |n| n.checked_sub(1))
                .is_ok()
        })
    }

    async fn think(&self) {
        let pause = self.think.pause();
        if pause.is_zero() {
            return;
        }
        tokio::select! {
            () = sleep(pause) => {}
            () = self.shared.stop.cancelled() => {}
        }
    }

    /// Wait until VU `index` may run: at once without a target, else once
    /// the target is above it. `false` when the run ends first.
    async fn wait_until_active(
        &self,
        index: usize,
        target: Option<&mut watch::Receiver<u32>>,
    ) -> bool {
        let stop = self.shared.stop;
        let Some(target) = target else {
            return !stop.is_cancelled();
        };
        loop {
            if stop.is_cancelled() {
                return false;
            }
            if above(*target.borrow_and_update(), index) {
                return true;
            }
            tokio::select! {
                changed = target.changed() => {
                    if changed.is_err() {
                        stop.cancelled().await;
                    }
                }
                () = stop.cancelled() => return false,
            }
        }
    }
}

fn above(target: u32, index: usize) -> bool {
    usize::try_from(target).unwrap_or(usize::MAX) > index
}

fn is_removed(index: usize, target: Option<&watch::Receiver<u32>>) -> bool {
    target.is_some_and(|t| !above(*t.borrow(), index))
}

/// Resolves `grace` after VU `index` is removed by the target; never
/// without a target.
async fn removed_for(index: usize, target: Option<watch::Receiver<u32>>, grace: Duration) {
    let Some(mut target) = target else {
        return std::future::pending().await;
    };
    while above(*target.borrow_and_update(), index) {
        if target.changed().await.is_err() {
            return std::future::pending().await;
        }
    }
    sleep(grace).await;
}

/// Move the target VU count along `stages` every 100 ms until they end.
pub(crate) async fn ramp(stages: &[Stage], target: &watch::Sender<u32>) {
    let start = Instant::now();
    let total: Duration = stages.iter().map(|s| s.duration).sum();
    let mut tick = interval(RAMP_TICK);
    tick.set_missed_tick_behavior(MissedTickBehavior::Delay);
    loop {
        tick.tick().await;
        let elapsed = start.elapsed();
        target.send_replace(ramp_target(stages, elapsed));
        if elapsed >= total {
            return;
        }
    }
}
