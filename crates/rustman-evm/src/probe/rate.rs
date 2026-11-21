//! Rate-limit discovery (chain-exploration "Rate-limit discovery"), only
//! with `--rate-discovery`: `eth_blockNumber` at 5, 10, 20 and 40 req/s
//! (never above `--max-rate`), one window per step, stopping at the first
//! HTTP 429 or JSON-RPC rate-limit error and then waiting out its
//! `Retry-After`.

use std::time::{Duration, Instant};

use rustman_report::exploration::EvmRateLimit;
use serde_json::json;

use super::checks::{ProviderRun, pause};
use super::meter::ProbeFailure;
use super::support::is_rate_limited;
use super::{CheckOutcome, ProbeOptions};

/// The rates tried, in order.
pub const STEPS: [u32; 4] = [5, 10, 20, 40];

/// The longest `Retry-After` waited for.
pub const MAX_RETRY_AFTER: Duration = Duration::from_secs(60);

/// The steps under `max_rate`: the standard ones up to it, or `max_rate`
/// alone when it is below the first.
pub fn steps(max_rate: u32) -> Vec<u32> {
    let steps: Vec<u32> = STEPS.into_iter().filter(|s| *s <= max_rate).collect();
    if steps.is_empty() {
        vec![max_rate.max(1)]
    } else {
        steps
    }
}

/// How a step ended.
enum Step {
    Passed,
    Limited(Option<Duration>),
    /// The budget ran out or the probe was cancelled.
    Stopped,
}

/// Discover the provider's rate limit.
pub async fn discover(run: &mut ProviderRun<'_>, options: &ProbeOptions) {
    let Some(rate) = &run.meter.provider.rate else {
        return;
    };
    let mut found = EvmRateLimit::default();
    let mut stopped = false;
    for step in steps(options.max_rate) {
        found.steps.push(step);
        match one_step(run, rate, step, options).await {
            Step::Passed => found.limit_rps = Some(step),
            Step::Limited(after) => {
                found.limited_at = Some(step);
                if let Some(after) = after.map(|a| a.min(MAX_RETRY_AFTER)) {
                    found.retry_after_ms = u64::try_from(after.as_millis()).ok();
                    pause(after, &options.cancel).await;
                }
                break;
            }
            Step::Stopped => {
                stopped = true;
                break;
            }
        }
    }
    let outcome = match stopped && run.meter.exhausted() {
        true => CheckOutcome::SkippedBudget,
        false => CheckOutcome::Passed,
    };
    run.check("rate-discovery", outcome, Some(found.summary()));
    run.rate_limit = Some(found);
}

/// Send one window of requests at `rate` req/s, each at least `1/rate`
/// after the previous one started.
async fn one_step(
    run: &ProviderRun<'_>,
    transport: &super::provider::RateTransport,
    rate: u32,
    options: &ProbeOptions,
) -> Step {
    let interval = Duration::from_secs(1) / rate;
    let window = options.timing.rate_window.as_secs_f64();
    let count = (f64::from(rate) * window).ceil().max(1.0) as u32;
    for _ in 0..count {
        let started = Instant::now();
        let answer = run
            .meter
            .call_on(&transport.rpc, "eth_blockNumber", json!([]))
            .await;
        match answer {
            Err(ProbeFailure::Rpc(failure)) if is_rate_limited(&failure) => {
                return Step::Limited(transport.watch.take_retry_after());
            }
            Err(ProbeFailure::Budget) => return Step::Stopped,
            _ => {}
        }
        let wait = interval.saturating_sub(started.elapsed());
        if !pause(wait, &options.cancel).await {
            return Step::Stopped;
        }
    }
    Step::Passed
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn steps_stop_at_the_max_rate() {
        assert_eq!(steps(40), [5, 10, 20, 40]);
        assert_eq!(steps(25), [5, 10, 20]);
        assert_eq!(steps(100), [5, 10, 20, 40]);
        assert_eq!(steps(3), [3]);
    }

    #[test]
    fn summaries_name_the_limit() {
        let limit = |limit_rps, limited_at| EvmRateLimit {
            limit_rps,
            limited_at,
            steps: vec![5, 10, 20, 40],
            retry_after_ms: None,
        };
        assert_eq!(
            limit(Some(20), Some(40)).summary(),
            "rate limit \u{2248} 20 req/s"
        );
        assert_eq!(limit(None, Some(5)).summary(), "rate limit < 5 req/s");
        assert_eq!(
            limit(Some(40), None).summary(),
            "no rate limit up to 40 req/s"
        );
    }
}
