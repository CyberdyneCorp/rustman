//! Mapping errors to messages and exit codes (design D12, ROADMAP exit codes).

use rustman_core::ConfigError;
use rustman_core::history::HistoryError;
use rustman_core::run::Summary;
use rustman_core::vars::ResolveError;
use rustman_http::{CurlError, RequestError};
use rustman_import::ImportError;

/// Usage or configuration error: bad flag, invalid file, unsupported version.
pub const CONFIG: u8 = 2;

/// A request could not be completed: unresolved variables, network, TLS,
/// timeout, auth.
pub const REQUEST: u8 = 3;

/// Every executed request passed.
pub const SUCCESS: u8 = 0;

/// At least one assertion failed (and no request errored).
pub const ASSERTION: u8 = 1;

/// Interrupted by SIGINT (Ctrl+C).
pub const INTERRUPTED: u8 = 130;

/// Any error a command can end with.
#[derive(Debug, thiserror::Error)]
pub enum CliError {
    #[error(transparent)]
    Config(#[from] ConfigError),
    #[error(transparent)]
    Request(#[from] RequestError),
    #[error(transparent)]
    Resolve(#[from] ResolveError),
    /// A curl command that cannot be imported.
    #[error(transparent)]
    Curl(#[from] CurlError),
    /// History cannot be read or written, or has no such entry.
    #[error(transparent)]
    History(#[from] HistoryError),
    /// A source that cannot be imported, or an import that cannot be written.
    #[error(transparent)]
    Import(#[from] ImportError),
}

impl CliError {
    pub fn code(&self) -> u8 {
        match self {
            Self::Config(err) => code(err),
            Self::Request(_) | Self::Resolve(_) => REQUEST,
            Self::Curl(_) | Self::History(_) | Self::Import(_) => CONFIG,
        }
    }

    pub fn render(&self) -> String {
        render(self)
    }
}

/// The exit code for an error.
pub fn code(err: &ConfigError) -> u8 {
    match err {
        ConfigError::NoWorkspace
        | ConfigError::AlreadyWorkspace(_)
        | ConfigError::NewerSettingsVersion(_)
        | ConfigError::Invalid(_)
        | ConfigError::EnvironmentNotFound { .. }
        | ConfigError::NoActiveEnvironment
        | ConfigError::RequestNotFound(_)
        | ConfigError::AmbiguousRequest { .. }
        | ConfigError::Usage(_)
        | ConfigError::Secrets(_)
        | ConfigError::Io { .. } => CONFIG,
    }
}

/// The exit code of a finished run: interrupted wins, then errors ("3 wins
/// over 1"), then assertion failures.
pub fn run_code(summary: &Summary) -> u8 {
    if summary.interrupted {
        INTERRUPTED
    } else if summary.errored > 0 {
        REQUEST
    } else if summary.failed > 0 {
        ASSERTION
    } else {
        SUCCESS
    }
}

/// The text printed on stderr for an error: every line (one per invalid
/// file, for instance) gets the `error:` prefix.
pub fn render(err: &dyn std::fmt::Display) -> String {
    err.to_string()
        .lines()
        .map(|line| format!("error: {line}"))
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    use rustman_core::settings::Settings;

    #[test]
    fn no_workspace_renders_the_spec_text() {
        let err =
            rustman_core::discovery::find_root(&std::env::temp_dir().join("no/such/dir"), None)
                .unwrap_err();
        assert_eq!(
            render(&err),
            "error: no rustman workspace found (run 'rustman init')"
        );
        assert_eq!(code(&err), 2);
    }

    #[test]
    fn newer_settings_version_exits_2() {
        let text = "[workspace]\nname = \"a\"\nversion = 2\n";
        let err = Settings::parse(Path::new("rustman.toml"), text).unwrap_err();
        assert_eq!(code(&err), 2);
        assert_eq!(
            render(&err),
            "error: rustman.toml version 2 is newer than this rustman supports (1)"
        );
    }

    #[test]
    fn missing_environment_exits_2() {
        let err = ConfigError::EnvironmentNotFound {
            name: "qa".into(),
            available: vec!["local".into(), "staging".into()],
        };
        assert_eq!(code(&err), 2);
        assert_eq!(
            render(&err),
            "error: environment 'qa' not found (available: local, staging)"
        );
    }

    #[test]
    fn invalid_files_exit_2() {
        let err = ConfigError::Invalid(vec![rustman_core::LoadError::new(
            Path::new("a"),
            None,
            "x",
        )]);
        assert_eq!(code(&err), 2);
        assert_eq!(render(&err), "error: a: x");
    }

    #[test]
    fn every_invalid_file_line_is_prefixed() {
        let err = ConfigError::Invalid(vec![
            rustman_core::LoadError::new(Path::new("a"), None, "x"),
            rustman_core::LoadError::new(Path::new("b"), None, "y"),
        ]);
        assert_eq!(render(&err), "error: a: x\nerror: b: y");
    }

    #[test]
    fn request_errors_exit_3() {
        let err = CliError::from(RequestError::Auth("oauth2: denied".into()));
        assert_eq!(
            (err.code(), err.render().as_str()),
            (3, "error: oauth2: denied")
        );
        let err = CliError::from(ConfigError::RequestNotFound("x".into()));
        assert_eq!(err.code(), 2);
    }

    fn summary(failed: usize, errored: usize, interrupted: bool) -> Summary {
        Summary {
            total: 5,
            passed: 5 - failed - errored,
            failed,
            errored,
            interrupted,
            ..Summary::default()
        }
    }

    #[test]
    fn run_codes_follow_the_spec() {
        assert_eq!(run_code(&summary(0, 0, false)), 0);
        assert_eq!(run_code(&summary(1, 0, false)), 1);
        assert_eq!(run_code(&summary(0, 1, false)), 3);
        // 3 wins over 1.
        assert_eq!(run_code(&summary(1, 1, false)), 3);
        assert_eq!(run_code(&summary(1, 1, true)), 130);
    }

    #[test]
    fn usage_errors_exit_2() {
        let err = CliError::from(ConfigError::Usage("bad --var".into()));
        assert_eq!((err.code(), err.render().as_str()), (2, "error: bad --var"));
    }

    #[test]
    fn import_errors_exit_2() {
        let err = CliError::from(ImportError::CannotDetect("data.json".into()));
        assert_eq!(err.code(), 2);
        assert!(
            err.render()
                .starts_with("error: import: cannot detect format of data.json")
        );
    }

    #[test]
    fn curl_errors_exit_2() {
        let err = CliError::from(CurlError("no URL in the command".into()));
        assert_eq!(
            (err.code(), err.render().as_str()),
            (2, "error: curl: no URL in the command")
        );
    }
}
