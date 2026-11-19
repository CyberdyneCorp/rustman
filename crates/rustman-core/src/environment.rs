//! Environments: `environments/<name>.yaml` files and active-environment
//! selection.

use std::fs;
use std::path::Path;

use serde::Deserialize;

use crate::error::{ConfigError, LoadError};
use crate::format::{Located, from_yaml};
use crate::model::Variables;
use crate::paths;
use crate::trace::TraceConfig;
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
    /// `production: true`: the safety policy's production guard applies.
    pub production: bool,
    /// `trace:`: trace propagation, over `rustman.toml [trace]`.
    pub trace: TraceConfig,
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
    #[serde(default)]
    production: bool,
    #[serde(default)]
    trace: Option<Located<TraceConfig>>,
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
        // Follows symlinks, but only to files inside the workspace.
        if !dir.join(&file_name).is_file() {
            loaded.warnings.push(format!(
                "ignoring environment '{stem}': not a readable file"
            ));
            continue;
        }
        if paths::contain(root, &dir, &file_name).is_err() {
            loaded.warnings.push(format!(
                "ignoring environment '{stem}': resolves outside the workspace"
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
    if let Some(trace) = &parsed.trace {
        trace
            .validate()
            .map_err(|message| LoadError::new(file, trace.position(), message))?;
    }
    Ok(Environment {
        name: name.to_owned(),
        variables: parsed.variables,
        secrets: parsed
            .secrets
            .into_iter()
            .map(Located::into_inner)
            .collect(),
        production: parsed.production,
        trace: parsed.trace.map(Located::into_inner).unwrap_or_default(),
    })
}

/// Check each environment's `trace:` block over `rustman.toml [trace]`
/// (they merge key by key, so a preset's `link_base` may come from either
/// file). Without environments, `[trace]` is checked on its own.
pub(crate) fn check_merged_trace(
    settings: &TraceConfig,
    environments: &[Environment],
) -> Vec<LoadError> {
    if environments.is_empty() {
        return settings
            .check_merged()
            .err()
            .map(|message| LoadError::new(Path::new(crate::settings::SETTINGS_FILE), None, message))
            .into_iter()
            .collect();
    }
    environments
        .iter()
        .filter_map(|env| {
            let message = env.trace.over(settings).check_merged().err()?;
            let file = Path::new(ENVIRONMENTS_DIR).join(format!("{}{SUFFIX}", env.name));
            Some(LoadError::new(&file, None, message))
        })
        .collect()
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

    #[cfg(unix)]
    #[test]
    fn symlinks_resolving_outside_the_workspace_are_skipped() {
        use std::os::unix::fs::symlink;
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("ws");
        let dir = root.join(ENVIRONMENTS_DIR);
        fs::create_dir_all(&dir).unwrap();
        fs::write(tmp.path().join("elsewhere.yaml"), "variables:\n  a: 1\n").unwrap();
        symlink(tmp.path().join("elsewhere.yaml"), dir.join("out.yaml")).unwrap();
        symlink("../../elsewhere.yaml", dir.join("relative.yaml")).unwrap();
        fs::write(dir.join("local.yaml"), "").unwrap();
        let loaded = load_all(&root);
        assert!(loaded.errors.is_empty(), "{:?}", loaded.errors);
        let names: Vec<_> = loaded
            .environments
            .iter()
            .map(|e| e.name.as_str())
            .collect();
        assert_eq!(names, ["local"]);
        assert_eq!(
            loaded.warnings,
            [
                "ignoring environment 'out': resolves outside the workspace",
                "ignoring environment 'relative': resolves outside the workspace",
            ]
        );
    }

    #[test]
    fn production_marker_defaults_to_false() {
        assert!(!env("variables:\n  a: 1\n").unwrap().production);
        assert!(env("production: true\n").unwrap().production);
        assert!(!env("production: false\n").unwrap().production);
        assert!(env("production: yes\n").is_err());
        let prod = env("production: true\n").unwrap();
        let info = crate::safety::EnvironmentInfo::from(&prod);
        assert_eq!(info.name, Some("local"));
        assert!(info.marked_production);
    }

    #[test]
    fn the_trace_block_is_read_and_validated() {
        let e = env("trace:\n  enabled: true\n  scope: run\n  formats: [w3c, b3multi]\n  baggage:\n    tenant: \"{{tenant_id}}\"\n  link: jaeger\n  link_base: http://localhost:16686\n").unwrap();
        assert!(e.trace.is_enabled());
        assert_eq!(e.trace.scope, Some(crate::trace::TraceScope::Run));
        assert_eq!(
            e.trace.formats,
            Some(vec![
                crate::trace::PropagationFormat::W3c,
                crate::trace::PropagationFormat::B3Multi
            ])
        );
        assert_eq!(e.trace.baggage.unwrap()["tenant"], "{{tenant_id}}");
        assert_eq!(env("").unwrap().trace, TraceConfig::default());
        let err = env("variables: {}\ntrace:\n  enabled: true\n  link: \"https://x/{traceid}\"\n")
            .unwrap_err();
        assert_eq!(
            err.to_string(),
            "environments/local.yaml:3: trace.link: unknown placeholder {traceid}"
        );
        assert!(env("trace:\n  formats: [zipkin]\n").is_err());
        assert!(env("trace:\n  sample: true\n").is_err());
    }

    #[test]
    fn a_preset_link_base_may_come_from_either_file() {
        let preset = TraceConfig {
            link: Some("jaeger".into()),
            ..TraceConfig::default()
        };
        let base = TraceConfig {
            link_base: Some("http://localhost:16686".into()),
            ..TraceConfig::default()
        };
        let with = |trace: &TraceConfig| Environment {
            name: "dev".into(),
            trace: trace.clone(),
            ..Environment::default()
        };
        assert!(check_merged_trace(&preset, &[with(&base)]).is_empty());
        assert!(check_merged_trace(&base, &[with(&preset)]).is_empty());
        assert!(check_merged_trace(&TraceConfig::default(), &[]).is_empty());
        let errors = check_merged_trace(&preset, &[with(&base), with(&TraceConfig::default())]);
        assert_eq!(
            errors.iter().map(ToString::to_string).collect::<Vec<_>>(),
            [format!(
                "{}: trace.link: preset 'jaeger' needs trace.link_base",
                Path::new("environments").join("dev.yaml").display()
            )]
        );
        let alone = check_merged_trace(&preset, &[]);
        assert_eq!(
            alone[0].to_string(),
            "rustman.toml: trace.link: preset 'jaeger' needs trace.link_base"
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
