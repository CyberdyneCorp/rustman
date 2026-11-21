//! Every probe request of one provider goes through a [`Meter`]: the
//! read-only allowlist, the request budget (chain-exploration "Probes pass
//! the safety policy"), the safety policy's pacing for WebSocket endpoints
//! (HTTP ones are paced by their governed sender), and latency per method:
//! the time on the wire for HTTP calls, so the governor's pacing wait is
//! never counted, and the time from the paced send for WebSocket ones.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Mutex, PoisonError};
use std::time::{Duration, Instant};

use serde_json::Value;

use super::catalog::is_allowed;
use super::provider::Provider;
use crate::blocklist::SEND_REFUSED;
use crate::rpc::{Rpc, RpcFailure};

/// Why a probe call returned no result.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ProbeFailure {
    /// The budget ran out; nothing was sent.
    Budget,
    /// The node, the transport or a guard said no.
    Rpc(RpcFailure),
}

/// The longest failure detail a report keeps, in bytes: a provider's error
/// messages are its own text, of any size.
pub const MAX_DETAIL_BYTES: usize = 2048;

impl ProbeFailure {
    /// The failure as shown in reports (already redacted by the
    /// transport), cut to [`MAX_DETAIL_BYTES`].
    pub fn detail(&self) -> String {
        let detail = match self {
            Self::Budget => return "skipped (budget)".to_owned(),
            Self::Rpc(RpcFailure::Error { code, message, .. }) => {
                format!("rpc error {code}: {message}")
            }
            Self::Rpc(RpcFailure::Transport(message) | RpcFailure::Refused(message)) => {
                message.clone()
            }
        };
        truncate(detail, MAX_DETAIL_BYTES)
    }
}

/// `text` cut to `max` bytes on a character boundary, saying how long it
/// was.
pub(crate) fn truncate(mut text: String, max: usize) -> String {
    if text.len() <= max {
        return text;
    }
    let total = text.len();
    let mut end = max;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    text.truncate(end);
    text.push_str(&format!("\u{2026} ({total} bytes)"));
    text
}

/// The answer of one probe call.
pub type Answer = Result<Value, ProbeFailure>;

/// Counts, gates and times one provider's requests.
pub struct Meter<'p> {
    pub provider: &'p Provider,
    budget: u64,
    sent: AtomicU64,
    exhausted: AtomicBool,
    latency: Mutex<BTreeMap<String, Vec<Duration>>>,
}

impl<'p> Meter<'p> {
    pub fn new(provider: &'p Provider, budget: u64) -> Self {
        Self {
            provider,
            budget,
            sent: AtomicU64::new(0),
            exhausted: AtomicBool::new(false),
            latency: Mutex::new(BTreeMap::new()),
        }
    }

    /// Call `method` on the provider's main transport.
    pub async fn call(&self, method: &str, params: Value) -> Answer {
        self.call_on(self.provider.rpc.as_ref(), method, params)
            .await
    }

    /// Call `method` on `rpc` (the provider's main or rate transport).
    pub async fn call_on(&self, rpc: &dyn Rpc, method: &str, params: Value) -> Answer {
        if !is_allowed(method) {
            let refusal = format!("{method}: {SEND_REFUSED}; probes send read-only methods only");
            return Err(ProbeFailure::Rpc(RpcFailure::Refused(refusal)));
        }
        if !self.reserve(1) {
            return Err(ProbeFailure::Budget);
        }
        let _slot = match self.provider.gate_ws().await {
            Ok(slot) => slot,
            Err(refusal) => return Err(ProbeFailure::Rpc(RpcFailure::Refused(refusal))),
        };
        let started = Instant::now();
        let (answer, wire) = rpc.call_timed(method, params).await;
        if !matches!(answer, Err(RpcFailure::Refused(_))) {
            self.time(method, wire.unwrap_or_else(|| started.elapsed()));
        }
        answer.map_err(ProbeFailure::Rpc)
    }

