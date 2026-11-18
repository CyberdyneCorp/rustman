//! The `[webhook]` section of `rustman.toml`: the bind address (spec:
//! Listener lifecycle and bind address) and the signature verifiers
//! (design D4). `rustman-core` reads the rest of the file and accepts this
//! section without warnings.

use std::path::Path;

use rustman_core::LoadError;
use rustman_core::settings::SETTINGS_FILE;
use serde::Deserialize;

use crate::config::ListenerConfig;
use crate::verifiers::VerifierSpec;

/// `[webhook]`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize)]
pub struct WebhookSettings {
    /// The address to bind when `--host` is not given.
    #[serde(default)]
    pub host: Option<String>,
    /// The port to bind when `--port` is not given.
    #[serde(default)]
    pub port: Option<u16>,
    #[serde(default)]
    pub verifiers: Vec<VerifierSpec>,
}

#[derive(Deserialize)]
struct File {
    #[serde(default)]
    webhook: WebhookSettings,
}

impl WebhookSettings {
    /// The `[webhook]` section of `text`, the contents of `rustman.toml`.
    pub fn parse(text: &str) -> Result<Self, LoadError> {
        toml::from_str::<File>(text)
            .map(|file| file.webhook)
            .map_err(|e| {
                let message = format!("[webhook]: {}", e.message().trim_end());
                LoadError::new(Path::new(SETTINGS_FILE), None, message)
            })
    }

    /// The listener settings these ask for: `host` and `port` over the
    /// defaults. Front-ends apply their own flags on top.
    pub fn listener_config(&self) -> ListenerConfig {
        let defaults = ListenerConfig::default();
        ListenerConfig {
            host: self.host.clone().unwrap_or(defaults.host),
            port: self.port.unwrap_or(defaults.port),
            ..defaults
        }
    }

    /// The `[webhook]` section of the workspace at `root`.
    pub fn load(root: &Path) -> Result<Self, LoadError> {
        let file = root.join(SETTINGS_FILE);
        let text = std::fs::read_to_string(&file)
            .map_err(|e| LoadError::new(Path::new(SETTINGS_FILE), None, e.to_string()))?;
        Self::parse(&text)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_listener_config_takes_host_and_port_over_the_defaults() {
        let config = WebhookSettings::default().listener_config();
        assert_eq!((config.host.as_str(), config.port), ("127.0.0.1", 9090));
        let settings = WebhookSettings {
            host: Some("0.0.0.0".into()),
            port: Some(9191),
            ..WebhookSettings::default()
        };
        let config = settings.listener_config();
        assert_eq!((config.host.as_str(), config.port), ("0.0.0.0", 9191));
        assert!(!config.allow_remote);
    }

    #[test]
    fn verifiers_are_read_in_order() {
        let text = r#"
[workspace]
name = "a"
version = 1

[[webhook.verifiers]]
path = "/hooks/github"
type = "github"
secret = "{{gh_hook}}"

[[webhook.verifiers]]
path = "/hooks/shop/**"
type = "hmac"
header = "X-Signature"
encoding = "base64"
secret = "{{shop}}"
"#;
        let settings = WebhookSettings::parse(text).unwrap();
        let kinds: Vec<_> = settings.verifiers.iter().map(|v| v.kind.as_str()).collect();
        assert_eq!(kinds, ["github", "hmac"]);
        assert_eq!(settings.verifiers[1].encoding.as_deref(), Some("base64"));
    }

    #[test]
    fn host_and_port_are_read() {
        let text = "[webhook]\nhost = \"0.0.0.0\"\nport = 9191\n";
        let settings = WebhookSettings::parse(text).unwrap();
        assert_eq!(
            (settings.host.as_deref(), settings.port),
            (Some("0.0.0.0"), Some(9191))
        );
        let err = WebhookSettings::parse("[webhook]\nport = 70000\n").unwrap_err();
        assert!(err.to_string().contains("[webhook]"), "{err}");
    }

    #[test]
    fn no_section_means_no_verifiers_and_typos_fail() {
        let none = WebhookSettings::parse("[workspace]\nname = \"a\"\nversion = 1\n").unwrap();
        assert!(none.verifiers.is_empty());
        let typo = "[[webhook.verifiers]]\npath = \"/h\"\ntype = \"github\"\nsecret = \"{{s}}\"\nsecrte = 1\n";
        let err = WebhookSettings::parse(typo).unwrap_err().to_string();
        assert!(err.contains("unknown field `secrte`"), "{err}");
    }
}
