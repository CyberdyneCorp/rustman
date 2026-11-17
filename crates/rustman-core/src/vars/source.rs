//! Variable sources and their precedence (environments spec, ROADMAP "Variable").

use std::borrow::Cow;

use crate::environment::Environment;
use crate::model::{VarValue, Variables};
use crate::secret::Secret;

/// A layer of the precedence chain, highest precedence first. A resolver
/// always consults layers in this order, whatever order sources were added in.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Layer {
    /// `--var k=v`.
    Override,
    /// Captures from earlier requests in the same run.
    Capture,
    /// The current row of a `--data` file.
    DataRow,
    /// The request file's `variables`.
    Request,
    /// `_collection.yaml` `variables`, nearest folder first.
    Collection,
    /// Captures persisted in `.rustman/state/<env>.yaml`.
    PersistedCapture,
    /// The active environment.
    Environment,
    /// `rustman.toml [variables]`.
    Settings,
}

/// The result of looking a name up in one source.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Lookup {
    Value(String),
    /// A value that came from the secret store.
    Secret(Secret<String>),
    /// The name is a secret of this source, but no value is stored or the
    /// store could not be read. Resolution stops here and reports the hint.
    SecretNotSet {
        hint: String,
    },
}

/// One source of variables in the precedence chain.
pub trait VarSource {
    fn layer(&self) -> Layer;
    /// `None` when this source does not define `name`.
    fn lookup(&self, name: &str) -> Option<Lookup>;
}

/// A plain set of variables at a given layer.
#[derive(Clone, Debug)]
pub struct VariablesSource<'a> {
    layer: Layer,
    vars: Cow<'a, Variables>,
}

impl<'a> VariablesSource<'a> {
    pub fn new(layer: Layer, vars: &'a Variables) -> Self {
        Self {
            layer,
            vars: Cow::Borrowed(vars),
        }
    }

    pub fn owned(layer: Layer, vars: Variables) -> VariablesSource<'static> {
        VariablesSource {
            layer,
            vars: Cow::Owned(vars),
        }
    }
}

impl VarSource for VariablesSource<'_> {
    fn layer(&self) -> Layer {
        self.layer
    }

    fn lookup(&self, name: &str) -> Option<Lookup> {
        self.vars.get(name).map(|v| Lookup::Value(v.to_string()))
    }
}

/// Where secret values are read from. The keychain and file stores implement it.
pub trait SecretLookup {
    /// The stored value of secret `name` in environment `env`, `None` when
    /// nothing is stored. `Err` carries why the store could not be read; it
    /// never contains a value.
    fn secret(&self, env: &str, name: &str) -> Result<Option<String>, String>;
}

/// A secret store that holds nothing: every secret is "not set".
#[derive(Clone, Copy, Debug, Default)]
pub struct NoSecrets;

impl SecretLookup for NoSecrets {
    fn secret(&self, _env: &str, _name: &str) -> Result<Option<String>, String> {
        Ok(None)
    }
}

/// The active environment, with its secrets read from a store.
pub struct EnvironmentSource<'a> {
    env: &'a Environment,
    secrets: &'a dyn SecretLookup,
}

impl<'a> EnvironmentSource<'a> {
    pub fn new(env: &'a Environment, secrets: &'a dyn SecretLookup) -> Self {
        Self { env, secrets }
    }
}

impl VarSource for EnvironmentSource<'_> {
    fn layer(&self) -> Layer {
        Layer::Environment
    }

    fn lookup(&self, name: &str) -> Option<Lookup> {
        if !self.env.is_secret(name) {
            return self
                .env
                .variables
                .get(name)
                .map(|v| Lookup::Value(v.to_string()));
        }
        let env = &self.env.name;
        Some(match self.secrets.secret(env, name) {
            Ok(Some(value)) => Lookup::Secret(Secret::new(name, value)),
            Ok(None) => Lookup::SecretNotSet {
                hint: format!("secret not set; run 'rustman secret set {name} --env {env}'"),
            },
            Err(err) => Lookup::SecretNotSet {
                hint: format!("could not read secret: {err}"),
            },
        })
    }
}

