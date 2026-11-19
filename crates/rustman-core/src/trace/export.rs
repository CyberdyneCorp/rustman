//! `[trace.export]`: where Rustman's own client spans go (trace-propagation
//! spec "OTLP export is opt-in and explicit", design D6).
//!
//! Nothing is exported unless `enabled = true` names an `endpoint`. The
//! standard `OTEL_EXPORTER_OTLP_*` variables are read only with
//! `use_env = true`, through the lookup [`ExportTarget::resolve`] is given.

use std::fmt;

use serde::Deserialize;

/// The `[trace.export]` table.
#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ExportConfig {
    pub enabled: bool,
    /// The collector: `http(s)://host:port`, with an optional path for
    /// `http/protobuf`. No default.
    pub endpoint: Option<String>,
    pub protocol: ExportProtocol,
    /// Let `OTEL_EXPORTER_OTLP_*` variables override the endpoint and
    /// protocol and add headers.
    pub use_env: bool,
    /// The `service.name` of exported spans; `rustman` by default.
    pub service_name: Option<String>,
}

/// The OTLP transport.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Deserialize)]
pub enum ExportProtocol {
    #[default]
    #[serde(rename = "grpc")]
    Grpc,
    #[serde(rename = "http/protobuf")]
    HttpProtobuf,
}

/// The message when export is on without an endpoint.
pub const ENDPOINT_REQUIRED: &str = "trace.export.endpoint is required when export is enabled";

/// The gRPC method spans are exported with.
pub const GRPC_EXPORT_PATH: &str = "/opentelemetry.proto.collector.trace.v1.TraceService/Export";

/// The path `http/protobuf` appends to an endpoint without one.
pub const HTTP_TRACES_PATH: &str = "/v1/traces";

const DEFAULT_SERVICE_NAME: &str = "rustman";

impl ExportConfig {
    /// Check the table: an enabled export needs an `http(s)://` endpoint.
    pub fn validate(&self) -> Result<(), String> {
        let endpoint = self.endpoint.as_deref().filter(|e| !e.is_empty());
        match endpoint {
            None if self.enabled => Err(ENDPOINT_REQUIRED.to_owned()),
            Some(endpoint) if !is_http_url(endpoint) => Err(format!(
                "trace.export.endpoint: expected an http:// or https:// URL, got '{endpoint}'"
            )),
            _ => Ok(()),
        }
    }
}

fn is_http_url(endpoint: &str) -> bool {
    url::Url::parse(endpoint)
        .is_ok_and(|u| matches!(u.scheme(), "http" | "https") && u.host_str().is_some())
}

/// Where spans are sent, resolved from an [`ExportConfig`] (and, with
/// `use_env`, the environment). Its `Debug` hides header values, which
/// usually hold an API key.
#[derive(Clone, PartialEq, Eq, Hash)]
pub struct ExportTarget {
    /// The full request URL: the gRPC method or the `/v1/traces` path.
    pub url: String,
    pub protocol: ExportProtocol,
    /// Extra request headers (`OTEL_EXPORTER_OTLP_HEADERS` with `use_env`).
    pub headers: Vec<(String, String)>,
    pub service_name: String,
}

impl fmt::Debug for ExportTarget {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let names: Vec<&str> = self.headers.iter().map(|(k, _)| k.as_str()).collect();
        f.debug_struct("ExportTarget")
            .field("url", &self.url)
            .field("protocol", &self.protocol)
            .field("headers", &names)
            .field("service_name", &self.service_name)
            .finish()
    }
}

/// Reads one environment variable.
pub type EnvLookup<'a> = &'a dyn Fn(&str) -> Option<String>;

