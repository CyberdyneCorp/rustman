//! Trace context propagation (trace-propagation spec, design D1-D3):
//! W3C `traceparent`/`tracestate`, B3 single and multi, and `baggage`,
//! generated and parsed by hand with no global state.
//!
//! [`TraceConfig`] is the `trace:` block of an environment and `[trace]` of
//! `rustman.toml`; [`TracePropagation`] is that configuration resolved for
//! one run, and [`inject`] applies it to the headers (or gRPC metadata) of
//! one outgoing request through a [`TraceContextInjector`].
//!
//! After the exchange, [`TraceRecord`] is what history and reports keep:
//! the ids, the backend's `traceresponse` id and the link a [`TraceLink`]
//! builds; [`server_timing`] parses the `Server-Timing` header.

pub mod b3;
pub mod baggage;
mod batch;
mod config;
mod export;
mod ids;
mod inject;
mod link;
mod record;
pub mod server_timing;
mod span;
pub mod w3c;

use std::fmt;

use crate::environment::Environment;
use crate::model::Variables;
use crate::settings::Settings;
use crate::vars::{EnvironmentSource, Layer, Resolver, SecretLookup, UsedSecret, VariablesSource};

pub use batch::{SpanQueue, WarnLimiter};
pub use config::{
    LINK_PLACEHOLDERS, LINK_PRESETS, PropagationFormat, TraceConfig, TraceScope, link_template,
};
pub use export::{
    ENDPOINT_REQUIRED, EnvLookup, ExportConfig, ExportProtocol, ExportTarget, GRPC_EXPORT_PATH,
    HTTP_TRACES_PATH,
};
pub use ids::{SpanId, TraceContext, TraceId};
pub use inject::{TraceContextInjector, carries_invalid, inject, inject_random, supplied};
pub use link::{LinkIds, TraceLink};
pub use record::{SentTrace, TRACERESPONSE, TraceRecord};
pub use server_timing::ServerTiming;
pub use span::{ClientSpan, SpanEvent, SpanValue, url_full};

/// Trace propagation resolved for one run: what [`inject`] adds to each
/// request. Its `Debug` leaves out `tracestate` and `baggage` values, which
/// may hold secrets.
#[derive(Clone, PartialEq, Eq)]
pub struct TracePropagation {
    pub sampled: bool,
    pub formats: Vec<PropagationFormat>,
    pub tracestate: Vec<(String, String)>,
    pub baggage: Vec<(String, String)>,
    /// The trace every exchange shares under `scope: run`; `None` gives
    /// each exchange its own.
    pub run_trace_id: Option<TraceId>,
    /// The secret values the `tracestate` and `baggage` members used, so
    /// a cross-origin redirect drops the headers carrying them.
    pub secrets: Vec<UsedSecret>,
}

impl Default for TracePropagation {
    /// W3C only, sampled, one trace per exchange: `trace: {enabled: true}`.
    fn default() -> Self {
        Self {
            sampled: true,
            formats: vec![PropagationFormat::W3c],
            tracestate: Vec::new(),
            baggage: Vec::new(),
            run_trace_id: None,
            secrets: Vec::new(),
        }
    }
}

impl fmt::Debug for TracePropagation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let keys = |members: &[(String, String)]| -> Vec<String> {
            members.iter().map(|(k, _)| k.clone()).collect()
        };
        f.debug_struct("TracePropagation")
            .field("sampled", &self.sampled)
            .field("formats", &self.formats)
            .field("tracestate", &keys(&self.tracestate))
            .field("baggage", &keys(&self.baggage))
            .field("run_trace_id", &self.run_trace_id)
            .finish_non_exhaustive()
    }
}

/// A resolved propagation and the secret values its members used.
pub type Resolved = (TracePropagation, Vec<UsedSecret>);

