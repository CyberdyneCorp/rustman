//! The `[safety]` table of `rustman.toml`.

use std::num::NonZeroU32;

use serde::Deserialize;

use super::origin::Origin;
use super::production::HostPattern;

/// Default requests per second for governed senders.
pub const DEFAULT_RATE: NonZeroU32 = NonZeroU32::new(10).unwrap();
/// Default concurrent requests for governed senders.
pub const DEFAULT_CONCURRENCY: NonZeroU32 = NonZeroU32::new(4).unwrap();

/// `[safety]`: workspace-wide limits for automated senders (exploration and
/// agents). Command-line flags override `rate` and `concurrency` and add to
/// `allowed_hosts`.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct SafetySettings {
    /// Origins allowed in addition to the target's own.
    pub allowed_hosts: Vec<Origin>,
    /// Hosts that are always treated as production.
    pub production_hosts: Vec<HostPattern>,
    /// Requests per second.
    pub rate: NonZeroU32,
    /// Probes in flight at once; a probe holds one slot across its redirect
    /// hops.
    pub concurrency: NonZeroU32,
}

impl Default for SafetySettings {
    fn default() -> Self {
        Self {
            allowed_hosts: Vec::new(),
            production_hosts: Vec::new(),
            rate: DEFAULT_RATE,
            concurrency: DEFAULT_CONCURRENCY,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::*;
    use crate::error::ConfigError;
    use crate::settings::Settings;

    const HEAD: &str = "[workspace]\nname = \"shop\"\nversion = 1\n";

    fn parse(safety: &str) -> Result<SafetySettings, ConfigError> {
        let text = format!("{HEAD}{safety}");
        let (settings, warnings) = Settings::parse(Path::new("rustman.toml"), &text)?;
        assert!(warnings.is_empty(), "{warnings:?}");
        Ok(settings.safety)
    }

    fn error_at(safety: &str) -> (Option<usize>, String) {
        let err = parse(safety).unwrap_err();
        let ConfigError::Invalid(errors) = &err else {
            panic!("{err}")
        };
        (errors[0].line(), err.to_string())
    }

    #[test]
    fn a_missing_table_applies_the_defaults() {
        let safety = parse("").unwrap();
        assert_eq!(safety, SafetySettings::default());
        assert!(safety.allowed_hosts.is_empty());
        assert!(safety.production_hosts.is_empty());
        assert_eq!(safety.rate.get(), 10);
        assert_eq!(safety.concurrency.get(), 4);
        assert_eq!(parse("[safety]\n").unwrap(), SafetySettings::default());
    }

    #[test]
    fn every_key_is_read() {
        let safety = parse(
            r#"
[safety]
allowed_hosts = ["https://auth.example.com", "http://127.0.0.1:9000"]
production_hosts = ["api.example.com", "*.prod.example.com"]
rate = 2
concurrency = 1
"#,
        )
        .unwrap();
        let allowed: Vec<String> = safety.allowed_hosts.iter().map(|o| o.to_string()).collect();
        assert_eq!(
            allowed,
            ["https://auth.example.com:443", "http://127.0.0.1:9000"]
        );
        assert!(safety.production_hosts[0].matches("api.example.com"));
        assert!(safety.production_hosts[1].matches("eu.prod.example.com"));
        assert_eq!(safety.rate.get(), 2);
        assert_eq!(safety.concurrency.get(), 1);
    }

    #[test]
    fn invalid_values_fail_with_their_line() {
        let rows = [
            (
                "[safety]\nallowed_hosts = [\"auth.example.com\"]\n",
                "invalid origin 'auth.example.com'",
            ),
            (
                "[safety]\nallowed_hosts = [\"https://auth.example.com/v1\"]\n",
                "expected scheme://host[:port]",
            ),
            (
                "[safety]\nproduction_hosts = [\"https://api.example.com\"]\n",
                "invalid production host 'https://api.example.com'",
            ),
            ("[safety]\nrate = 0\n", "nonzero"),
            ("[safety]\nconcurrency = -1\n", "expected a nonzero u32"),
            ("[safety]\nrate = \"fast\"\n", "expected a nonzero u32"),
        ];
        for (text, message) in rows {
            let (line, err) = error_at(text);
            assert_eq!(line, Some(5), "{text}: {err}");
            assert!(err.contains(message), "{text}: {err}");
        }
    }

    #[test]
    fn unknown_safety_keys_are_errors() {
        // A misspelled production_hosts must not silently disable the guard.
        let (line, err) = error_at("[safety]\nproduction_host = [\"api.example.com\"]\n");
        assert_eq!(line, Some(5), "{err}");
        assert!(err.contains("unknown field `production_host`"), "{err}");
    }
}