impl ExportTarget {
    /// The target of `config`; `None` when export is off (or, which load
    /// validation prevents, has no usable endpoint). `env` is consulted
    /// only with `use_env = true`.
    pub fn resolve(config: &ExportConfig, env: EnvLookup<'_>) -> Option<Self> {
        if !config.enabled {
            return None;
        }
        let none = |_: &str| None;
        let env: EnvLookup<'_> = if config.use_env { env } else { &none };
        let read = |names: [&str; 2]| names.iter().find_map(|n| env(n).filter(|v| !v.is_empty()));
        let protocol = match read([
            "OTEL_EXPORTER_OTLP_TRACES_PROTOCOL",
            "OTEL_EXPORTER_OTLP_PROTOCOL",
        ])
        .as_deref()
        {
            Some("grpc") => ExportProtocol::Grpc,
            Some("http/protobuf") => ExportProtocol::HttpProtobuf,
            _ => config.protocol,
        };
        let url = match env("OTEL_EXPORTER_OTLP_TRACES_ENDPOINT").filter(|v| !v.is_empty()) {
            Some(full) => full,
            None => {
                let base = env("OTEL_EXPORTER_OTLP_ENDPOINT")
                    .filter(|v| !v.is_empty())
                    .or_else(|| config.endpoint.clone())?;
                request_url(&base, protocol)?
            }
        };
        let headers = read([
            "OTEL_EXPORTER_OTLP_TRACES_HEADERS",
            "OTEL_EXPORTER_OTLP_HEADERS",
        ])
        .map(|h| parse_headers(&h))
        .unwrap_or_default();
        Some(Self {
            url,
            protocol,
            headers,
            service_name: config
                .service_name
                .clone()
                .unwrap_or_else(|| DEFAULT_SERVICE_NAME.to_owned()),
        })
    }

    /// [`ExportTarget::resolve`] with the process environment.
    pub fn from_process_env(config: &ExportConfig) -> Option<Self> {
        Self::resolve(config, &|name| std::env::var(name).ok())
    }

    /// `host:port` of the collector, as the TUI badge shows it.
    pub fn host(&self) -> String {
        let Ok(url) = url::Url::parse(&self.url) else {
            return self.url.clone();
        };
        let host = url.host_str().unwrap_or_default();
        match url.port_or_known_default() {
            Some(port) => format!("{host}:{port}"),
            None => host.to_owned(),
        }
    }
}

/// The request URL for `base`: the gRPC method path, or `/v1/traces` when
/// an `http/protobuf` endpoint has no path of its own.
fn request_url(base: &str, protocol: ExportProtocol) -> Option<String> {
    let url = url::Url::parse(base).ok()?;
    let root = format!("{}://{}", url.scheme(), url.host_str()?);
    let root = match url.port() {
        Some(port) => format!("{root}:{port}"),
        None => root,
    };
    let path = url.path().trim_end_matches('/');
    Some(match protocol {
        ExportProtocol::Grpc => format!("{root}{GRPC_EXPORT_PATH}"),
        ExportProtocol::HttpProtobuf if path.is_empty() => format!("{root}{HTTP_TRACES_PATH}"),
        ExportProtocol::HttpProtobuf => format!("{root}{path}"),
    })
}

