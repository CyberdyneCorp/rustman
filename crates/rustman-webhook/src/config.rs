//! Listener settings: bind address, request limits and buffer bounds, with
//! the defaults of the webhook-inspector spec.

use std::time::Duration;

use time::UtcOffset;

pub const DEFAULT_HOST: &str = "127.0.0.1";
pub const DEFAULT_PORT: u16 = 9090;

const MIB: usize = 1024 * 1024;

/// Per-request limits (spec: Body size limit, Header limits, Timeouts).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Limits {
    /// Largest accepted body, declared or streamed; larger bodies get 413.
    pub max_body: usize,
    /// Most header fields per request; more get 431.
    pub max_headers: usize,
    /// Most bytes of header fields per request; more get 431.
    pub max_header_bytes: usize,
    /// Time allowed to receive the request head.
    pub header_timeout: Duration,
    /// Time allowed to receive the body once the head arrived.
    pub body_timeout: Duration,
    /// An open connection with no request in flight and no traffic for this
    /// long is closed.
    pub idle_timeout: Duration,
    /// Most connections served at once; more wait to be accepted.
    pub max_connections: usize,
    /// Most requests in flight on one HTTP/2 connection.
    pub max_streams: u32,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            max_body: 10 * MIB,
            max_headers: 100,
            max_header_bytes: 64 * 1024,
            header_timeout: Duration::from_secs(10),
            body_timeout: Duration::from_secs(30),
            idle_timeout: Duration::from_secs(60),
            max_connections: 256,
            max_streams: 16,
        }
    }
}

/// Bounds of the in-memory capture buffer (spec: Bounded capture buffer).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BufferLimits {
    pub max_captures: usize,
    /// Total body bytes kept across all captures.
    pub max_buffer_bytes: usize,
}

impl BufferLimits {
    /// For a front-end that only follows events and never reads the
    /// buffer back: just the newest capture is kept.
    pub fn events_only() -> Self {
        Self {
            max_captures: 1,
            max_buffer_bytes: 0,
        }
    }
}

impl Default for BufferLimits {
    fn default() -> Self {
        Self {
            max_captures: 500,
            max_buffer_bytes: 256 * MIB,
        }
    }
}

/// Where and how the listener runs.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ListenerConfig {
    /// An IP address or `localhost`.
    pub host: String,
    /// `0` picks a free port.
    pub port: u16,
    /// Allow binding an address reachable from the network.
    pub allow_remote: bool,
    pub limits: Limits,
    /// The offset capture timestamps are shown in.
    pub offset: UtcOffset,
}

impl Default for ListenerConfig {
    fn default() -> Self {
        Self {
            host: DEFAULT_HOST.to_owned(),
            port: DEFAULT_PORT,
            allow_remote: false,
            limits: Limits::default(),
            offset: local_offset(),
        }
    }
}

/// The local UTC offset, or UTC when the platform cannot tell safely (on
/// Unix once the process runs several threads). Front-ends call this before
/// starting a multi-threaded runtime.
pub fn local_offset() -> UtcOffset {
    UtcOffset::current_local_offset().unwrap_or(UtcOffset::UTC)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_follow_the_spec() {
        let config = ListenerConfig::default();
        assert_eq!((config.host.as_str(), config.port), ("127.0.0.1", 9090));
        assert!(!config.allow_remote);
        let limits = config.limits;
        assert_eq!(limits.max_body, 10_485_760);
        assert_eq!((limits.max_headers, limits.max_header_bytes), (100, 65_536));
        assert_eq!(limits.header_timeout, Duration::from_secs(10));
        assert_eq!(limits.body_timeout, Duration::from_secs(30));
        assert_eq!(limits.idle_timeout, Duration::from_secs(60));
        assert_eq!((limits.max_connections, limits.max_streams), (256, 16));
        let buffer = BufferLimits::default();
        assert_eq!(
            (buffer.max_captures, buffer.max_buffer_bytes),
            (500, 268_435_456)
        );
    }

    #[test]
    fn an_events_only_store_keeps_just_the_newest_capture() {
        use crate::capture::tests::capture;
        let store = crate::CaptureStore::new(BufferLimits::events_only());
        store.push(capture("/a", b"abc"));
        store.push(capture("/b", b"defg"));
        assert_eq!(store.len(), 1);
        assert_eq!(store.total_bytes(), 4);
        assert_eq!(store.snapshot()[0].path, "/b");
    }
}
