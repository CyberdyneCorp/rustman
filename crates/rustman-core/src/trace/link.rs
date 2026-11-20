//! Trace links (design D4): a URL template, or a preset expanded with
//! `trace.link_base`, filled with an exchange's ids and time window.

use super::config::{check_link_scheme, link_template};
use super::{TraceConfig, TraceId};
use crate::vars::{Resolver, UsedSecret};

/// A link template with its `{{variables}}` resolved, ready to fill with
/// the placeholders of [`LinkIds`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TraceLink {
    template: String,
}

/// What a link is filled with.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LinkIds<'a> {
    /// The trace the link opens: the backend's when it restarted the trace.
    pub trace_id: &'a str,
    pub span_id: &'a str,
    /// Unix milliseconds of the exchange's start and end.
    pub start_ms: u64,
    pub end_ms: u64,
}

impl TraceLink {
    /// A link to fill as is; `template` must be valid ([`link_template`]).
    pub fn new(template: impl Into<String>) -> Self {
        Self {
            template: template.into(),
        }
    }

    /// The link `config` asks for (`trace.link`, a preset needing
    /// `trace.link_base`), with its `{{variables}}` resolved through
    /// `resolver`; `None` without `trace.link`.
    pub fn resolve(
        config: &TraceConfig,
        resolver: &Resolver<'_>,
    ) -> Result<Option<(Self, Vec<UsedSecret>)>, String> {
        let Some(link) = &config.link else {
            return Ok(None);
        };
        let template = link_template(link, config.link_base.as_deref())?;
        let mut i = resolver.interpolator();
        let text = i.text(&template).text;
        let (text, secrets) = i.finish(text).map_err(|e| format!("trace.link: {e}"))?;
        check_link_scheme(&text).map_err(|_| {
            "trace.link: the resolved link is not an http:// or https:// URL".to_owned()
        })?;
        Ok(Some((Self::new(text), secrets)))
    }

    /// The template filled with `ids`.
    pub fn render(&self, ids: &LinkIds<'_>) -> String {
        let dd = TraceId::from_hex(ids.trace_id)
            .map(|id| id.low_u64().to_string())
            .unwrap_or_default();
        self.template
            .replace("{trace_id_dd}", &dd)
            .replace("{trace_id}", ids.trace_id)
            .replace("{span_id}", ids.span_id)
            .replace("{start_ms}", &ids.start_ms.to_string())
            .replace("{end_ms}", &ids.end_ms.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{VarValue, Variables};
    use crate::vars::{Layer, VariablesSource};

    const TRACE: &str = "4bf92f3577b34da6a3ce929d0e0e4736";
    const SPAN: &str = "00f067aa0ba902b7";

    fn ids() -> LinkIds<'static> {
        LinkIds {
            trace_id: TRACE,
            span_id: SPAN,
            start_ms: 1_700_000_000_000,
            end_ms: 1_700_000_000_084,
        }
    }

    /// A link, its base, its variables and the URL expected.
    type Case = (&'static str, Option<&'static str>, Vars, String);
    type Vars = &'static [(&'static str, &'static str)];

    fn rendered(link: &str, base: Option<&str>, vars: &[(&str, &str)]) -> Result<String, String> {
        let vars = Variables::new(
            vars.iter()
                .map(|(k, v)| ((*k).to_owned(), VarValue::String((*v).to_owned())))
                .collect(),
        );
        let resolver = Resolver::new().with(VariablesSource::new(Layer::Override, &vars));
        let config = TraceConfig {
            link: Some(link.to_owned()),
            link_base: base.map(str::to_owned),
            ..TraceConfig::default()
        };
        let (link, _) = TraceLink::resolve(&config, &resolver)?.expect("a link");
        Ok(link.render(&ids()))
    }

    #[test]
    fn presets_and_templates_render_the_expected_urls() {
        let tempo = format!(
            "http://grafana:3000/explore?left=%7B%22datasource%22:%22tempo%22,%22queries%22:%5B%7B%22queryType%22:%22traceql%22,%22query%22:%22{TRACE}%22%7D%5D%7D"
        );
        let cases: [Case; 7] = [
            (
                "jaeger",
                Some("http://localhost:16686"),
                &[],
                format!("http://localhost:16686/trace/{TRACE}"),
            ),
            ("tempo", Some("http://grafana:3000/"), &[], tempo),
            (
                "honeycomb",
                Some("https://ui.honeycomb.io/acme/environments/prod"),
                &[],
                format!("https://ui.honeycomb.io/acme/environments/prod/trace?trace_id={TRACE}"),
            ),
            (
                "datadog",
                Some("https://app.datadoghq.com"),
                &[],
                format!("https://app.datadoghq.com/apm/trace/{TRACE}"),
            ),
            (
                "https://app.datadoghq.eu/apm/trace/{trace_id_dd}",
                None,
                &[],
                // 0xa3ce929d0e0e4736, the lower 64 bits.
                "https://app.datadoghq.eu/apm/trace/11803532876627986230".to_owned(),
            ),
            (
                "{{grafana}}/t/{trace_id}?span={span_id}&from={start_ms}&to={end_ms}",
                None,
                &[("grafana", "https://g.example")],
                format!(
                    "https://g.example/t/{TRACE}?span={SPAN}&from=1700000000000&to=1700000000084"
                ),
            ),
            (
                "https://x/{trace_id}/{trace_id}",
                None,
                &[],
                format!("https://x/{TRACE}/{TRACE}"),
            ),
        ];
        for (link, base, vars, expected) in cases {
            assert_eq!(rendered(link, base, vars).unwrap(), expected, "{link}");
        }
    }

    #[test]
    fn errors_name_the_key() {
        assert_eq!(
            rendered("jaeger", None, &[]).unwrap_err(),
            "trace.link: preset 'jaeger' needs trace.link_base"
        );
        let err = rendered("{{nope}}/{trace_id}", None, &[]).unwrap_err();
        assert!(err.starts_with("trace.link: unresolved variable"), "{err}");
        let not_http = "trace.link: the resolved link is not an http:// or https:// URL";
        let file = rendered(
            "{{ui}}/{trace_id}",
            None,
            &[("ui", "file:///tmp/x.command")],
        );
        assert_eq!(file.unwrap_err(), not_http);
        let base = rendered("jaeger", Some("-a Calculator"), &[]);
        assert_eq!(base.unwrap_err(), not_http);
        let none = TraceLink::resolve(&TraceConfig::default(), &Resolver::new()).unwrap();
        assert!(none.is_none());
    }
}
