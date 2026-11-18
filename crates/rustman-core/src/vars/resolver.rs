//! Resolving templates through the precedence chain (design D8).

use std::fmt;
use std::ops::Range;

use super::dynamic::{OsEnv, process_env};
use super::source::{EnvironmentSource, Layer, Lookup, SecretLookup, VarSource, VariablesSource};
use super::template::{Segment, Template, TemplateError};
use crate::environment::Environment;
use crate::redact::Redactor;
use crate::secret::Secret;
use crate::settings::Settings;
use crate::workspace::RequestRef;

/// A placeholder that could not be resolved.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Missing {
    /// The variable name, or the dynamic placeholder such as `$env API_KEY`.
    pub name: String,
    /// Extra advice, e.g. how to set a missing secret.
    pub hint: Option<String>,
}

impl fmt::Display for Missing {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.hint {
            Some(hint) => write!(f, "{} ({hint})", self.name),
            None => f.write_str(&self.name),
        }
    }
}

/// Why a template or request could not be resolved. Nothing may be sent.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum ResolveError {
    #[error(transparent)]
    Template(#[from] TemplateError),
    /// Every missing name, in order of first use, without duplicates.
    #[error("unresolved variable(s): {}", join(.0))]
    Unresolved(Vec<Missing>),
    #[error("{0}")]
    Unsupported(String),
    /// A file path that, once interpolated, leaves the workspace.
    #[error(transparent)]
    PathEscape(#[from] crate::paths::PathEscape),
}

fn join(missing: &[Missing]) -> String {
    missing
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join(", ")
}

/// A byte range of resolved text whose value came from a secret.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SecretSpan {
    pub name: String,
    pub range: Range<usize>,
}

/// Resolved text plus where secrets were substituted into it.
///
/// Its `Debug` shows the secret spans redacted.
#[derive(Clone, Default, PartialEq, Eq)]
pub struct Resolved {
    pub text: String,
    pub secret_spans: Vec<SecretSpan>,
}

impl Resolved {
    /// Text that came from no variable at all.
    pub fn literal(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            secret_spans: Vec::new(),
        }
    }
}

impl fmt::Debug for Resolved {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Resolved")
            .field("text", &Redactor::new().resolved(self))
            .field("secret_spans", &self.secret_spans)
            .finish()
    }
}

/// A secret value used while resolving, kept so output sinks can scan for it.
pub type UsedSecret = Secret<String>;

/// The precedence chain: sources ordered by [`Layer`], then by insertion.
pub struct Resolver<'a> {
    sources: Vec<Box<dyn VarSource + 'a>>,
    os_env: OsEnv,
}

impl Default for Resolver<'_> {
    fn default() -> Self {
        Self::new()
    }
}

impl<'a> Resolver<'a> {
    /// An empty chain that reads `{{$env NAME}}` from the process environment.
    pub fn new() -> Self {
        Self {
            sources: Vec::new(),
            os_env: process_env,
        }
    }

    /// The chain for one request: request `variables`, collection `variables`
    /// (nearest folder first), the active environment and `rustman.toml`.
    /// Overrides, captures, data rows and persisted captures are added with
    /// [`Resolver::push`].
    pub fn for_request(
        settings: &'a Settings,
        request: &RequestRef<'a>,
        environment: Option<&'a Environment>,
        secrets: &'a dyn SecretLookup,
    ) -> Self {
        let mut resolver = Self::new();
        if let Some(vars) = request.request().variables() {
            resolver.push(VariablesSource::new(Layer::Request, vars));
        }
        for vars in request.collection_variables() {
            resolver.push(VariablesSource::new(Layer::Collection, vars));
        }
        if let Some(env) = environment {
            resolver.push(EnvironmentSource::new(env, secrets));
        }
        resolver.push(VariablesSource::new(Layer::Settings, &settings.variables));
        resolver
    }

    /// Read `{{$env NAME}}` through `os_env` instead of the process environment.
    pub fn with_os_env(mut self, os_env: OsEnv) -> Self {
        self.os_env = os_env;
        self
    }

    /// Add a source after every source of the same or a higher-precedence layer.
    pub fn push(&mut self, source: impl VarSource + 'a) {
        let layer = source.layer();
        let index = self.sources.partition_point(|s| s.layer() <= layer);
        self.sources.insert(index, Box::new(source));
    }

    /// Builder form of [`Resolver::push`].
    pub fn with(mut self, source: impl VarSource + 'a) -> Self {
        self.push(source);
        self
    }

    /// The value of `name` from the highest-precedence source defining it.
    pub fn lookup(&self, name: &str) -> Option<Lookup> {
        self.lookup_with_layer(name).map(|(_, lookup)| lookup)
    }

    /// [`Resolver::lookup`], with the layer the value came from.
    pub fn lookup_with_layer(&self, name: &str) -> Option<(Layer, Lookup)> {
        self.sources
            .iter()
            .find_map(|s| s.lookup(name).map(|found| (s.layer(), found)))
    }

