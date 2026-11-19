//! Optional security enforcement (spec: Optional security enforcement):
//! with `--enforce-security`, a request without the credentials of any
//! alternative of the operation's effective security requirement gets
//! `401` with `WWW-Authenticate`. Values are only compared when
//! `[mock.security]` names an expected value for the scheme, read from an
//! `environments` secret; the comparison takes constant time.

use std::collections::BTreeMap;
use std::path::Path;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use http::StatusCode;
use rustman_core::settings::SETTINGS_FILE;
use rustman_core::vars::{EnvironmentSource, Resolver, SecretLookup};
use rustman_core::{ConfigError, Environment, LoadError, Secret, Workspace};
use rustman_explore::openapi::model::{
    ApiModel, Operation, ParameterLocation, SecurityRequirement, SecurityScheme,
};
use rustman_webhook::verifiers::{resolve_secret, secret_name};
use subtle::ConstantTimeEq;

use crate::problem::Problem;
use crate::request::RequestView;

/// Expected credential values by security scheme name. For `http basic`
/// the value is `user:password`; for the others it is the token or key.
pub type Expected = BTreeMap<String, Secret<String>>;

/// A credential sent for one scheme: `Present(None)` is one whose value
/// cannot be compared (such as mutual TLS).
enum Sent {
    Missing,
    Present(Option<String>),
}

/// A refused request: the `401` problem and its `WWW-Authenticate` values.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Denied {
    pub problem: Problem,
    pub challenges: Vec<String>,
}

/// `Ok` when `request` satisfies the effective security of `operation`,
/// else why it is refused.
pub fn check(
    model: &ApiModel,
    operation: &Operation,
    request: &RequestView<'_>,
    expected: &Expected,
) -> Result<(), Box<Denied>> {
    let requirements = operation.security.as_ref().unwrap_or(&model.security);
    if requirements.is_empty() || requirements.iter().any(Vec::is_empty) {
        return Ok(());
    }
    let schemes = &model.security_schemes;
    let mut invalid = false;
    for requirement in requirements {
        match satisfied(requirement, schemes, request, expected) {
            Verdict::Satisfied => return Ok(()),
            Verdict::Invalid => invalid = true,
            Verdict::Missing => {}
        }
    }
    let challenges = challenges(requirements, schemes);
    let detail = if invalid {
        "the credentials sent do not match the expected values"
    } else {
        "the operation requires credentials that the request does not carry"
    };
    let problem = Problem::new(
        StatusCode::UNAUTHORIZED,
        "unauthorized",
        "Unauthorized",
        format!("{detail} ({})", names(requirements).join(" or ")),
    );
    Err(Box::new(Denied {
        problem,
        challenges,
    }))
}

enum Verdict {
    Satisfied,
    Missing,
    Invalid,
}

fn satisfied(
    requirement: &SecurityRequirement,
    schemes: &[(String, SecurityScheme)],
    request: &RequestView<'_>,
    expected: &Expected,
) -> Verdict {
    for (name, _) in requirement {
        let Some((_, scheme)) = schemes.iter().find(|(n, _)| n == name) else {
            continue;
        };
        match credential(scheme, request) {
            Sent::Missing => return Verdict::Missing,
            Sent::Present(value) => {
                if !matches_expected(expected.get(name), value.as_deref()) {
                    return Verdict::Invalid;
                }
            }
        }
    }
    Verdict::Satisfied
}

fn matches_expected(expected: Option<&Secret<String>>, sent: Option<&str>) -> bool {
    match (expected, sent) {
        (None, _) => true,
        (Some(expected), Some(sent)) => expected.expose().as_bytes().ct_eq(sent.as_bytes()).into(),
        (Some(_), None) => false,
    }
}

/// The credential `request` carries for `scheme`.
fn credential(scheme: &SecurityScheme, request: &RequestView<'_>) -> Sent {
    let found = match scheme {
        SecurityScheme::ApiKey { name, location } => api_key(request, name, *location),
        SecurityScheme::HttpBasic => authorization(request, "Basic").and_then(basic),
        SecurityScheme::HttpBearer { .. }
        | SecurityScheme::OAuth2 { .. }
        | SecurityScheme::OpenIdConnect { .. } => authorization(request, "Bearer"),
        SecurityScheme::Http { scheme } => authorization(request, scheme),
        SecurityScheme::MutualTls => return Sent::Present(None),
    };
    match found {
        Some(value) => Sent::Present(Some(value)),
        None => Sent::Missing,
    }
}

fn api_key(request: &RequestView<'_>, name: &str, location: ParameterLocation) -> Option<String> {
    let value = match location {
        ParameterLocation::Header => request.header(name),
        ParameterLocation::Query => request.query_values(name).first().copied(),
        ParameterLocation::Cookie => request.cookie(name),
        ParameterLocation::Path => None,
    };
    value.filter(|v| !v.is_empty()).map(str::to_owned)
}

