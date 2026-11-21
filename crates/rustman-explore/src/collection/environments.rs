//! `servers` as environments: one per server, named from its description in
//! kebab-case (`server-<n>` otherwise), defining `baseUrl` with the server
//! variables substituted by their defaults, the variables themselves, and
//! the auth secrets as placeholders without values.

use rustman_core::Environment;
use rustman_core::model::{VarValue, Variables};

use super::names;
use super::request::BASE_URL;
use crate::openapi::model::{ApiModel, Server};
use crate::openapi::{Diagnostic, Location};

/// The environments for `model`'s servers. Relative server URLs are
/// resolved against `location` when the document came from a URL.
pub fn environments(
    model: &ApiModel,
    secrets: &[String],
    location: Option<&Location>,
    warnings: &mut Vec<Diagnostic>,
) -> Vec<Environment> {
    let mut taken: Vec<String> = Vec::new();
    let mut out = Vec::new();
    for (index, server) in model.servers.iter().enumerate() {
        let fallback = format!("server-{}", index + 1);
        let base = names::file_name(server.description.as_deref().unwrap_or(""), &fallback);
        let name = names::unique(&base, &taken);
        taken.push(name.clone());
        let base_url = base_url(server, location);
        if !base_url.contains("://") {
            let at = format!("/servers/{index}/url");
            warnings.push(Diagnostic::warning(
                at,
                format!("server URL '{base_url}' is relative; set 'baseUrl' in environment '{name}' by hand"),
            ));
        }
        out.push(Environment {
            name,
            variables: variables(server, base_url),
            secrets: secrets.to_vec(),
            production: false,
            trace: Default::default(),
            chain_id: None,
        });
    }
    out
}

/// The server URL with every `{variable}` replaced by its default, made
/// absolute against a URL source, without a trailing `/`.
fn base_url(server: &Server, location: Option<&Location>) -> String {
    let mut url = server.url.clone();
    for variable in &server.variables {
        url = url.replace(&format!("{{{}}}", variable.name), &variable.default);
    }
    if let Some(Location::Url(source)) = location
        && !url.contains("://")
        && let Ok(joined) = source.join(&url)
    {
        url = crate::openapi::source::shown(&joined);
    }
    while url.ends_with('/') && !url.ends_with("://") {
        url.pop();
    }
    url
}

fn variables(server: &Server, base_url: String) -> Variables {
    let mut entries = vec![(BASE_URL.to_owned(), VarValue::String(base_url))];
    for variable in &server.variables {
        if variable.name != BASE_URL {
            let value = VarValue::String(variable.default.clone());
            entries.push((variable.name.clone(), value));
        }
    }
    Variables::new(entries)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::openapi::model::ServerVariable;
    use serde_json::json;

    fn server(url: &str, description: Option<&str>) -> Server {
        Server {
            url: url.into(),
            description: description.map(str::to_owned),
            variables: Vec::new(),
        }
    }

    fn model(servers: Vec<Server>) -> ApiModel {
        ApiModel {
            title: "t".into(),
            version: "1".into(),
            description: None,
            servers,
            tags: Vec::new(),
            operations: Vec::new(),
            security: Vec::new(),
            security_schemes: Vec::new(),
            components: json!({}),
        }
    }

    #[test]
    fn two_servers_become_two_environments() {
        let m = model(vec![
            server("https://staging.example.com", Some("Staging")),
            server("https://api.example.com/", Some("Production")),
            server("https://other.example.com", None),
        ]);
        let mut warnings = Vec::new();
        let envs = environments(&m, &["t_token".into()], None, &mut warnings);
        let summary: Vec<(String, String)> = envs
            .iter()
            .map(|e| {
                (
                    e.name.clone(),
                    e.variables.get("baseUrl").unwrap().to_string(),
                )
            })
            .collect();
        assert_eq!(
            summary,
            [
                ("staging".into(), "https://staging.example.com".into()),
                ("production".into(), "https://api.example.com".into()),
                ("server-3".into(), "https://other.example.com".into()),
            ]
        );
        assert_eq!(envs[0].secrets, ["t_token"]);
        assert!(warnings.is_empty());
    }

    #[test]
    fn server_variables_are_substituted_and_exposed() {
        let mut s = server("https://{region}.example.com/{version}", Some("EU"));
        s.variables = vec![
            ServerVariable {
                name: "region".into(),
                default: "eu".into(),
                values: Vec::new(),
            },
            ServerVariable {
                name: "version".into(),
                default: "v2".into(),
                values: Vec::new(),
            },
        ];
        let envs = environments(&model(vec![s]), &[], None, &mut Vec::new());
        let vars: Vec<(&str, String)> = envs[0]
            .variables
            .iter()
            .map(|(n, v)| (n, v.to_string()))
            .collect();
        assert_eq!(
            vars,
            [
                ("baseUrl", "https://eu.example.com/v2".to_owned()),
                ("region", "eu".to_owned()),
                ("version", "v2".to_owned()),
            ]
        );
    }

    #[test]
    fn relative_urls_resolve_against_a_url_source_or_warn() {
        let m = model(vec![server("/v1", None)]);
        let source = Location::parse("https://ana:pw@api.example.com/docs/openapi.json");
        let envs = environments(&m, &[], Some(&source), &mut Vec::new());
        assert_eq!(
            envs[0].variables.get("baseUrl").unwrap().to_string(),
            "https://api.example.com/v1"
        );
        let mut warnings = Vec::new();
        environments(&m, &[], None, &mut warnings);
        assert_eq!(
            warnings[0].to_string(),
            "/servers/0/url: server URL '/v1' is relative; set 'baseUrl' in environment 'server-1' by hand"
        );
    }
}
