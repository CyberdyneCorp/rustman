//! Latency and fault injection (spec: Latency simulation, Fault
//! injection; design D4). Randomness comes from a per-route stream seeded
//! from the server seed and the route and advanced per request, so a given
//! seed and request sequence reproduces the same delays and faults.

use std::collections::HashMap;
use std::convert::Infallible;
use std::pin::Pin;
use std::sync::Mutex;
use std::task::{Context, Poll};
use std::time::Duration;

use bytes::Bytes;
use http::StatusCode;
use http_body::{Body, Frame, SizeHint};
use serde::Deserialize;
use tokio::time::{Instant, Sleep};

use crate::respond::mix;

/// The longest `delay_ms` and `jitter_ms`, as for webhook mocks.
pub const MAX_DELAY_MS: u64 = rustman_webhook::mocks::MAX_DELAY_MS;

/// How often a slow body sends a chunk.
const SLOW_TICK: Duration = Duration::from_millis(100);

/// A deterministic stream of picks (SplitMix64).
#[derive(Clone, Debug)]
pub struct Picks(u64);

impl Picks {
    pub fn new(seed: u64) -> Self {
        Self(seed)
    }

    pub fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    /// A number in `[0, 1)`.
    pub fn unit(&mut self) -> f64 {
        // 53 random bits: exact in an f64.
        (self.next_u64() >> 11) as f64 / (1u64 << 53) as f64
    }
}

/// Per-route request counters: the n-th request to a route gets the picks
/// of `(seed, route, n)`, whatever other routes receive meanwhile.
#[derive(Debug, Default)]
pub struct Sequences {
    counters: Mutex<HashMap<String, u64>>,
}

impl Sequences {
    /// The picks of the next request to `route`.
    pub fn next(&self, seed: u64, route: &str) -> Picks {
        let n = {
            let mut counters = self.counters.lock().unwrap_or_else(|e| e.into_inner());
            let counter = counters.entry(route.to_owned()).or_default();
            let n = *counter;
            *counter += 1;
            n
        };
        Picks::new(mix(seed, &[route, &n.to_string()]))
    }
}

/// A fixed delay with a uniform `± jitter`, applied before the headers.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Latency {
    #[serde(default)]
    pub delay_ms: u64,
    #[serde(default)]
    pub jitter_ms: u64,
}

impl Latency {
    /// An error when a value is over [`MAX_DELAY_MS`].
    pub fn check(&self) -> Result<(), String> {
        for (name, value) in [("delay_ms", self.delay_ms), ("jitter_ms", self.jitter_ms)] {
            if value > MAX_DELAY_MS {
                return Err(format!("{name} {value} exceeds {MAX_DELAY_MS}"));
            }
        }
        Ok(())
    }

    pub fn is_zero(&self) -> bool {
        self.delay_ms == 0 && self.jitter_ms == 0
    }

    /// The delay for one request, within `0..=MAX_DELAY_MS`.
    pub fn delay(&self, picks: &mut Picks) -> Duration {
        let mut ms = self.delay_ms;
        if self.jitter_ms > 0 {
            let span = self.jitter_ms * 2 + 1;
            let offset = picks.next_u64() % span;
            ms = (ms + offset).saturating_sub(self.jitter_ms);
        }
        Duration::from_millis(ms.min(MAX_DELAY_MS))
    }
}

/// `fault:` of an override.
#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Fault {
    /// The share of requests answered with `status`, `0.0..=1.0`.
    #[serde(default)]
    pub error_rate: f64,
    /// The status of an injected error (default `500`).
    #[serde(default)]
    pub status: Option<u16>,
    /// Close the connection without any response.
    #[serde(default)]
    pub drop_connection: bool,
    /// Stream the body at this many bytes per second.
    #[serde(default)]
    pub slow_body_bps: Option<u64>,
}

/// What a fault does to one request.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Injected {
    None,
    Error(StatusCode),
    Drop,
}

impl Fault {
    pub fn check(&self) -> Result<(), String> {
        if !(0.0..=1.0).contains(&self.error_rate) {
            return Err(format!(
                "error_rate {} is not within 0.0..=1.0",
                self.error_rate
            ));
        }
        if self.slow_body_bps == Some(0) {
            return Err("slow_body_bps must be at least 1".to_owned());
        }
        self.error_status().map(|_| ())
    }

    fn error_status(&self) -> Result<StatusCode, String> {
        let code = self.status.unwrap_or(500);
        StatusCode::from_u16(code)
            .ok()
            .filter(|s| s.as_u16() >= 400)
            .ok_or_else(|| format!("fault status {code} is not an error status (400-599)"))
    }

    /// The fault for one request; one pick is drawn either way, so the
    /// sequence does not depend on the configuration of other fields.
    pub fn decide(&self, picks: &mut Picks) -> Injected {
        let roll = picks.unit();
        if self.drop_connection {
            return Injected::Drop;
        }
        if self.error_rate > 0.0 && roll < self.error_rate {
            return Injected::Error(
                self.error_status()
                    .unwrap_or(StatusCode::INTERNAL_SERVER_ERROR),
            );
        }
        Injected::None
    }
}

