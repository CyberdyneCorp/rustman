//! The `trace:` block of an environment and `[trace]` of `rustman.toml`.

use std::collections::BTreeMap;

use serde::Deserialize;

use super::{ExportConfig, baggage, w3c};

/// One trace propagation setting block. Every field is optional so an
/// environment's block can override `rustman.toml [trace]` key by key
/// ([`TraceConfig::over`]).
#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct TraceConfig {
    /// Inject trace headers. Off unless set (or `--trace`).
    pub enabled: Option<bool>,
    /// The sampled flag sent; on by default.
    pub sampled: Option<bool>,
    /// One trace per exchange (default) or per run.
    pub scope: Option<TraceScope>,
    /// The header formats sent; `[w3c]` by default.
    pub formats: Option<Vec<PropagationFormat>>,
    /// `tracestate` members; values may hold `{{variables}}`.
    pub tracestate: Option<BTreeMap<String, String>>,
    /// `baggage` members; values may hold `{{variables}}`.
    pub baggage: Option<BTreeMap<String, String>>,
    /// A link template or a preset name (`jaeger`, `tempo`, `honeycomb`,
    /// `datadog`).
    pub link: Option<String>,
    /// The base URL a preset link starts with.
    pub link_base: Option<String>,
    /// `[trace.export]`: OTLP export of client spans.
    pub export: Option<ExportConfig>,
}

/// Which exchanges share a trace id.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TraceScope {
    /// A new trace per exchange.
    #[default]
    Exchange,
    /// One trace for a whole run or exploration, a new span per exchange.
    Run,
}

/// A trace header format.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
pub enum PropagationFormat {
    /// `traceparent` (W3C Trace Context).
    #[serde(rename = "w3c")]
    W3c,
    /// The single `b3` header.
    #[serde(rename = "b3")]
    B3,
    /// `X-B3-TraceId`, `X-B3-SpanId` and `X-B3-Sampled`.
    #[serde(rename = "b3multi")]
    B3Multi,
}

/// The placeholders a link template may use, besides `{{variables}}`.
pub const LINK_PLACEHOLDERS: [&str; 5] =
    ["trace_id", "span_id", "start_ms", "end_ms", "trace_id_dd"];

/// Link presets and their templates; `{base}` is `trace.link_base`.
pub const LINK_PRESETS: [(&str, &str); 4] = [
    ("jaeger", "{base}/trace/{trace_id}"),
    (
        "tempo",
        "{base}/explore?left=%7B%22datasource%22:%22tempo%22,%22queries%22:%5B%7B%22queryType%22:%22traceql%22,%22query%22:%22{trace_id}%22%7D%5D%7D",
    ),
    ("honeycomb", "{base}/trace?trace_id={trace_id}"),
    ("datadog", "{base}/apm/trace/{trace_id}"),
];

impl TraceConfig {
    /// This block over `base`: each key set here wins.
    pub fn over(&self, base: &TraceConfig) -> TraceConfig {
        TraceConfig {
            enabled: self.enabled.or(base.enabled),
            sampled: self.sampled.or(base.sampled),
            scope: self.scope.or(base.scope),
            formats: self.formats.clone().or_else(|| base.formats.clone()),
            tracestate: self.tracestate.clone().or_else(|| base.tracestate.clone()),
            baggage: self.baggage.clone().or_else(|| base.baggage.clone()),
            link: self.link.clone().or_else(|| base.link.clone()),
            link_base: self.link_base.clone().or_else(|| base.link_base.clone()),
            export: self.export.clone().or_else(|| base.export.clone()),
        }
    }

    pub fn is_enabled(&self) -> bool {
        self.enabled.unwrap_or(false)
    }

    /// `[trace.export] enabled = true`.
    pub fn is_exporting(&self) -> bool {
        self.export.as_ref().is_some_and(|e| e.enabled)
    }

