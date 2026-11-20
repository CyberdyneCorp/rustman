//! Errors of loading a workspace's plugins. Every message names the
//! plugin (plugins spec, "Plugin package and manifest").

use std::path::Path;

/// Why a plugin, or the workspace's plugin set, could not be loaded.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum LoadError {
    /// An invalid declaration, manifest, grant or limit.
    #[error("plugin {plugin}: {message}")]
    Invalid { plugin: String, message: String },
    /// A plugin file that exists but is not read: not a regular file
    /// inside the workspace, too large, or not readable.
    #[error("plugin {plugin}: {message}")]
    Unreadable { plugin: String, message: String },
    /// The plugin targets an API major this build does not support.
    #[error("plugin {plugin} targets plugin API {api}; this rustman supports {supported}")]
    UnsupportedApi {
        plugin: String,
        api: u64,
        supported: String,
    },
    /// The `.wasm` file does not hash to its pin.
    #[error("plugin {plugin}: sha256 mismatch (expected {expected}, got {actual})")]
    Mismatch {
        plugin: String,
        expected: String,
        actual: String,
    },
    /// A URL source that is not `https`.
    #[error("plugin sources must use https")]
    PlainHttp,
    /// `rustman.toml` or `rustman.lock` cannot be read or written.
    #[error("{0}")]
    Workspace(String),
}

impl LoadError {
    pub(crate) fn invalid(plugin: &str, message: impl Into<String>) -> Self {
        Self::Invalid {
            plugin: plugin.to_owned(),
            message: message.into(),
        }
    }

    /// An I/O error on `path`: a missing file is an invalid declaration,
    /// any other error makes the file [`LoadError::Unreadable`].
    pub(crate) fn io(plugin: &str, path: &Path, err: &std::io::Error) -> Self {
        let message = format!("{}: {err}", path.display());
        if err.kind() == std::io::ErrorKind::NotFound {
            Self::invalid(plugin, message)
        } else {
            Self::Unreadable {
                plugin: plugin.to_owned(),
                message,
            }
        }
    }

    /// The exit code of a front-end that stops on this error: every load
    /// error is a configuration error.
    pub fn exit_code(&self) -> u8 {
        2
    }

    /// Whether the workspace can still run without this plugin: a plugin
    /// for another API major is skipped, everything else stops the load.
    pub fn is_skippable(&self) -> bool {
        matches!(self, Self::UnsupportedApi { .. })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn messages_name_the_plugin() {
        let mismatch = LoadError::Mismatch {
            plugin: "hmac".into(),
            expected: "ab12".into(),
            actual: "cd34".into(),
        };
        assert_eq!(
            mismatch.to_string(),
            "plugin hmac: sha256 mismatch (expected ab12, got cd34)"
        );
        assert_eq!(mismatch.exit_code(), 2);
        assert!(!mismatch.is_skippable());
        let api = LoadError::UnsupportedApi {
            plugin: "hmac".into(),
            api: 2,
            supported: "1".into(),
        };
        assert_eq!(
            api.to_string(),
            "plugin hmac targets plugin API 2; this rustman supports 1"
        );
        assert!(api.is_skippable());
        assert_eq!(
            LoadError::PlainHttp.to_string(),
            "plugin sources must use https"
        );
    }
}
