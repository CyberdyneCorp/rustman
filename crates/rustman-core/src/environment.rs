//! Environments: `environments/<name>.yaml` files and active-environment
//! selection.

use std::fs;
use std::path::Path;

use serde::Deserialize;

use crate::error::{ConfigError, LoadError};
use crate::format::{Located, from_yaml};
use crate::model::Variables;
use crate::vars::template::is_variable_name;

/// The folder, under the workspace root, holding one file per environment.
pub const ENVIRONMENTS_DIR: &str = "environments";

/// The OS environment variable that selects the active environment.
pub const ENV_VAR: &str = "RUSTMAN_ENV";

const SUFFIX: &str = ".yaml";

/// One loaded environment.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Environment {
    /// The file stem, restricted to `[A-Za-z0-9_-]+`.
    pub name: String,
    pub variables: Variables,
    /// Variables whose values live outside the workspace (keychain or
    /// `.rustman/secrets.yaml`).
    pub secrets: Vec<String>,
}

impl Environment {
    pub fn is_secret(&self, name: &str) -> bool {
        self.secrets.iter().any(|s| s == name)
    }
}

/// The contents of an environment file.
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct EnvironmentFile {
    #[serde(default)]
    variables: Variables,
    #[serde(default)]
    secrets: Vec<Located<String>>,
}

/// Whether `name` is a valid environment name: `[A-Za-z0-9_-]+`.
pub fn is_valid_name(name: &str) -> bool {
    !name.is_empty()
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

/// Environments found under a workspace, sorted by name, plus the problems
/// found while loading them.
#[derive(Debug, Default)]
pub(crate) struct Loaded {
    pub environments: Vec<Environment>,
    pub errors: Vec<LoadError>,
    pub warnings: Vec<String>,
}

/// Load every `environments/*.yaml` file under `root`. A missing folder means
/// no environments.
pub(crate) fn load_all(root: &Path) -> Loaded {
    let mut loaded = Loaded::default();
    let dir = root.join(ENVIRONMENTS_DIR);
    let Ok(entries) = fs::read_dir(&dir) else {
        return loaded;
    };
    let mut names: Vec<String> = entries
        .filter_map(Result::ok)
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|name| name.ends_with(SUFFIX))
        .collect();
    names.sort();
    for file_name in names {
        let Some(stem) = file_name.strip_suffix(SUFFIX) else {
            continue;
        };
        // Follows symlinks, like request files do.
        if !dir.join(&file_name).is_file() {
            loaded.warnings.push(format!(
                "ignoring environment '{stem}': not a readable file"
            ));
            continue;
        }
        if !is_valid_name(stem) {
            loaded
                .warnings
                .push(format!("ignoring environment '{stem}': invalid name"));
            continue;
        }
        let rel = Path::new(ENVIRONMENTS_DIR).join(&file_name);
        match load_one(root, &rel, stem) {
            Ok(env) => loaded.environments.push(env),
            Err(err) => loaded.errors.push(err),
        }
    }
    loaded
}

fn load_one(root: &Path, rel: &Path, name: &str) -> Result<Environment, LoadError> {
    let text =
        fs::read_to_string(root.join(rel)).map_err(|e| LoadError::new(rel, None, e.to_string()))?;
    parse(rel, name, &text)
}

/// Parse the text of the environment file `file` named `name`.
pub fn parse(file: &Path, name: &str, text: &str) -> Result<Environment, LoadError> {
    let parsed: EnvironmentFile = if text.trim().is_empty() {
        EnvironmentFile::default()
    } else {
        from_yaml(file, text)?
    };
    for secret in &parsed.secrets {
        if !is_variable_name(secret) || secret.starts_with('$') {
            return Err(LoadError::new(
                file,
                secret.position(),
                format!(
                    "invalid secret name '{}' (a template cannot reference it)",
                    **secret
                ),
            ));
        }
        if parsed.variables.get(secret).is_some() {
            return Err(LoadError::new(
                file,
                secret.position(),
                format!(
                    "secret '{}' must not have a value under 'variables'",
                    **secret
                ),
            ));
        }
    }
    Ok(Environment {
        name: name.to_owned(),
        variables: parsed.variables,
        secrets: parsed
            .secrets
            .into_iter()
            .map(Located::into_inner)
            .collect(),
    })
}

/// The name of the active environment: `--env`, else `RUSTMAN_ENV`, else the
/// workspace default, else none. Empty values count as unset.
pub fn active_name<'a>(
    flag: Option<&'a str>,
    env_var: Option<&'a str>,
    default: Option<&'a str>,
) -> Option<&'a str> {
    [flag, env_var, default]
        .into_iter()
        .flatten()
        .find(|name| !name.is_empty())
}

