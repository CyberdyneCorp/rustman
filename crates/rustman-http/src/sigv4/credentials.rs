//! SigV4 credential sources: explicit fields, `from: env`, and the static
//! keys of a `from: profile` profile in `~/.aws/credentials` (or
//! `~/.aws/config`). SSO, assume-role and `credential_process` profiles are
//! refused (runner-parity design D4).

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use rustman_core::model::AwsCredentials;

use crate::error::RequestError;

/// Static AWS keys.
#[derive(Clone, PartialEq, Eq)]
pub(crate) struct Keys {
    pub access_key: String,
    pub secret_key: String,
    pub session_token: Option<String>,
}

/// Never prints the secret key or the session token.
impl std::fmt::Debug for Keys {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Keys")
            .field("access_key", &self.access_key)
            .finish_non_exhaustive()
    }
}

/// Profile keys this signer cannot use, in the order they are reported.
const UNSUPPORTED_KEYS: &[&str] = &[
    "sso_start_url",
    "sso_session",
    "role_arn",
    "credential_process",
    "web_identity_token_file",
    "source_profile",
];

fn error(message: String) -> RequestError {
    RequestError::Auth(format!("aws_sigv4: {message}"))
}

/// The keys `source` names, reading variables with `env`.
pub(crate) fn resolve(
    source: &AwsCredentials,
    env: &dyn Fn(&str) -> Option<String>,
) -> Result<Keys, RequestError> {
    match source {
        AwsCredentials::Fields {
            access_key,
            secret_key,
            session_token,
        } => Ok(Keys {
            access_key: access_key.clone(),
            secret_key: secret_key.clone(),
            session_token: session_token.clone().filter(|t| !t.is_empty()),
        }),
        AwsCredentials::Env => from_env(env),
        AwsCredentials::Profile(name) => from_profile(name, env),
    }
}

fn from_env(env: &dyn Fn(&str) -> Option<String>) -> Result<Keys, RequestError> {
    let var = |name: &str| env(name).filter(|v| !v.is_empty());
    match (var("AWS_ACCESS_KEY_ID"), var("AWS_SECRET_ACCESS_KEY")) {
        (Some(access_key), Some(secret_key)) => Ok(Keys {
            access_key,
            secret_key,
            session_token: var("AWS_SESSION_TOKEN"),
        }),
        _ => Err(error(
            "from: env needs AWS_ACCESS_KEY_ID and AWS_SECRET_ACCESS_KEY".into(),
        )),
    }
}

fn from_profile(name: &str, env: &dyn Fn(&str) -> Option<String>) -> Result<Keys, RequestError> {
    let home = env("HOME")
        .or_else(|| env("USERPROFILE"))
        .map(PathBuf::from)
        .or_else(std::env::home_dir)
        .unwrap_or_default();
    let aws = home.join(".aws");
    let credentials =
        env("AWS_SHARED_CREDENTIALS_FILE").map_or(aws.join("credentials"), PathBuf::from);
    let config = env("AWS_CONFIG_FILE").map_or(aws.join("config"), PathBuf::from);
    let mut profile = section(&config, &config_section(name));
    let from_credentials = section(&credentials, name);
    if profile.is_none() && from_credentials.is_none() {
        return Err(error(format!(
            "profile '{name}' not found in {}",
            credentials.display()
        )));
    }
    profile
        .get_or_insert_default()
        .extend(from_credentials.unwrap_or_default());
    static_keys(name, &profile.unwrap_or_default())
}

/// The section name of profile `name` in `~/.aws/config`.
fn config_section(name: &str) -> String {
    if name == "default" {
        name.to_owned()
    } else {
        format!("profile {name}")
    }
}

fn static_keys(name: &str, profile: &HashMap<String, String>) -> Result<Keys, RequestError> {
    if let Some(key) = UNSUPPORTED_KEYS.iter().find(|k| profile.contains_key(**k)) {
        return Err(error(format!(
            "profile '{name}' uses {key}, which is not supported; export temporary credentials or use a secret provider"
        )));
    }
    let get = |key: &str| profile.get(key).filter(|v| !v.is_empty()).cloned();
    match (get("aws_access_key_id"), get("aws_secret_access_key")) {
        (Some(access_key), Some(secret_key)) => Ok(Keys {
            access_key,
            secret_key,
            session_token: get("aws_session_token"),
        }),
        _ => Err(error(format!(
            "profile '{name}' has no aws_access_key_id and aws_secret_access_key"
        ))),
    }
}

/// The `key = value` pairs of `[name]` in the INI file at `path`, or `None`
/// when the file or the section does not exist.
fn section(path: &Path, name: &str) -> Option<HashMap<String, String>> {
    let text = std::fs::read_to_string(path).ok()?;
    parse_section(&text, name)
}