/// A body sent in chunks at `bps` bytes per second.
#[derive(Debug)]
pub struct SlowBody {
    rest: Bytes,
    chunk: usize,
    sleep: Pin<Box<Sleep>>,
}

impl SlowBody {
    pub fn new(body: Bytes, bps: u64) -> Self {
        let per_tick = bps.saturating_mul(SLOW_TICK.as_millis() as u64) / 1000;
        Self {
            rest: body,
            chunk: usize::try_from(per_tick.max(1)).unwrap_or(usize::MAX),
            sleep: Box::pin(tokio::time::sleep(Duration::ZERO)),
        }
    }
}

impl Body for SlowBody {
    type Data = Bytes;
    type Error = Infallible;

    fn poll_frame(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Bytes>, Infallible>>> {
        if self.rest.is_empty() {
            return Poll::Ready(None);
        }
        if self.sleep.as_mut().poll(cx).is_pending() {
            return Poll::Pending;
        }
        let take = self.chunk.min(self.rest.len());
        let chunk = self.rest.split_to(take);
        let next = Instant::now() + SLOW_TICK;
        self.sleep.as_mut().reset(next);
        Poll::Ready(Some(Ok(Frame::data(chunk))))
    }

    fn is_end_stream(&self) -> bool {
        self.rest.is_empty()
    }

    fn size_hint(&self) -> SizeHint {
        SizeHint::with_exact(self.rest.len() as u64)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use http_body_util::BodyExt;

    #[test]
    fn sequences_are_per_route_and_reproducible() {
        let a = Sequences::default();
        let first: Vec<u64> = (0..3).map(|_| a.next(1, "GET /x").next_u64()).collect();
        let b = Sequences::default();
        b.next(1, "GET /other");
        let again: Vec<u64> = (0..3).map(|_| b.next(1, "GET /x").next_u64()).collect();
        assert_eq!(first, again);
        assert_ne!(first[0], first[1]);
        assert_ne!(first[0], Sequences::default().next(2, "GET /x").next_u64());
    }

    #[test]
    fn jitter_stays_in_range() {
        let latency = Latency {
            delay_ms: 100,
            jitter_ms: 50,
        };
        let mut picks = Picks::new(3);
        for _ in 0..200 {
            let ms = latency.delay(&mut picks).as_millis();
            assert!((50..=150).contains(&ms), "{ms}");
        }
        let low = Latency {
            delay_ms: 0,
            jitter_ms: 10,
        };
        assert!(low.delay(&mut picks) <= Duration::from_millis(10));
        assert!(
            Latency {
                delay_ms: 60_001,
                jitter_ms: 0
            }
            .check()
            .is_err()
        );
        assert!(
            Latency {
                delay_ms: 0,
                jitter_ms: 60_001
            }
            .check()
            .is_err()
        );
    }

    #[test]
    fn error_rates_are_honored() {
        let always = Fault {
            error_rate: 1.0,
            status: Some(503),
            ..Fault::default()
        };
        let never = Fault::default();
        let half = Fault {
            error_rate: 0.5,
            ..Fault::default()
        };
        let mut picks = Picks::new(9);
        assert_eq!(
            always.decide(&mut picks),
            Injected::Error(StatusCode::SERVICE_UNAVAILABLE)
        );
        assert_eq!(never.decide(&mut picks), Injected::None);
        let errors = (0..1000)
            .filter(|_| half.decide(&mut picks) != Injected::None)
            .count();
        assert!((400..600).contains(&errors), "{errors}");
        let drop = Fault {
            drop_connection: true,
            ..Fault::default()
        };
        assert_eq!(drop.decide(&mut picks), Injected::Drop);
    }

    #[test]
    fn invalid_faults_are_refused() {
        assert!(
            Fault {
                error_rate: 1.5,
                ..Fault::default()
            }
            .check()
            .is_err()
        );
        assert!(
            Fault {
                status: Some(200),
                ..Fault::default()
            }
            .check()
            .is_err()
        );
        assert!(
            Fault {
                slow_body_bps: Some(0),
                ..Fault::default()
            }
            .check()
            .is_err()
        );
        assert!(
            Fault {
                error_rate: 0.2,
                status: Some(503),
                ..Fault::default()
            }
            .check()
            .is_ok()
        );
    }

    #[tokio::test(start_paused = true)]
    async fn slow_bodies_are_paced() {
        let started = Instant::now();
        let body = SlowBody::new(Bytes::from_static(b"0123456789"), 20);
        let collected = body.collect().await.unwrap().to_bytes();
        assert_eq!(collected.as_ref(), b"0123456789");
        // 2 bytes per 100 ms tick: 5 chunks, the first one at once.
        assert_eq!(started.elapsed(), Duration::from_millis(400));
    }
}