/// Find `name` among `environments`, or fail with the not-found error.
pub fn find<'a>(
    environments: &'a [Environment],
    name: &str,
) -> Result<&'a Environment, ConfigError> {
    environments
        .iter()
        .find(|e| e.name == name)
        .ok_or_else(|| ConfigError::EnvironmentNotFound {
            name: name.to_owned(),
            available: environments.iter().map(|e| e.name.clone()).collect(),
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::Position;
    use crate::model::VarValue;

    fn env(text: &str) -> Result<Environment, LoadError> {
        parse(Path::new("environments/local.yaml"), "local", text)
    }

    #[test]
    fn names_are_restricted() {
        for ok in ["local", "prod-eu_1", "A"] {
            assert!(is_valid_name(ok), "{ok}");
        }
        for bad in ["", "my env", "a.b", "é", "a/b"] {
            assert!(!is_valid_name(bad), "{bad}");
        }
    }

    #[test]
    fn variables_and_secrets_are_read() {
        let e = env(
            "variables:\n  base_url: http://x\n  port: 0808\n  debug: no\nsecrets: [api_token]\n",
        )
        .unwrap();
        assert_eq!(e.name, "local");
        assert_eq!(
            e.variables.get("base_url"),
            Some(&VarValue::String("http://x".into()))
        );
        assert_eq!(
            e.variables.get("port"),
            Some(&VarValue::String("0808".into()))
        );
        assert_eq!(
            e.variables.get("debug"),
            Some(&VarValue::String("no".into()))
        );
        assert!(e.is_secret("api_token"));
        assert!(!e.is_secret("base_url"));
    }

    #[test]
    fn empty_file_is_an_empty_environment() {
        let e = env("").unwrap();
        assert!(e.variables.is_empty() && e.secrets.is_empty());
    }

    #[test]
    fn unknown_key_has_exact_position() {
        let err = env("variables: {}\nvars: {}\n").unwrap_err();
        assert_eq!(err.position, Some(Position { line: 2, column: 1 }));
        assert!(err.message.starts_with("unknown field `vars`"), "{err}");
    }

    #[test]
    fn nested_values_are_rejected() {
        let err = env("variables:\n  a:\n    b: 1\n").unwrap_err();
        assert_eq!(err.line(), Some(3), "{err}");
    }

    #[test]
    fn secret_names_must_be_referenceable() {
        for bad in ["'my token'", "'{x}'", "'$uuid'", "''"] {
            let err = env(&format!("secrets:\n  - {bad}\n")).unwrap_err();
            assert_eq!(err.position, Some(Position { line: 2, column: 5 }), "{bad}");
            assert!(err.to_string().contains("invalid secret name"), "{err}");
        }
        assert!(env("secrets: [api_token, db.password, a-b]\n").is_ok());
    }

    #[test]
    fn secret_with_a_value_is_rejected_at_the_secret() {
        let err = env("variables:\n  token: abc\nsecrets:\n  - token\n").unwrap_err();
        assert_eq!(err.position, Some(Position { line: 4, column: 5 }));
        assert_eq!(
            err.to_string(),
            "environments/local.yaml:4: secret 'token' must not have a value under 'variables'"
        );
    }

    #[test]
    fn flag_beats_env_var_beats_default() {
        assert_eq!(
            active_name(Some("prod"), Some("staging"), Some("local")),
            Some("prod")
        );
        assert_eq!(
            active_name(None, Some("staging"), Some("local")),
            Some("staging")
        );
        assert_eq!(active_name(None, None, Some("local")), Some("local"));
        assert_eq!(active_name(None, Some(""), Some("local")), Some("local"));
        assert_eq!(active_name(None, None, None), None);
    }

    #[cfg(unix)]
    #[test]
    fn symlinked_files_are_followed_and_dangling_ones_warn() {
        use std::os::unix::fs::symlink;
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join(ENVIRONMENTS_DIR);
        fs::create_dir_all(&dir).unwrap();
        fs::write(tmp.path().join("shared.yaml"), "variables:\n  a: 1\n").unwrap();
        symlink(tmp.path().join("shared.yaml"), dir.join("linked.yaml")).unwrap();
        symlink(tmp.path().join("gone.yaml"), dir.join("dangling.yaml")).unwrap();
        let loaded = load_all(tmp.path());
        assert!(loaded.errors.is_empty(), "{:?}", loaded.errors);
        let names: Vec<_> = loaded
            .environments
            .iter()
            .map(|e| e.name.as_str())
            .collect();
        assert_eq!(names, ["linked"]);
        assert_eq!(
            loaded.warnings,
            ["ignoring environment 'dangling': not a readable file"]
        );
    }

    #[test]
    fn missing_environment_lists_the_available_ones() {
        let envs = [
            Environment {
                name: "local".into(),
                ..Environment::default()
            },
            Environment {
                name: "staging".into(),
                ..Environment::default()
            },
        ];
        assert_eq!(find(&envs, "staging").unwrap().name, "staging");
        let err = find(&envs, "qa").unwrap_err();
        assert_eq!(
            err.to_string(),
            "environment 'qa' not found (available: local, staging)"
        );
        assert_eq!(
            find(&[], "qa").unwrap_err().to_string(),
            "environment 'qa' not found (available: none)"
        );
    }
}