/// `OTEL_EXPORTER_OTLP_HEADERS`: `k1=v1,k2=v2`; malformed pairs are skipped.
fn parse_headers(text: &str) -> Vec<(String, String)> {
    text.split(',')
        .filter_map(|pair| pair.split_once('='))
        .map(|(k, v)| (k.trim().to_owned(), v.trim().to_owned()))
        .filter(|(k, _)| !k.is_empty())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn enabled(endpoint: Option<&str>) -> ExportConfig {
        ExportConfig {
            enabled: true,
            endpoint: endpoint.map(str::to_owned),
            ..ExportConfig::default()
        }
    }

    fn env_with(pairs: &'static [(&'static str, &'static str)]) -> impl Fn(&str) -> Option<String> {
        move |name| {
            pairs
                .iter()
                .find(|(k, _)| *k == name)
                .map(|(_, v)| (*v).to_owned())
        }
    }

    const OTEL_ENV: &[(&str, &str)] = &[
        (
            "OTEL_EXPORTER_OTLP_ENDPOINT",
            "http://shared-collector:4318",
        ),
        ("OTEL_EXPORTER_OTLP_PROTOCOL", "http/protobuf"),
        ("OTEL_EXPORTER_OTLP_HEADERS", "x-api-key=k3y, bad ,x-team=a"),
    ];

    #[test]
    fn an_enabled_export_needs_an_endpoint() {
        assert_eq!(enabled(None).validate().unwrap_err(), ENDPOINT_REQUIRED);
        assert_eq!(enabled(Some("")).validate().unwrap_err(), ENDPOINT_REQUIRED);
        assert!(
            enabled(Some("localhost:4317"))
                .validate()
                .unwrap_err()
                .starts_with("trace.export.endpoint: expected an http")
        );
        assert!(enabled(Some("http://localhost:4317")).validate().is_ok());
        assert!(ExportConfig::default().validate().is_ok());
    }

    #[test]
    fn nothing_resolves_unless_enabled() {
        let env = env_with(OTEL_ENV);
        let off = ExportConfig {
            use_env: true,
            endpoint: Some("http://localhost:4317".into()),
            ..ExportConfig::default()
        };
        assert_eq!(ExportTarget::resolve(&off, &env), None);
    }

    #[test]
    fn env_vars_are_ignored_without_use_env() {
        let env = env_with(OTEL_ENV);
        let target = ExportTarget::resolve(&enabled(Some("http://localhost:4317")), &env).unwrap();
        assert_eq!(
            target.url,
            "http://localhost:4317/opentelemetry.proto.collector.trace.v1.TraceService/Export"
        );
        assert_eq!(target.protocol, ExportProtocol::Grpc);
        assert!(target.headers.is_empty());
        assert_eq!(target.service_name, "rustman");
        assert_eq!(target.host(), "localhost:4317");
    }

    #[test]
    fn use_env_lets_the_variables_win() {
        let env = env_with(OTEL_ENV);
        let config = ExportConfig {
            use_env: true,
            service_name: Some("ci".into()),
            ..enabled(Some("http://localhost:4317"))
        };
        let target = ExportTarget::resolve(&config, &env).unwrap();
        assert_eq!(target.url, "http://shared-collector:4318/v1/traces");
        assert_eq!(target.protocol, ExportProtocol::HttpProtobuf);
        assert_eq!(
            target.headers,
            [
                ("x-api-key".to_owned(), "k3y".to_owned()),
                ("x-team".to_owned(), "a".to_owned())
            ]
        );
        assert_eq!(target.service_name, "ci");
        let debug = format!("{target:?}");
        assert!(
            !debug.contains("k3y") && debug.contains("x-api-key"),
            "{debug}"
        );
        let full = env_with(&[("OTEL_EXPORTER_OTLP_TRACES_ENDPOINT", "https://c/custom")]);
        let target = ExportTarget::resolve(&config, &full).unwrap();
        assert_eq!(target.url, "https://c/custom");
        assert_eq!(target.host(), "c:443");
    }

    #[test]
    fn http_endpoints_get_the_traces_path_unless_they_have_one() {
        let http = |endpoint: &str| {
            let config = ExportConfig {
                protocol: ExportProtocol::HttpProtobuf,
                ..enabled(Some(endpoint))
            };
            ExportTarget::resolve(&config, &|_| None).unwrap().url
        };
        assert_eq!(http("http://h:4318"), "http://h:4318/v1/traces");
        assert_eq!(http("http://h:4318/"), "http://h:4318/v1/traces");
        assert_eq!(http("https://h/otlp/v1/traces"), "https://h/otlp/v1/traces");
    }

    #[test]
    fn the_protocol_names_deserialize() {
        #[derive(Deserialize)]
        struct T {
            p: ExportProtocol,
        }
        let t: T = toml::from_str("p = \"http/protobuf\"").unwrap();
        assert_eq!(t.p, ExportProtocol::HttpProtobuf);
        assert!(toml::from_str::<T>("p = \"http/json\"").is_err());
    }
}