impl TracePropagation {
    /// What `config` asks for, with `tracestate` and `baggage` values
    /// resolved through `resolver`; `None` when propagation is off and not
    /// `forced` (`--trace`). An enabled `[trace.export]` turns propagation
    /// on, since exported spans need ids. A `scope: run` configuration
    /// draws the run's trace id here.
    pub fn resolve(
        config: &TraceConfig,
        forced: bool,
        resolver: &Resolver<'_>,
    ) -> Result<Option<Resolved>, String> {
        if !forced && !config.is_enabled() && !config.is_exporting() {
            return Ok(None);
        }
        let mut i = resolver.interpolator();
        let mut members = |map: &Option<std::collections::BTreeMap<String, String>>| {
            map.iter()
                .flatten()
                .map(|(k, v)| (k.clone(), i.text(v).text))
                .collect::<Vec<_>>()
        };
        let tracestate = members(&config.tracestate);
        let baggage = members(&config.baggage);
        let ((tracestate, baggage), secrets) = i
            .finish((tracestate, baggage))
            .map_err(|e| format!("trace: {e}"))?;
        for (key, value) in &tracestate {
            w3c::check_member(key, value).map_err(|e| format!("trace.tracestate: {e}"))?;
        }
        let run_trace_id =
            (config.scope == Some(TraceScope::Run)).then(|| TraceId::random(&mut rand::rng()));
        let propagation = Self {
            sampled: config.sampled.unwrap_or(true),
            formats: config
                .formats
                .clone()
                .unwrap_or_else(|| vec![PropagationFormat::W3c]),
            tracestate,
            baggage,
            run_trace_id,
            secrets: secrets.clone(),
        };
        Ok(Some((propagation, secrets)))
    }

    /// [`TracePropagation::resolve`] for a run in a workspace: the active
    /// environment's `trace:` block over `rustman.toml [trace]`, values
    /// resolved from `--var` overrides, the environment and
    /// `rustman.toml [variables]`.
    pub fn for_workspace(
        settings: &Settings,
        environment: Option<&Environment>,
        secrets: &dyn SecretLookup,
        overrides: &Variables,
        forced: bool,
    ) -> Result<Option<Resolved>, String> {
        WorkspaceScope {
            settings,
            environment,
            secrets,
            overrides,
        }
        .propagation(forced)
    }
}

/// Where the trace configuration of a run comes from: the active
/// environment's `trace:` block over `rustman.toml [trace]`, with values
/// resolved from `--var` overrides, the environment and `[variables]`.
#[derive(Clone, Copy)]
pub struct WorkspaceScope<'a> {
    pub settings: &'a Settings,
    pub environment: Option<&'a Environment>,
    pub secrets: &'a dyn SecretLookup,
    pub overrides: &'a Variables,
}