    /// Check every key of one block, with a message naming it, such as
    /// `trace.link: unknown placeholder {traceid}`. A preset's need for
    /// `link_base` depends on the merged blocks: see
    /// [`TraceConfig::check_merged`].
    pub fn validate(&self) -> Result<(), String> {
        if self.formats.as_ref().is_some_and(Vec::is_empty) {
            return Err("trace.formats: name at least one format".to_owned());
        }
        for key in self.tracestate.iter().flat_map(BTreeMap::keys) {
            w3c::check_member(key, "x").map_err(|e| format!("trace.tracestate: {e}"))?;
        }
        for key in self.baggage.iter().flat_map(BTreeMap::keys) {
            if !baggage::is_key(key) {
                return Err(format!("trace.baggage: invalid key '{key}'"));
            }
        }
        if let Some(link) = self.link.as_deref().filter(|l| !is_preset(l)) {
            check_custom_link(link)?;
        }
        self.export.as_ref().map_or(Ok(()), ExportConfig::validate)
    }

    /// Check what only the merged configuration can tell (an environment's
    /// block over `[trace]`): a preset link needs `link_base` from either.
    pub fn check_merged(&self) -> Result<(), String> {
        match &self.link {
            Some(link) => link_template(link, self.link_base.as_deref()).map(drop),
            None => Ok(()),
        }
    }
}

fn is_preset(link: &str) -> bool {
    LINK_PRESETS.iter().any(|(name, _)| *name == link)
}

/// A custom template: known placeholders, and an `http(s)://` URL unless
/// it starts with a `{{variable}}` (checked once resolved, by
/// [`check_link_scheme`]).
fn check_custom_link(link: &str) -> Result<(), String> {
    check_placeholders(link)?;
    if link.starts_with("{{") {
        return Ok(());
    }
    check_link_scheme(link)
}

/// A link is opened in the system browser, so it must be an absolute
/// `http://` or `https://` URL.
pub fn check_link_scheme(link: &str) -> Result<(), String> {
    let lower = link.to_ascii_lowercase();
    let host = ["http://", "https://"]
        .iter()
        .find_map(|scheme| lower.strip_prefix(scheme));
    match host {
        Some(rest) if !rest.is_empty() && !rest.starts_with('/') => Ok(()),
        _ => Err(format!(
            "trace.link: expected an http:// or https:// URL, got '{link}'"
        )),
    }
}

/// The template `link` stands for: a preset expanded with `base`, or
/// `link` itself once its placeholders are checked.
pub fn link_template(link: &str, base: Option<&str>) -> Result<String, String> {
    if let Some((name, template)) = LINK_PRESETS.iter().find(|(name, _)| *name == link) {
        let base = base
            .filter(|b| !b.is_empty())
            .ok_or_else(|| format!("trace.link: preset '{name}' needs trace.link_base"))?;
        return Ok(template.replace("{base}", base.trim_end_matches('/')));
    }
    check_custom_link(link)?;
    Ok(link.to_owned())
}

