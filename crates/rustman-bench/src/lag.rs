//! Generator lag (design "Risks": the load generator itself is the
//! bottleneck). A probe on the bench's own runtime yields to the scheduler
//! every [`PROBE_EVERY`] and measures how long the runtime takes to poll
//! it again: when the runtime's threads are saturated (or blocked), that
//! delay grows, and so does every latency the bench measures. Above
//! [`LAG_LIMIT`] at p99, the results may be generator-bound.
//!
//! Yielding rather than sleeping keeps the measure independent of the
//! operating system's timer resolution (about 15 ms on Windows).

use std::sync::{Mutex, PoisonError};
use std::time::{Duration, Instant};

use hdrhistogram::Histogram;

use crate::metrics::{MAX_LATENCY_US, SIGNIFICANT_DIGITS};

/// How often the probe measures.
pub const PROBE_EVERY: Duration = Duration::from_millis(20);
/// The p99 lag above which results may be generator-bound.
pub const LAG_LIMIT: Duration = Duration::from_millis(10);

/// Measures the scheduling lag of the runtime it runs on.
#[derive(Debug)]
pub struct LagProbe {
    lag: Mutex<Histogram<u64>>,
}

impl Default for LagProbe {
    fn default() -> Self {
        Self::new()
    }
}

impl LagProbe {
    pub fn new() -> Self {
        Self {
            lag: Mutex::new(
                Histogram::new_with_bounds(1, MAX_LATENCY_US, SIGNIFICANT_DIGITS)
                    .expect("valid histogram bounds"),
            ),
        }
    }

    /// Measure until dropped; never resolves.
    pub async fn run(&self) {
        loop {
            tokio::time::sleep(PROBE_EVERY).await;
            let yielded = Instant::now();
            tokio::task::yield_now().await;
            self.record(yielded.elapsed());
        }
    }

    fn record(&self, lag: Duration) {
        let micros = u64::try_from(lag.as_micros()).unwrap_or(u64::MAX);
        self.lag
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .saturating_record(micros.clamp(1, MAX_LATENCY_US));
    }

    /// The 99th percentile of the lag so far (zero before any measure).
    pub fn p99(&self) -> Duration {
        let lag = self.lag.lock().unwrap_or_else(PoisonError::into_inner);
        if lag.is_empty() {
            return Duration::ZERO;
        }
        Duration::from_micros(lag.value_at_quantile(0.99))
    }

    /// The generator-bound warning, when the p99 lag is above
    /// [`LAG_LIMIT`].
    pub fn warning(&self) -> Option<String> {
        let p99 = self.p99();
        (p99 > LAG_LIMIT).then(|| {
            format!(
                "the load generator lagged {:.1} ms at p99 (over 10 ms): results may be generator-bound",
                p99.as_secs_f64() * 1000.0
            )
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_lag_no_warning() {
        let probe = LagProbe::new();
        assert_eq!(probe.p99(), Duration::ZERO);
        probe.record(Duration::from_micros(300));
        assert_eq!(probe.warning(), None);
    }

    /// Task 4.5: a runtime whose only thread is blocked again and again
    /// (an artificially throttled generator) triggers the warning.
    #[test]
    fn a_throttled_runtime_is_generator_bound() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .unwrap();
        let probe = LagProbe::new();
        runtime.block_on(async {
            let hog = async {
                for _ in 0..15 {
                    // Blocks the runtime's only thread, as CPU-bound work would.
                    std::thread::sleep(Duration::from_millis(30));
                    tokio::task::yield_now().await;
                }
            };
            tokio::select! {
                () = hog => {}
                () = probe.run() => {}
            }
        });
        assert!(
            probe.p99() >= Duration::from_millis(20),
            "{:?}",
            probe.p99()
        );
        let warning = probe.warning().unwrap();
        assert!(
            warning.ends_with("results may be generator-bound"),
            "{warning}"
        );
    }
}