fn parse_section(text: &str, name: &str) -> Option<HashMap<String, String>> {
    let mut found = None;
    let mut current = false;
    for line in text.lines().map(str::trim) {
        if line.is_empty() || line.starts_with(['#', ';']) {
            continue;
        }
        if let Some(header) = line.strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
            current = header.trim() == name;
            if current {
                found.get_or_insert_with(HashMap::new);
            }
            continue;
        }
        if let (true, Some((key, value))) = (current, line.split_once('=')) {
            let section = found.get_or_insert_with(HashMap::new);
            section.insert(key.trim().to_ascii_lowercase(), value.trim().to_owned());
        }
    }
    found
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env_of(vars: &[(&str, String)]) -> impl Fn(&str) -> Option<String> + use<> {
        let vars: HashMap<String, String> = vars
            .iter()
            .map(|(k, v)| ((*k).to_owned(), v.clone()))
            .collect();
        move |name| vars.get(name).cloned()
    }

    #[test]
    fn fields_are_used_as_given() {
        let source = AwsCredentials::Fields {
            access_key: "AK".into(),
            secret_key: "SK".into(),
            session_token: Some(String::new()),
        };
        let keys = resolve(&source, &|_| None).unwrap();
        assert_eq!((keys.access_key.as_str(), keys.session_token), ("AK", None));
    }

    #[test]
    fn env_reads_the_three_variables() {
        let env = env_of(&[
            ("AWS_ACCESS_KEY_ID", "AK".into()),
            ("AWS_SECRET_ACCESS_KEY", "SK".into()),
            ("AWS_SESSION_TOKEN", "ST".into()),
        ]);
        let keys = resolve(&AwsCredentials::Env, &env).unwrap();
        assert_eq!(keys.session_token.as_deref(), Some("ST"));
        let err = resolve(&AwsCredentials::Env, &|_| None).unwrap_err();
        assert_eq!(
            err.to_string(),
            "aws_sigv4: from: env needs AWS_ACCESS_KEY_ID and AWS_SECRET_ACCESS_KEY"
        );
    }

    fn home_with(credentials: &str, config: &str) -> tempfile::TempDir {
        let home = tempfile::tempdir().unwrap();
        std::fs::create_dir(home.path().join(".aws")).unwrap();
        std::fs::write(home.path().join(".aws/credentials"), credentials).unwrap();
        std::fs::write(home.path().join(".aws/config"), config).unwrap();
        home
    }

    fn profile(home: &tempfile::TempDir, name: &str) -> Result<Keys, RequestError> {
        let env = env_of(&[("HOME", home.path().display().to_string())]);
        resolve(&AwsCredentials::Profile(name.into()), &env)
    }

    #[test]
    fn static_profiles_come_from_the_credentials_file() {
        let home = home_with(
            "# keys\n[default]\naws_access_key_id = D\naws_secret_access_key = DS\n\n[dev]\r\naws_access_key_id=AK\r\naws_secret_access_key = SK\r\naws_session_token = ST\r\n",
            "[profile dev]\nregion = eu-west-1\n",
        );
        let keys = profile(&home, "dev").unwrap();
        assert_eq!(
            (keys.access_key.as_str(), keys.secret_key.as_str()),
            ("AK", "SK")
        );
        assert_eq!(keys.session_token.as_deref(), Some("ST"));
        assert_eq!(profile(&home, "default").unwrap().access_key, "D");
    }

    #[test]
    fn sso_and_role_profiles_are_refused() {
        let home = home_with(
            "",
            "[profile dev]\nsso_start_url = https://sso.example\nsso_region = us-east-1\n\n[profile ops]\nrole_arn = arn:aws:iam::1:role/x\nsource_profile = default\n",
        );
        assert_eq!(
            profile(&home, "dev").unwrap_err().to_string(),
            "aws_sigv4: profile 'dev' uses sso_start_url, which is not supported; export temporary credentials or use a secret provider"
        );
        assert!(
            profile(&home, "ops")
                .unwrap_err()
                .to_string()
                .contains("uses role_arn")
        );
    }

    #[test]
    fn missing_profiles_and_keys_are_reported() {
        let home = home_with("[partial]\naws_access_key_id = AK\n", "");
        let err = profile(&home, "nope").unwrap_err().to_string();
        assert!(
            err.starts_with("aws_sigv4: profile 'nope' not found in "),
            "{err}"
        );
        assert_eq!(
            profile(&home, "partial").unwrap_err().to_string(),
            "aws_sigv4: profile 'partial' has no aws_access_key_id and aws_secret_access_key"
        );
    }
}