impl WorkspaceScope<'_> {
    /// The merged configuration.
    pub fn config(&self) -> TraceConfig {
        match self.environment {
            Some(env) => env.trace.over(&self.settings.trace),
            None => self.settings.trace.clone(),
        }
    }

    /// [`TracePropagation::resolve`] for this scope's merged configuration.
    pub fn propagation(&self, forced: bool) -> Result<Option<Resolved>, String> {
        self.with(|config, resolver| TracePropagation::resolve(config, forced, resolver))
    }

    /// The configured link, with its variables resolved; `None` without
    /// `trace.link`.
    pub fn link(&self) -> Result<Option<(TraceLink, Vec<UsedSecret>)>, String> {
        self.with(TraceLink::resolve)
    }

    fn with<T>(&self, f: impl FnOnce(&TraceConfig, &Resolver<'_>) -> T) -> T {
        let mut resolver = Resolver::new();
        resolver.push(VariablesSource::new(Layer::Override, self.overrides));
        if let Some(env) = self.environment {
            resolver.push(EnvironmentSource::new(env, self.secrets));
        }
        resolver.push(VariablesSource::new(
            Layer::Settings,
            &self.settings.variables,
        ));
        f(&self.config(), &resolver)
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;
    use crate::model::VarValue;
    use crate::vars::NoSecrets;

    fn vars(pairs: &[(&str, &str)]) -> Variables {
        Variables::new(
            pairs
                .iter()
                .map(|(k, v)| ((*k).to_owned(), VarValue::String((*v).to_owned())))
                .collect(),
        )
    }

    fn resolve(
        config: &TraceConfig,
        forced: bool,
        vars: &Variables,
    ) -> Result<Option<Resolved>, String> {
        let resolver = Resolver::new().with(VariablesSource::new(Layer::Override, vars));
        TracePropagation::resolve(config, forced, &resolver)
    }

    #[test]
    fn off_unless_enabled_or_forced() {
        let none = Variables::default();
        assert!(
            resolve(&TraceConfig::default(), false, &none)
                .unwrap()
                .is_none()
        );
        let off = TraceConfig {
            enabled: Some(false),
            ..TraceConfig::default()
        };
        let (forced, _) = resolve(&off, true, &none).unwrap().unwrap();
        assert_eq!(forced, TracePropagation::default());
    }

    #[test]
    fn baggage_values_resolve_variables() {
        let config = TraceConfig {
            enabled: Some(true),
            baggage: Some(BTreeMap::from([("tenant".into(), "{{tenant_id}}".into())])),
            tracestate: Some(BTreeMap::from([("rojo".into(), "{{span}}".into())])),
            scope: Some(TraceScope::Run),
            sampled: Some(false),
            ..TraceConfig::default()
        };
        let (p, secrets) = resolve(
            &config,
            false,
            &vars(&[("tenant_id", "acme"), ("span", "x1")]),
        )
        .unwrap()
        .unwrap();
        assert_eq!(p.baggage, [("tenant".to_owned(), "acme".to_owned())]);
        assert_eq!(p.tracestate, [("rojo".to_owned(), "x1".to_owned())]);
        assert!(p.run_trace_id.is_some() && !p.sampled && secrets.is_empty());
        let err = resolve(&config, false, &Variables::default()).unwrap_err();
        assert!(err.starts_with("trace: unresolved variable(s): "), "{err}");
        let bad = resolve(
            &config,
            false,
            &vars(&[("tenant_id", "a"), ("span", "a,b")]),
        );
        assert_eq!(
            bad.unwrap_err(),
            "trace.tracestate: invalid tracestate value for 'rojo'"
        );
    }

    #[test]
    fn the_environment_block_wins_and_debug_hides_values() {
        let (mut settings, _) = Settings::parse(
            std::path::Path::new("rustman.toml"),
            "[workspace]\nname = \"a\"\nversion = 1\n[trace]\nenabled = true\nsampled = false\n[trace.baggage]\nk = \"{{v}}\"\n[variables]\nv = \"hidden-value\"\n",
        )
        .unwrap();
        settings.variables = vars(&[("v", "hidden-value")]);
        let env = Environment {
            name: "staging".into(),
            trace: TraceConfig {
                sampled: Some(true),
                ..TraceConfig::default()
            },
            ..Environment::default()
        };
        let none = Variables::default();
        let (p, _) =
            TracePropagation::for_workspace(&settings, Some(&env), &NoSecrets, &none, false)
                .unwrap()
                .unwrap();
        assert!(p.sampled);
        assert_eq!(p.baggage, [("k".to_owned(), "hidden-value".to_owned())]);
        let debug = format!("{p:?}");
        assert!(
            !debug.contains("hidden-value") && debug.contains("\"k\""),
            "{debug}"
        );
        let (p, _) = TracePropagation::for_workspace(&settings, None, &NoSecrets, &none, false)
            .unwrap()
            .unwrap();
        assert!(!p.sampled);
    }

    #[test]
    fn the_workspace_link_resolves_environment_variables() {
        let (settings, _) = Settings::parse(
            std::path::Path::new("rustman.toml"),
            "[workspace]\nname = \"a\"\nversion = 1\n[trace]\nlink = \"{{ui}}/trace/{trace_id}\"\n",
        )
        .unwrap();
        let env = Environment {
            name: "staging".into(),
            variables: vars(&[("ui", "http://jaeger:16686")]),
            ..Environment::default()
        };
        let none = Variables::default();
        let scope = WorkspaceScope {
            settings: &settings,
            environment: Some(&env),
            secrets: &NoSecrets,
            overrides: &none,
        };
        let (link, _) = scope.link().unwrap().unwrap();
        let ids = LinkIds {
            trace_id: "4bf92f3577b34da6a3ce929d0e0e4736",
            span_id: "00f067aa0ba902b7",
            start_ms: 0,
            end_ms: 0,
        };
        assert_eq!(
            link.render(&ids),
            "http://jaeger:16686/trace/4bf92f3577b34da6a3ce929d0e0e4736"
        );
        let bare = WorkspaceScope {
            environment: None,
            ..scope
        };
        assert!(
            bare.link()
                .unwrap_err()
                .starts_with("trace.link: unresolved")
        );
    }
}
