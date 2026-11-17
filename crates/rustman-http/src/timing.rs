//! The timing breakdown of one exchange.
//!
//! Phases that did not happen (DNS for an IP literal, TLS for plain HTTP,
//! every connection phase on a reused connection) are `None`, never zero.

use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use serde::{Serialize, Serializer};

/// Durations of one exchange, serialized as milliseconds with microsecond
/// precision (`12.345`), or `null` for a phase that did not apply.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
pub struct Timings {
    #[serde(serialize_with = "optional_ms")]
    pub dns: Option<Duration>,
    #[serde(serialize_with = "optional_ms")]
    pub connect: Option<Duration>,
    #[serde(serialize_with = "optional_ms")]
    pub tls: Option<Duration>,
    /// Connection ready (or request start, on a reused connection) until the
    /// response head arrived.
    #[serde(serialize_with = "ms")]
    pub ttfb: Duration,
    /// Response head until the last body byte.
    #[serde(serialize_with = "ms")]
    pub download: Duration,
    #[serde(serialize_with = "ms")]
    pub total: Duration,
}

/// Milliseconds with microsecond precision.
pub fn millis(duration: Duration) -> f64 {
    // A u128 of microseconds fits an f64 exactly for any realistic duration.
    duration.as_micros() as f64 / 1000.0
}

fn ms<S: Serializer>(duration: &Duration, s: S) -> Result<S::Ok, S::Error> {
    s.serialize_f64(millis(*duration))
}

fn optional_ms<S: Serializer>(duration: &Option<Duration>, s: S) -> Result<S::Ok, S::Error> {
    match duration {
        Some(d) => ms(d, s),
        None => s.serialize_none(),
    }
}

/// The phases of opening one connection.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ConnectTimings {
    pub dns: Option<Duration>,
    pub connect: Duration,
    pub tls: Option<Duration>,
    /// When the connection was ready to carry a request.
    pub ready_at: Instant,
}

/// Connection timings handed to the first response on that connection only,
/// so later requests reusing it report `None` phases.
#[derive(Clone, Debug)]
pub(crate) struct FreshConnection(Arc<Mutex<Option<ConnectTimings>>>);

impl FreshConnection {
    pub fn new(timings: ConnectTimings) -> Self {
        Self(Arc::new(Mutex::new(Some(timings))))
    }

    pub fn take(&self) -> Option<ConnectTimings> {
        self.0.lock().unwrap_or_else(PoisonError::into_inner).take()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn serializes_milliseconds_and_nulls() {
        let timings = Timings {
            dns: None,
            connect: Some(Duration::from_micros(1_500)),
            tls: None,
            ttfb: Duration::from_micros(12_345),
            download: Duration::ZERO,
            total: Duration::from_millis(20),
        };
        let json = serde_json::to_value(timings).unwrap();
        assert_eq!(
            json,
            serde_json::json!({
                "dns": null, "connect": 1.5, "tls": null,
                "ttfb": 12.345, "download": 0.0, "total": 20.0
            })
        );
    }

    #[test]
    fn connection_timings_are_taken_once() {
        let fresh = FreshConnection::new(ConnectTimings {
            dns: None,
            connect: Duration::from_millis(1),
            tls: None,
            ready_at: Instant::now(),
        });
        let shared = fresh.clone();
        assert!(fresh.take().is_some());
        assert!(shared.take().is_none());
    }
}