/// The credentials after `Authorization: <scheme> ` (scheme in any case).
fn authorization(request: &RequestView<'_>, scheme: &str) -> Option<String> {
    let value = request.header("authorization")?.trim();
    let (sent, rest) = value.split_once(' ')?;
    let rest = rest.trim();
    (sent.eq_ignore_ascii_case(scheme) && !rest.is_empty()).then(|| rest.to_owned())
}

/// `user:password` from Basic credentials.
fn basic(encoded: String) -> Option<String> {
    let bytes = STANDARD.decode(encoded.as_bytes()).ok()?;
    String::from_utf8(bytes).ok().filter(|s| s.contains(':'))
}

/// One `WWW-Authenticate` challenge per HTTP-style scheme involved, in
/// requirement order and without repeats.
fn challenges(
    requirements: &[SecurityRequirement],
    schemes: &[(String, SecurityScheme)],
) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let all = requirements.iter().flatten();
    for (name, _) in all {
        let Some((_, scheme)) = schemes.iter().find(|(n, _)| n == name) else {
            continue;
        };
        let challenge = challenge(scheme);
        if !out.contains(&challenge) {
            out.push(challenge);
        }
    }
    out
}

fn challenge(scheme: &SecurityScheme) -> String {
    match scheme {
        SecurityScheme::HttpBasic => "Basic realm=\"rustman-mock\"".to_owned(),
        SecurityScheme::Http { scheme } => scheme.clone(),
        SecurityScheme::ApiKey { name, location } => {
            let location = match location {
                ParameterLocation::Header => "header",
                ParameterLocation::Query => "query",
                ParameterLocation::Cookie => "cookie",
                ParameterLocation::Path => "path",
            };
            format!("ApiKey name=\"{name}\", in=\"{location}\"")
        }
        _ => "Bearer".to_owned(),
    }
}

fn names(requirements: &[SecurityRequirement]) -> Vec<String> {
    requirements
        .iter()
        .map(|r| {
            let names: Vec<&str> = r.iter().map(|(n, _)| n.as_str()).collect();
            names.join(" + ")
        })
        .collect()
}

/// The expected values of `[mock.security]` (scheme name to a
/// `{{variable}}` reference), read from `environment` through `secrets`
/// only: never from `[variables]` in `rustman.toml`, which is meant for Git.
pub fn resolve(
    specs: &BTreeMap<String, String>,
    environment: Option<&Environment>,
    secrets: &dyn SecretLookup,
) -> Result<Expected, LoadError> {
    let mut resolver = Resolver::new();
    if let Some(environment) = environment {
        resolver.push(EnvironmentSource::new(environment, secrets));
    }
    specs
        .iter()
        .map(|(scheme, reference)| {
            let value = secret_name(reference).and_then(|name| resolve_secret(&resolver, &name));
            value.map(|v| (scheme.clone(), v)).map_err(|e| {
                let message = format!("[mock.security] {scheme}: {e}");
                LoadError::new(Path::new(SETTINGS_FILE), None, message)
            })
        })
        .collect()
}

/// The expected values for the workspace at `root`: the environment named
/// by `explicit`, else `env_var` (`RUSTMAN_ENV`), else the workspace
/// default, its secrets read from the store `open_secrets` opens. Nothing
/// is read when `specs` is empty.
pub fn load<S: SecretLookup>(
    root: &Path,
    specs: &BTreeMap<String, String>,
    explicit: Option<&str>,
    env_var: Option<&str>,
    open_secrets: impl FnOnce() -> Result<S, ConfigError>,
) -> Result<Expected, ConfigError> {
    if specs.is_empty() {
        return Ok(Expected::new());
    }
    let ws = Workspace::load(root)?;
    let active = ws.active_environment(explicit, env_var)?;
    let secrets = open_secrets()?;
    resolve(specs, active, &secrets).map_err(|e| ConfigError::Invalid(vec![e]))
}

#[cfg(test)]
mod tests {
    use super::*;
    use bytes::Bytes;
    use http::{HeaderMap, HeaderValue};
    use serde_json::json;

    fn model(
        schemes: Vec<(String, SecurityScheme)>,
        security: Vec<SecurityRequirement>,
    ) -> ApiModel {
        ApiModel {
            title: "t".into(),
            version: "1".into(),
            description: None,
            servers: Vec::new(),
            tags: Vec::new(),
            operations: Vec::new(),
            security,
            security_schemes: schemes,
            components: json!({}),
        }
    }