/// Every `{name}` must be a known placeholder; `{{variable}}` is left to
/// variable resolution.
fn check_placeholders(template: &str) -> Result<(), String> {
    let mut rest = template;
    while let Some(start) = rest.find('{') {
        let after = &rest[start..];
        if let Some(variable) = after.strip_prefix("{{") {
            let end = variable
                .find("}}")
                .ok_or("trace.link: unclosed {{ in the template")?;
            rest = &variable[end + 2..];
            continue;
        }
        let end = after
            .find('}')
            .ok_or("trace.link: unclosed { in the template")?;
        let name = &after[1..end];
        if !LINK_PLACEHOLDERS.contains(&name) {
            return Err(format!("trace.link: unknown placeholder {{{name}}}"));
        }
        rest = &after[end + 1..];
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn link(link: &str, base: Option<&str>) -> Result<(), String> {
        TraceConfig {
            link: Some(link.to_owned()),
            link_base: base.map(str::to_owned),
            ..TraceConfig::default()
        }
        .validate()
    }

    #[test]
    fn an_unknown_placeholder_is_named() {
        assert_eq!(
            link("https://x/{traceid}", None).unwrap_err(),
            "trace.link: unknown placeholder {traceid}"
        );
        assert!(
            link(
                "https://x/{trace_id}?s={span_id}&a={start_ms}&b={end_ms}",
                None
            )
            .is_ok()
        );
        assert!(link("{{grafana}}/t/{trace_id_dd}", None).is_ok());
        assert!(link("https://x/{trace_id", None).is_err());
        assert!(link("{{ui}}/t/{trace_id}", None).is_ok());
        assert!(link("https://x/{{base", None).is_err());
    }

    #[test]
    fn presets_need_a_base_once_merged() {
        assert!(link("jaeger", None).is_ok());
        let alone = TraceConfig {
            link: Some("jaeger".into()),
            ..TraceConfig::default()
        };
        assert_eq!(
            alone.check_merged().unwrap_err(),
            "trace.link: preset 'jaeger' needs trace.link_base"
        );
        let env = TraceConfig {
            link_base: Some("http://localhost:16686".into()),
            ..TraceConfig::default()
        };
        assert!(env.over(&alone).check_merged().is_ok());
        assert!(alone.over(&env).check_merged().is_ok());
        assert_eq!(
            link_template("jaeger", Some("http://localhost:16686/")).unwrap(),
            "http://localhost:16686/trace/{trace_id}"
        );
        for (name, _) in LINK_PRESETS {
            let template = link_template(name, Some("https://ui")).unwrap();
            assert!(template.starts_with("https://ui/"), "{template}");
            assert!(template.contains("{trace_id}"), "{template}");
        }
    }

    #[test]
    fn a_custom_link_must_be_an_http_url() {
        for bad in [
            "file:///tmp/x.command",
            "-a Calculator",
            "javascript:alert(1)",
            "http:///{trace_id}",
            "ui/{trace_id}",
        ] {
            assert_eq!(
                link(bad, None).unwrap_err(),
                format!("trace.link: expected an http:// or https:// URL, got '{bad}'"),
            );
        }
        assert!(link("HTTPS://ui/{trace_id}", None).is_ok());
    }

    #[test]
    fn keys_are_checked() {
        let tracestate = TraceConfig {
            tracestate: Some(BTreeMap::from([("Bad".to_owned(), "v".to_owned())])),
            ..TraceConfig::default()
        };
        assert_eq!(
            tracestate.validate().unwrap_err(),
            "trace.tracestate: invalid tracestate key 'Bad'"
        );
        let baggage = TraceConfig {
            baggage: Some(BTreeMap::from([("a b".to_owned(), "v".to_owned())])),
            ..TraceConfig::default()
        };
        assert_eq!(
            baggage.validate().unwrap_err(),
            "trace.baggage: invalid key 'a b'"
        );
        let empty = TraceConfig {
            formats: Some(Vec::new()),
            ..TraceConfig::default()
        };
        assert!(empty.validate().is_err());
    }

    #[test]
    fn an_environment_block_overrides_key_by_key() {
        let settings = TraceConfig {
            enabled: Some(true),
            sampled: Some(false),
            link: Some("jaeger".into()),
            ..TraceConfig::default()
        };
        let env = TraceConfig {
            sampled: Some(true),
            scope: Some(TraceScope::Run),
            ..TraceConfig::default()
        };
        let merged = env.over(&settings);
        assert!(merged.is_enabled());
        assert_eq!(merged.sampled, Some(true));
        assert_eq!(merged.scope, Some(TraceScope::Run));
        assert_eq!(merged.link.as_deref(), Some("jaeger"));
        assert!(!TraceConfig::default().is_enabled());
    }

    #[test]
    fn an_environment_export_table_replaces_the_whole_table() {
        let settings = TraceConfig {
            export: Some(ExportConfig {
                enabled: true,
                endpoint: Some("http://localhost:4317".into()),
                ..ExportConfig::default()
            }),
            ..TraceConfig::default()
        };
        let env = TraceConfig {
            export: Some(ExportConfig::default()),
            ..TraceConfig::default()
        };
        assert!(!env.over(&settings).is_exporting());
        assert!(settings.over(&TraceConfig::default()).is_exporting());
    }
}