/// Parse repeated `--var NAME=VALUE` arguments. Values are strings; a later
/// occurrence of a name replaces an earlier one.
pub fn parse_overrides<S: AsRef<str>>(args: &[S]) -> Result<Variables, String> {
    let mut entries: Vec<(String, VarValue)> = Vec::new();
    for arg in args {
        let arg = arg.as_ref();
        let (name, value) = arg
            .split_once('=')
            .filter(|(name, _)| !name.is_empty())
            .ok_or_else(|| format!("invalid --var '{arg}': expected NAME=VALUE"))?;
        entries.retain(|(n, _)| n != name);
        entries.push((name.to_owned(), VarValue::String(value.to_owned())));
    }
    Ok(Variables::new(entries))
}

#[cfg(test)]
mod tests {
    use super::*;

    struct OneSecret;

    impl SecretLookup for OneSecret {
        fn secret(&self, env: &str, name: &str) -> Result<Option<String>, String> {
            if name == "broken" {
                return Err("OS keychain error: denied".to_owned());
            }
            Ok((env == "prod" && name == "api_token").then(|| "s3cr3t".to_owned()))
        }
    }

    fn prod() -> Environment {
        Environment {
            name: "prod".into(),
            variables: Variables::new(vec![("n".into(), VarValue::Integer(5))]),
            secrets: vec!["api_token".into(), "other".into(), "broken".into()],
        }
    }

    #[test]
    fn layers_are_ordered_by_precedence() {
        let mut layers = [
            Layer::Settings,
            Layer::Environment,
            Layer::PersistedCapture,
            Layer::Collection,
            Layer::Request,
            Layer::DataRow,
            Layer::Capture,
            Layer::Override,
        ];
        layers.sort();
        assert_eq!(layers[0], Layer::Override);
        assert_eq!(layers[7], Layer::Settings);
        assert!(Layer::Capture < Layer::DataRow && Layer::DataRow < Layer::Request);
        assert!(Layer::PersistedCapture < Layer::Environment);
    }

    #[test]
    fn environment_values_are_rendered_as_text() {
        let env = prod();
        let source = EnvironmentSource::new(&env, &NoSecrets);
        assert_eq!(source.lookup("n"), Some(Lookup::Value("5".into())));
        assert_eq!(source.lookup("missing"), None);
    }

    #[test]
    fn secrets_come_from_the_store() {
        let env = prod();
        let source = EnvironmentSource::new(&env, &OneSecret);
        assert_eq!(
            source.lookup("api_token"),
            Some(Lookup::Secret(Secret::new("api_token", "s3cr3t".into())))
        );
        assert_eq!(
            source.lookup("other"),
            Some(Lookup::SecretNotSet {
                hint: "secret not set; run 'rustman secret set other --env prod'".into()
            })
        );
    }

    #[test]
    fn unreadable_secrets_report_the_store_error() {
        let env = prod();
        let source = EnvironmentSource::new(&env, &OneSecret);
        assert_eq!(
            source.lookup("broken"),
            Some(Lookup::SecretNotSet {
                hint: "could not read secret: OS keychain error: denied".into()
            })
        );
    }

    #[test]
    fn overrides_parse_name_value_pairs() {
        let vars = parse_overrides(&["a=1", "b=x=y", "a=2", "e="]).unwrap();
        let pairs: Vec<_> = vars.iter().map(|(n, v)| (n, v.to_string())).collect();
        assert_eq!(
            pairs,
            [("b", "x=y".into()), ("a", "2".into()), ("e", String::new())]
        );
        assert_eq!(
            parse_overrides(&["novalue"]).unwrap_err(),
            "invalid --var 'novalue': expected NAME=VALUE"
        );
        assert!(parse_overrides(&["=v"]).is_err());
    }
}