    fn operation(security: Option<Vec<SecurityRequirement>>) -> Operation {
        Operation {
            pointer: "/paths/~1x/get".into(),
            operation_id: None,
            method: "GET".into(),
            path: "/x".into(),
            summary: None,
            description: None,
            tags: Vec::new(),
            parameters: Vec::new(),
            request_body: None,
            responses: Vec::new(),
            security,
            deprecated: false,
        }
    }

    fn req(
        name: &str,
        scheme: SecurityScheme,
    ) -> (Vec<(String, SecurityScheme)>, Vec<SecurityRequirement>) {
        (
            vec![(name.into(), scheme)],
            vec![vec![(name.into(), Vec::new())]],
        )
    }

    fn run(
        model: &ApiModel,
        op: &Operation,
        headers: &[(&'static str, &'static str)],
        query: &str,
        expected: &Expected,
    ) -> Option<(Problem, Vec<String>)> {
        let mut map = HeaderMap::new();
        for (n, v) in headers {
            map.append(*n, HeaderValue::from_static(v));
        }
        let body = Bytes::new();
        let request = RequestView::new("GET", "/x", Some(query), &map, &body);
        check(model, op, &request, expected)
            .err()
            .map(|d| (d.problem, d.challenges))
    }

    #[test]
    fn bearer_basic_and_api_keys() {
        let none = Expected::new();
        let (schemes, security) = req("b", SecurityScheme::HttpBearer { format: None });
        let m = model(schemes, security);
        let op = operation(None);
        let (problem, challenges) = run(&m, &op, &[], "", &none).unwrap();
        assert_eq!(
            (problem.status, challenges),
            (401, vec!["Bearer".to_owned()])
        );
        assert!(run(&m, &op, &[("authorization", "bearer abc")], "", &none).is_none());
        assert!(run(&m, &op, &[("authorization", "Basic abc")], "", &none).is_some());
        assert!(run(&m, &operation(Some(Vec::new())), &[], "", &none).is_none());

        let (schemes, security) = req("basic", SecurityScheme::HttpBasic);
        let m = model(schemes, security);
        assert!(run(&m, &op, &[("authorization", "Basic YTpi")], "", &none).is_none());
        let (_, challenges) = run(&m, &op, &[("authorization", "Basic !!")], "", &none).unwrap();
        assert_eq!(challenges, ["Basic realm=\"rustman-mock\""]);

        let key = SecurityScheme::ApiKey {
            name: "api_key".into(),
            location: ParameterLocation::Query,
        };
        let (schemes, security) = req("key", key);
        let m = model(schemes, security);
        assert!(run(&m, &op, &[], "api_key=1", &none).is_none());
        assert!(run(&m, &op, &[], "api_key=", &none).is_some());
    }

    #[test]
    fn alternatives_and_oauth2_as_bearer() {
        let schemes = vec![
            (
                "oauth".to_owned(),
                SecurityScheme::OAuth2 { flows: json!({}) },
            ),
            (
                "key".to_owned(),
                SecurityScheme::ApiKey {
                    name: "X-Key".into(),
                    location: ParameterLocation::Header,
                },
            ),
        ];
        let security = vec![
            vec![("oauth".to_owned(), vec!["read".to_owned()])],
            vec![("key".to_owned(), Vec::new())],
        ];
        let m = model(schemes, security);
        let op = operation(None);
        let none = Expected::new();
        assert!(run(&m, &op, &[("x-key", "k")], "", &none).is_none());
        assert!(run(&m, &op, &[("authorization", "Bearer t")], "", &none).is_none());
        let (problem, challenges) = run(&m, &op, &[], "", &none).unwrap();
        assert_eq!(
            challenges,
            ["Bearer", "ApiKey name=\"X-Key\", in=\"header\""]
        );
        assert!(
            problem.detail.contains("oauth or key"),
            "{}",
            problem.detail
        );
    }

    #[test]
    fn expected_values_are_compared_only_when_configured() {
        let (schemes, security) = req("b", SecurityScheme::HttpBearer { format: None });
        let m = model(schemes, security);
        let op = operation(None);
        let mut expected = Expected::new();
        expected.insert("b".into(), Secret::new("token", "s3cret".to_owned()));
        assert!(
            run(
                &m,
                &op,
                &[("authorization", "Bearer s3cret")],
                "",
                &expected
            )
            .is_none()
        );
        let (problem, _) =
            run(&m, &op, &[("authorization", "Bearer nope")], "", &expected).unwrap();
        assert!(problem.detail.contains("do not match"));
        assert!(!problem.to_json().contains("s3cret"));
    }

    #[test]
    fn references_must_name_a_secret_variable() {
        let mut specs = BTreeMap::new();
        specs.insert("b".to_owned(), "plain-text".to_owned());
        let secrets = rustman_core::vars::NoSecrets;
        let err = resolve(&specs, None, &secrets).unwrap_err();
        assert!(err.to_string().contains("[mock.security] b"), "{err}");
    }
}