    /// Take `n` requests from the budget; `false` (and the budget marked
    /// exhausted) when fewer are left.
    pub fn reserve(&self, n: u64) -> bool {
        let taken = self
            .sent
            .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |sent| {
                (sent + n <= self.budget).then_some(sent + n)
            });
        if taken.is_err() {
            self.exhausted.store(true, Ordering::SeqCst);
        }
        taken.is_ok()
    }

    pub fn sent(&self) -> u64 {
        self.sent.load(Ordering::SeqCst)
    }

    pub fn budget(&self) -> u64 {
        self.budget
    }

    /// A request was refused for lack of budget.
    pub fn exhausted(&self) -> bool {
        self.exhausted.load(Ordering::SeqCst)
    }

    fn time(&self, method: &str, took: Duration) {
        self.latency
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .entry(method.to_owned())
            .or_default()
            .push(took);
    }

    /// Latency samples per method, in method order.
    pub fn latencies(&self) -> BTreeMap<String, Vec<Duration>> {
        self.latency
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }
}

/// The `p`th percentile (nearest rank) of `samples`, in milliseconds.
pub fn percentile(samples: &[Duration], p: u32) -> f64 {
    let mut sorted: Vec<Duration> = samples.to_vec();
    sorted.sort_unstable();
    if sorted.is_empty() {
        return 0.0;
    }
    let rank = (sorted.len() * p as usize).div_ceil(100).max(1);
    millis(sorted[rank.min(sorted.len()) - 1])
}

/// `took` in milliseconds, to a tenth.
pub fn millis(took: Duration) -> f64 {
    (took.as_secs_f64() * 10_000.0).round() / 10.0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn percentiles_use_the_nearest_rank() {
        let ms = |v: &[u64]| {
            v.iter()
                .map(|m| Duration::from_millis(*m))
                .collect::<Vec<_>>()
        };
        let samples = ms(&[5, 1, 4, 2, 3, 6, 7, 8, 9, 100]);
        assert_eq!(percentile(&samples, 50), 5.0);
        assert_eq!(percentile(&samples, 95), 100.0);
        assert_eq!(percentile(&samples, 100), 100.0);
        assert_eq!(percentile(&ms(&[7]), 50), 7.0);
        assert_eq!(percentile(&[], 50), 0.0);
        assert_eq!(millis(Duration::from_micros(1234)), 1.2);
    }

    #[test]
    fn failures_show_their_detail() {
        let error = RpcFailure::Error {
            code: -32601,
            message: "no such method".into(),
            data: None,
        };
        assert_eq!(
            ProbeFailure::Rpc(error).detail(),
            "rpc error -32601: no such method"
        );
        assert_eq!(ProbeFailure::Budget.detail(), "skipped (budget)");
        assert_eq!(
            ProbeFailure::Rpc(RpcFailure::Transport("HTTP 429".into())).detail(),
            "HTTP 429"
        );
    }

    #[test]
    fn huge_provider_errors_are_cut() {
        let message = format!("\u{e9}{}", "x".repeat(3 * MAX_DETAIL_BYTES));
        let failure = ProbeFailure::Rpc(RpcFailure::Error {
            code: -32000,
            message,
            data: None,
        });
        let detail = failure.detail();
        assert!(detail.len() < MAX_DETAIL_BYTES + 32, "{}", detail.len());
        assert!(detail.starts_with("rpc error -32000: \u{e9}xx"));
        assert!(detail.ends_with("\u{2026} (6164 bytes)"), "{detail}");
        let odd = truncate(
            format!("{}\u{e9}tail", "y".repeat(MAX_DETAIL_BYTES - 1)),
            MAX_DETAIL_BYTES,
        );
        assert!(odd.starts_with(&"y".repeat(MAX_DETAIL_BYTES - 1)));
        assert!(!odd.contains('\u{e9}'));
    }
}