    /// Resolve one string.
    pub fn resolve(&self, input: &str) -> Result<Resolved, ResolveError> {
        let mut interpolator = self.interpolator();
        let resolved = interpolator.text(input);
        interpolator.finish(resolved).map(|(resolved, _)| resolved)
    }

    /// Resolve many strings, collecting every problem before failing.
    pub fn interpolator(&self) -> Interpolator<'_, 'a> {
        Interpolator {
            resolver: self,
            missing: Vec::new(),
            invalid: None,
            secrets: Vec::new(),
        }
    }
}

/// Resolves the strings of one request and gathers every missing name, so
/// the error lists all of them.
pub struct Interpolator<'r, 'a> {
    resolver: &'r Resolver<'a>,
    missing: Vec<Missing>,
    invalid: Option<TemplateError>,
    secrets: Vec<UsedSecret>,
}

impl Interpolator<'_, '_> {
    /// Resolve `input`. Problems are recorded and reported by [`Self::finish`];
    /// the returned text is then incomplete and must not be used.
    pub fn text(&mut self, input: &str) -> Resolved {
        match Template::parse(input) {
            Ok(template) => self.template(&template),
            Err(err) => {
                self.invalid.get_or_insert(err);
                Resolved::default()
            }
        }
    }

    /// Resolve a parsed template. Values are inserted as they are: a value
    /// containing `{{x}}` is not expanded again.
    pub fn template(&mut self, template: &Template) -> Resolved {
        let mut out = Resolved::default();
        for segment in template.segments() {
            match segment {
                Segment::Literal(text) => out.text.push_str(text),
                Segment::Var(name) => self.variable(name, &mut out),
                Segment::Dynamic(dynamic) => match dynamic.evaluate(self.resolver.os_env) {
                    Some(value) => out.text.push_str(&value),
                    None => self.miss(dynamic.to_string(), None),
                },
            }
        }
        out
    }

    fn variable(&mut self, name: &str, out: &mut Resolved) {
        match self.resolver.lookup(name) {
            Some(Lookup::Value(value)) => out.text.push_str(&value),
            Some(Lookup::Secret(secret)) => {
                let start = out.text.len();
                out.text.push_str(secret.expose());
                out.secret_spans.push(SecretSpan {
                    name: name.to_owned(),
                    range: start..out.text.len(),
                });
                self.used_secret(secret);
            }
            Some(Lookup::SecretNotSet { hint }) => self.miss(name.to_owned(), Some(hint)),
            None => self.miss(name.to_owned(), None),
        }
    }

    fn used_secret(&mut self, secret: Secret<String>) {
        if !self.secrets.iter().any(|s| s.name() == secret.name()) {
            self.secrets.push(secret);
        }
    }

    fn miss(&mut self, name: String, hint: Option<String>) {
        if !self.missing.iter().any(|m| m.name == name) {
            self.missing.push(Missing { name, hint });
        }
    }

    /// `value` with the secrets used to build it, or the first template error,
    /// or every unresolved name.
    pub fn finish<T>(self, value: T) -> Result<(T, Vec<UsedSecret>), ResolveError> {
        if let Some(err) = self.invalid {
            return Err(err.into());
        }
        if !self.missing.is_empty() {
            return Err(ResolveError::Unresolved(self.missing));
        }
        Ok((value, self.secrets))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{VarValue, Variables};
    use crate::vars::source::NoSecrets;

    fn vars(pairs: &[(&str, &str)]) -> Variables {
        Variables::new(
            pairs
                .iter()
                .map(|(n, v)| ((*n).to_owned(), VarValue::String((*v).to_owned())))
                .collect(),
        )
    }

    fn at(layer: Layer, pairs: &[(&str, &str)]) -> VariablesSource<'static> {
        VariablesSource::owned(layer, vars(pairs))
    }

    fn text(resolver: &Resolver<'_>, input: &str) -> String {
        resolver.resolve(input).unwrap().text
    }

    #[test]
    fn every_layer_beats_the_ones_below_it() {
        let layers = [
            Layer::Override,
            Layer::Capture,
            Layer::DataRow,
            Layer::Request,
            Layer::Collection,
            Layer::PersistedCapture,
            Layer::Environment,
            Layer::Settings,
        ];
        for (i, winner) in layers.iter().enumerate() {
            // Only the winner and the layers below it define `x`.
            let mut resolver = Resolver::new();
            for layer in layers[i..].iter().rev() {
                resolver.push(at(*layer, &[("x", &format!("{layer:?}"))]));
            }
            assert_eq!(text(&resolver, "{{x}}"), format!("{winner:?}"), "step {i}");
        }
    }

    #[test]
    fn insertion_order_does_not_change_precedence() {
        let resolver = Resolver::new()
            .with(at(Layer::Settings, &[("x", "settings")]))
            .with(at(Layer::Override, &[("x", "override")]))
            .with(at(Layer::Environment, &[("x", "env")]));
        assert_eq!(text(&resolver, "{{x}}"), "override");
    }

