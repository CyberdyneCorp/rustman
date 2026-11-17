//! Mapping errors to messages and exit codes (design D12, ROADMAP exit codes).

use rustman_core::ConfigError;

/// Usage or configuration error: bad flag, invalid file, unsupported version.
pub const CONFIG: u8 = 2;

/// The exit code for an error.
pub fn code(err: &ConfigError) -> u8 {
    match err {
        ConfigError::NoWorkspace
        | ConfigError::AlreadyWorkspace(_)
        | ConfigError::NewerSettingsVersion(_)
        | ConfigError::Invalid(_)
        | ConfigError::EnvironmentNotFound { .. }
        | ConfigError::NoActiveEnvironment
        | ConfigError::Secrets(_)
        | ConfigError::Io { .. } => CONFIG,
    }
}

/// The text printed on stderr for an error: every line (one per invalid
/// file, for instance) gets the `error:` prefix.
pub fn render(err: &ConfigError) -> String {
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
}
