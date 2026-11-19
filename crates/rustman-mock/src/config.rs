//! Mock server settings: bind address, request limits, the synthesis seed,
//! `Prefer` handling, validation, security enforcement, stateful mode and
//! latency, with the defaults of the mock-server spec.

use rustman_webhook::{Limits, ListenerConfig};

use crate::faults::Latency;
use crate::proxy::Upstream;
use crate::security::Expected;
use crate::stateful::{DEFAULT_MAX_ITEMS, DEFAULT_MAX_STORE_BYTES};

pub const DEFAULT_HOST: &str = "127.0.0.1";
pub const DEFAULT_PORT: u16 = 4010;

/// Where and how the mock runs.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MockConfig {
    /// An IP address or `localhost`.
    pub host: String,
    /// `0` picks a free port.
    pub port: u16,
    /// Allow binding an address reachable from the network.
    pub allow_remote: bool,
    /// The same limits (and defaults) as the webhook listener.
    pub limits: Limits,
    /// Seed of synthesized bodies.
    pub seed: u64,
    /// Honor the `Prefer` header (off with `--no-prefer`).
    pub prefer: bool,
    /// Validate requests against their operation (off with `--no-validate`).
    pub validate: bool,
    /// Answer `401` to requests without the operation's credentials
    /// (`--enforce-security`).
    pub enforce_security: bool,
    /// Expected credential values by scheme name (`[mock.security]`).
    pub security: Expected,
    /// Back resources with the in-memory store (`--stateful`).
    pub stateful: bool,
    /// The most items per resource in stateful mode.
    pub max_items: usize,
    /// The most bytes of JSON the stateful store holds, all resources
    /// together.
    pub max_store_bytes: usize,
    /// The delay of every response without an override delay
    /// (`[mock] delay_ms` and `jitter_ms`).
    pub latency: Latency,
    /// CORS: `None` turns it on for loopback binds only (`--cors`,
    /// `--no-cors`).
    pub cors: Option<bool>,
    /// Forward requests no operation or override matches to this upstream
    /// (`--proxy-unmatched`).
    pub proxy_unmatched: Option<Upstream>,
}

impl Default for MockConfig {
    fn default() -> Self {
        Self {
            host: DEFAULT_HOST.to_owned(),
            port: DEFAULT_PORT,
            allow_remote: false,
            limits: Limits::default(),
            seed: 0,
            prefer: true,
            validate: true,
            enforce_security: false,
            security: Expected::new(),
            stateful: false,
            max_items: DEFAULT_MAX_ITEMS,
            max_store_bytes: DEFAULT_MAX_STORE_BYTES,
            latency: Latency::default(),
            cors: None,
            proxy_unmatched: None,
        }
    }
}

impl MockConfig {
    /// The listener settings the shared bind rules are checked with.
    pub(crate) fn listener(&self) -> ListenerConfig {
        ListenerConfig {
            host: self.host.clone(),
            port: self.port,
            allow_remote: self.allow_remote,
            limits: self.limits.clone(),
            ..ListenerConfig::default()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_follow_the_spec() {
        let config = MockConfig::default();
        assert_eq!((config.host.as_str(), config.port), ("127.0.0.1", 4010));
        assert!(!config.allow_remote && config.prefer && config.validate);
        assert!(!config.enforce_security && !config.stateful);
        assert_eq!(config.max_items, 10_000);
        assert_eq!(config.seed, 0);
        assert_eq!(config.limits.max_body, 10 * 1024 * 1024);
        assert_eq!(config.limits.max_headers, 100);
        assert_eq!(config.limits.max_header_bytes, 64 * 1024);
    }
}