    #[test]
    fn same_layer_keeps_insertion_order() {
        let resolver = Resolver::new()
            .with(at(Layer::Collection, &[("x", "nearest")]))
            .with(at(Layer::Collection, &[("x", "root"), ("y", "root")]));
        assert_eq!(text(&resolver, "{{x}}-{{y}}"), "nearest-root");
    }

    #[test]
    fn override_beats_environment() {
        let env = Environment {
            name: "local".into(),
            variables: vars(&[("base_url", "http://localhost:8080")]),
            secrets: Vec::new(),
            production: false,
        };
        let resolver = Resolver::new()
            .with(EnvironmentSource::new(&env, &NoSecrets))
            .with(at(
                Layer::Override,
                &[("base_url", "http://127.0.0.1:9000")],
            ));
        assert_eq!(
            text(&resolver, "{{base_url}}/users"),
            "http://127.0.0.1:9000/users"
        );
    }

    #[test]
    fn capture_beats_environment() {
        let env = Environment {
            name: "local".into(),
            variables: vars(&[("token", "old")]),
            secrets: Vec::new(),
            production: false,
        };
        let resolver = Resolver::new()
            .with(EnvironmentSource::new(&env, &NoSecrets))
            .with(at(Layer::Capture, &[("token", "new")]));
        assert_eq!(text(&resolver, "{{token}}"), "new");
    }

    #[test]
    fn names_are_case_sensitive() {
        let resolver = Resolver::new().with(at(Layer::Settings, &[("Token", "a")]));
        assert_eq!(text(&resolver, "{{Token}}"), "a");
        assert!(resolver.resolve("{{token}}").is_err());
    }

    #[test]
    fn escaped_braces_stay_literal() {
        let resolver = Resolver::new();
        assert_eq!(text(&resolver, "\\{{not_a_var}}"), "{{not_a_var}}");
    }

    #[test]
    fn values_are_not_expanded_again() {
        let resolver = Resolver::new().with(at(Layer::Settings, &[("a", "{{b}}"), ("b", "x")]));
        assert_eq!(text(&resolver, "{{a}}"), "{{b}}");
    }

    #[test]
    fn every_missing_name_is_listed_once() {
        let resolver = Resolver::new().with(at(Layer::Settings, &[("known", "k")]));
        let mut i = resolver.interpolator();
        i.text("{{base_url}}/{{known}}/{{token}}");
        i.text("{{base_url}}?{{$env RUSTMAN_TEST_SURELY_UNSET_VAR}}");
        let err = i.finish(()).unwrap_err();
        assert_eq!(
            err.to_string(),
            "unresolved variable(s): base_url, token, $env RUSTMAN_TEST_SURELY_UNSET_VAR"
        );
    }

    #[test]
    fn missing_os_variable_names_the_placeholder() {
        let resolver = Resolver::new().with_os_env(|_| None);
        assert_eq!(
            resolver
                .resolve("{{$env API_KEY}}")
                .unwrap_err()
                .to_string(),
            "unresolved variable(s): $env API_KEY"
        );
        let resolver = Resolver::new().with_os_env(|n| (n == "API_KEY").then(|| "k".into()));
        assert_eq!(text(&resolver, "key={{$env API_KEY}}"), "key=k");
    }

    #[test]
    fn template_errors_win_over_missing_names() {
        let resolver = Resolver::new();
        let mut i = resolver.interpolator();
        i.text("{{missing}}");
        i.text("{{open");
        assert!(matches!(i.finish(()), Err(ResolveError::Template(_))));
    }

    #[test]
    fn secret_spans_and_used_secrets_are_recorded() {
        struct Store;
        impl SecretLookup for Store {
            fn secret(&self, _: &str, _: &str) -> Result<Option<String>, String> {
                Ok(Some("s3cr3t".into()))
            }
        }
        let env = Environment {
            name: "prod".into(),
            variables: Variables::default(),
            secrets: vec!["api_token".into()],
            production: false,
        };
        let resolver = Resolver::new().with(EnvironmentSource::new(&env, &Store));
        let mut i = resolver.interpolator();
        let r = i.text("Bearer {{api_token}}");
        let (r, used) = i.finish(r).unwrap();
        assert_eq!(r.text, "Bearer s3cr3t");
        assert_eq!(
            r.secret_spans,
            [SecretSpan {
                name: "api_token".into(),
                range: 7..13
            }]
        );
        assert_eq!(used[0].expose(), "s3cr3t");
        assert!(!format!("{used:?}").contains("s3cr3t"));
    }

    #[test]
    fn secret_not_set_gives_the_spec_hint() {
        let env = Environment {
            name: "prod".into(),
            variables: Variables::default(),
            secrets: vec!["api_token".into()],
            production: false,
        };
        let resolver = Resolver::new()
            .with(EnvironmentSource::new(&env, &NoSecrets))
            .with(at(Layer::Settings, &[("api_token", "from-settings")]));
        assert_eq!(
            resolver.resolve("{{api_token}}").unwrap_err().to_string(),
            "unresolved variable(s): api_token (secret not set; run 'rustman secret set api_token --env prod')"
        );
    }
}
