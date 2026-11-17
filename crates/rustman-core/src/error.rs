//! Error types shared by the workspace loader and its callers.

use std::fmt;
use std::io;
use std::path::{Path, PathBuf};

use crate::secrets::SecretError;
use crate::settings::SETTINGS_VERSION;

/// A 1-based line and column inside a workspace file.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Position {
    pub line: usize,
    pub column: usize,
}

/// A problem in one workspace file, rendered as `<path>:<line>: <message>`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LoadError {
    pub file: PathBuf,
    pub position: Option<Position>,
    pub message: String,
}

impl LoadError {
    pub fn new(file: &Path, position: Option<Position>, message: impl Into<String>) -> Self {
        Self {
            file: file.to_path_buf(),
            position,
            message: message.into(),
        }
    }

    /// The line of the error, when it is known.
    pub fn line(&self) -> Option<usize> {
        self.position.map(|p| p.line)
    }

    /// The column of the error, when it is known.
    pub fn column(&self) -> Option<usize> {
        self.position.map(|p| p.column)
    }
}

impl fmt::Display for LoadError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let file = self.file.display();
        match self.position {
            Some(p) => write!(f, "{file}:{}: {}", p.line, self.message),
            None => write!(f, "{file}: {}", self.message),
        }
    }
}

impl std::error::Error for LoadError {}

/// A configuration problem: no workspace, an invalid file, an unsupported version.
///
/// The CLI maps every variant to exit code 2 (design D12).
#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("no rustman workspace found (run 'rustman init')")]
    NoWorkspace,
    #[error("{0} is already a rustman workspace")]
    AlreadyWorkspace(String),
    #[error("rustman.toml version {0} is newer than this rustman supports ({SETTINGS_VERSION})")]
    NewerSettingsVersion(i64),
    #[error("{}", join_lines(.0))]
    Invalid(Vec<LoadError>),
    #[error(
        "environment '{name}' not found (available: {})",
        list_or_none(available)
    )]
    EnvironmentNotFound {
        name: String,
        available: Vec<String>,
    },
    #[error("no environment selected (pass --env <name> or set RUSTMAN_ENV)")]
    NoActiveEnvironment,
    #[error("request '{0}' not found (pass a request file, its id or its name)")]
    RequestNotFound(String),
    #[error("request name '{name}' is ambiguous: {}", paths.join(", "))]
    AmbiguousRequest { name: String, paths: Vec<String> },
    #[error(transparent)]
    Secrets(#[from] SecretError),
    #[error("{}: {source}", path.display())]
    Io {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
}

impl ConfigError {
    pub(crate) fn io(path: &Path, source: io::Error) -> Self {
        Self::Io {
            path: path.to_path_buf(),
            source,
        }
    }
}

impl From<LoadError> for ConfigError {
    fn from(err: LoadError) -> Self {
        Self::Invalid(vec![err])
    }
}

fn list_or_none(names: &[String]) -> String {
    if names.is_empty() {
        "none".to_owned()
    } else {
        names.join(", ")
    }
}

fn join_lines(errors: &[LoadError]) -> String {
    errors
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn load_error_renders_path_and_line() {
        let err = LoadError::new(
            Path::new("a.rman.yaml"),
            Some(Position { line: 2, column: 7 }),
            "boom",
        );
        assert_eq!(err.to_string(), "a.rman.yaml:2: boom");
        assert_eq!((err.line(), err.column()), (Some(2), Some(7)));
    }

    #[test]
    fn load_error_without_position_renders_path_only() {
        let err = LoadError::new(Path::new("rustman.toml"), None, "boom");
        assert_eq!(err.to_string(), "rustman.toml: boom");
    }

    #[test]
    fn config_error_texts_match_the_spec() {
        assert_eq!(
            ConfigError::NoWorkspace.to_string(),
            "no rustman workspace found (run 'rustman init')"
        );
        assert_eq!(
            ConfigError::NewerSettingsVersion(2).to_string(),
            "rustman.toml version 2 is newer than this rustman supports (1)"
        );
        assert_eq!(
            ConfigError::AlreadyWorkspace("proj".into()).to_string(),
            "proj is already a rustman workspace"
        );
    }

    #[test]
    fn request_lookup_errors_name_the_target() {
        assert_eq!(
            ConfigError::AmbiguousRequest {
                name: "List".into(),
                paths: vec!["a/list.rman.yaml".into(), "b/list.rman.yaml".into()],
            }
            .to_string(),
            "request name 'List' is ambiguous: a/list.rman.yaml, b/list.rman.yaml"
        );
        assert_eq!(
            ConfigError::RequestNotFound("nope".into()).to_string(),
            "request 'nope' not found (pass a request file, its id or its name)"
        );
    }

    #[test]
    fn invalid_lists_every_error() {
        let a = LoadError::new(Path::new("a"), None, "x");
        let b = LoadError::new(Path::new("b"), None, "y");
        assert_eq!(ConfigError::Invalid(vec![a, b]).to_string(), "a: x\nb: y");
    }
}
