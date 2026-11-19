//! The `[mock]` section of `rustman.toml`: the bind address, the seed, the
//! request limits, the mocks directory, latency, the stateful cap and the
//! expected security values. `rustman-core` reads the rest of the file and
//! accepts this section without warnings.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use rustman_core::LoadError;
use rustman_core::settings::SETTINGS_FILE;
use serde::Deserialize;

use crate::config::MockConfig;
use crate::faults::Latency;
use crate::overrides::MOCKS_DIR;

/// `[mock]`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize)]
pub struct MockSettings {
    /// The address to bind when `--host` is not given.
    #[serde(default)]
    pub host: Option<String>,
    /// The port to bind when `--port` is not given.
    #[serde(default)]
    pub port: Option<u16>,
    /// The synthesis seed when `--seed` is not given.
    #[serde(default)]
    pub seed: Option<u64>,
    /// Largest accepted request body in bytes (413 past it).
    #[serde(default)]
    pub max_body_bytes: Option<usize>,
    /// Most header fields per request (431 past it).
    #[serde(default)]
    pub max_headers: Option<usize>,
    /// Most bytes of header fields per request (431 past it).
    #[serde(default)]
    pub max_header_bytes: Option<usize>,
    /// The directory of `*.rman-mock.yaml` files, relative to the
    /// workspace root (default `mocks`).
    #[serde(default)]
    pub dir: Option<PathBuf>,
    /// The delay of every response without an override delay.
    #[serde(default)]
    pub delay_ms: Option<u64>,
    /// A uniform `±` range around `delay_ms`.
    #[serde(default)]
    pub jitter_ms: Option<u64>,
    /// The most items per resource in stateful mode (507 past it).
    #[serde(default)]
    pub max_items: Option<usize>,
    /// The most bytes of JSON the stateful store holds (507 past it).
    #[serde(default)]
    pub max_store_bytes: Option<usize>,
    /// `[mock.security]`: expected credential values by security scheme
    /// name, each a `{{variable}}` read from the active environment.
    #[serde(default)]
    pub security: BTreeMap<String, String>,
}

#[derive(Deserialize)]
struct File {
    #[serde(default)]
    mock: MockSettings,
}

impl MockSettings {
    /// The `[mock]` section of `text`, the contents of `rustman.toml`.
    pub fn parse(text: &str) -> Result<Self, LoadError> {
        toml::from_str::<File>(text)
            .map(|file| file.mock)
            .map_err(|e| {
                let message = format!("[mock]: {}", e.message().trim_end());
                LoadError::new(Path::new(SETTINGS_FILE), None, message)
            })
    }

    /// The `[mock]` section of the workspace at `root`.
    pub fn load(root: &Path) -> Result<Self, LoadError> {
        let text = std::fs::read_to_string(root.join(SETTINGS_FILE))
            .map_err(|e| LoadError::new(Path::new(SETTINGS_FILE), None, e.to_string()))?;
        Self::parse(&text)
    }

    /// The mocks directory of the workspace at `root`.
    pub fn mocks_dir(&self, root: &Path) -> PathBuf {
        root.join(self.dir.as_deref().unwrap_or(Path::new(MOCKS_DIR)))
    }

    /// The `[mock]` latency, refused past 60000 ms.
    pub fn latency(&self) -> Result<Latency, LoadError> {
        let latency = Latency {
            delay_ms: self.delay_ms.unwrap_or(0),
            jitter_ms: self.jitter_ms.unwrap_or(0),
        };
        latency
            .check()
            .map(|()| latency)
            .map_err(|e| LoadError::new(Path::new(SETTINGS_FILE), None, format!("[mock]: {e}")))
    }

    /// The mock settings these ask for, over the defaults. Front-ends
    /// apply their own flags (and resolved security values) on top.
    pub fn config(&self) -> MockConfig {
        let defaults = MockConfig::default();
        let limits = defaults.limits.clone();
        MockConfig {
            host: self.host.clone().unwrap_or(defaults.host),
            port: self.port.unwrap_or(defaults.port),
            seed: self.seed.unwrap_or(defaults.seed),
            limits: rustman_webhook::Limits {
                max_body: self.max_body_bytes.unwrap_or(limits.max_body),
                max_headers: self.max_headers.unwrap_or(limits.max_headers),
                max_header_bytes: self.max_header_bytes.unwrap_or(limits.max_header_bytes),
                ..limits
            },
            max_items: self.max_items.unwrap_or(defaults.max_items),
            max_store_bytes: self.max_store_bytes.unwrap_or(defaults.max_store_bytes),
            latency: Latency {
                delay_ms: self.delay_ms.unwrap_or(0),
                jitter_ms: self.jitter_ms.unwrap_or(0),
            },
            ..defaults
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_section_keeps_the_defaults() {
        let settings = MockSettings::parse("[workspace]\nname = \"a\"\nversion = 1\n").unwrap();
        assert_eq!(settings.config(), MockConfig::default());
    }

    #[test]
    fn keys_override_the_defaults() {
        let text = "[mock]\nhost = \"::1\"\nport = 4011\nseed = 7\nmax_body_bytes = 10\nmax_headers = 5\nmax_header_bytes = 900\n";
        let config = MockSettings::parse(text).unwrap().config();
        assert_eq!(
            (config.host.as_str(), config.port, config.seed),
            ("::1", 4011, 7)
        );
        let limits = config.limits;
        assert_eq!(
            (limits.max_body, limits.max_headers, limits.max_header_bytes),
            (10, 5, 900)
        );
    }

    #[test]
    fn mocks_latency_cap_and_security_keys() {
        let text = "[mock]\ndir = \"stubs\"\ndelay_ms = 20\njitter_ms = 5\nmax_items = 3\nmax_store_bytes = 4096\n[mock.security]\nbearer = \"{{token}}\"\n";
        let settings = MockSettings::parse(text).unwrap();
        assert_eq!(
            settings.mocks_dir(Path::new("ws")),
            Path::new("ws").join("stubs")
        );
        let config = settings.config();
        assert_eq!((config.latency.delay_ms, config.latency.jitter_ms), (20, 5));
        assert_eq!((config.max_items, config.max_store_bytes), (3, 4096));
        assert_eq!(MockSettings::default().config().max_store_bytes, 64 << 20);
        assert_eq!(settings.security["bearer"], "{{token}}");
        assert_eq!(
            MockSettings::default().mocks_dir(Path::new("ws")),
            Path::new("ws").join("mocks")
        );
        let slow = MockSettings::parse("[mock]\ndelay_ms = 60001\n").unwrap();
        assert!(
            slow.latency()
                .unwrap_err()
                .to_string()
                .contains("delay_ms 60001")
        );
    }

    #[test]
    fn invalid_values_name_the_section() {
        let err = MockSettings::parse("[mock]\nport = 70000\n").unwrap_err();
        assert!(err.to_string().contains("[mock]"), "{err}");
    }
}
