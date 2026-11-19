//! Where to authorize: the protected resource metadata (RFC 9728) named by
//! the server's `WWW-Authenticate` challenge (or at its well-known URL),
//! then the authorization server metadata (RFC 8414, with the OpenID
//! Connect discovery URL as fallback). Both are checked against what the
//! client asked for: the `resource` must be the MCP server's URL (or a
//! prefix of it on the same origin, RFC 9728 section 3.3), and the `issuer`
//! the authorization server's URL (RFC 8414 section 3.3), so a server
//! cannot obtain a token meant for another API. Every endpoint the
//! metadata names must use `https` (`http` only on a loopback host) before
//! anything is opened in the browser or sent to it, so a hostile server
//! cannot hand the OS a `file:` or custom-scheme URL, or have a code or
//! client secret sent in plain text.

use serde::Deserialize;
use url::Url;

use super::web::Web;

/// RFC 9728 protected resource metadata.
#[derive(Clone, Debug, Deserialize)]
pub(crate) struct ResourceMetadata {
    pub resource: Option<String>,
    #[serde(default)]
    pub authorization_servers: Vec<String>,
    #[serde(default)]
    pub scopes_supported: Vec<String>,
}

/// RFC 8414 authorization server metadata.
#[derive(Clone, Debug, Deserialize)]
pub(crate) struct ServerMetadata {
    pub issuer: Option<String>,
    pub authorization_endpoint: String,
    pub token_endpoint: String,
    pub registration_endpoint: Option<String>,
    #[serde(default)]
    pub code_challenge_methods_supported: Vec<String>,
}

/// The `resource_metadata` parameter of a `WWW-Authenticate` challenge.
pub(crate) fn resource_metadata_url(challenge: &str) -> Option<String> {
    let start = challenge.find("resource_metadata=")? + "resource_metadata=".len();
    let rest = &challenge[start..];
    let value = match rest.strip_prefix('"') {
        Some(quoted) => &quoted[..quoted.find('"')?],
        None => rest.split([',', ' ']).next()?,
    };
    Some(value.to_owned())
}

/// `<origin>/.well-known/<name><path>`, as RFC 8414 and 9728 build it.
fn well_known(url: &Url, name: &str) -> String {
    let path = url.path().trim_end_matches('/');
    format!(
        "{}/.well-known/{name}{path}",
        url.origin().ascii_serialization()
    )
}

fn parse_url(text: &str) -> Result<Url, String> {
    Url::parse(text).map_err(|e| format!("invalid URL {text}: {e}"))
}

/// The protected resource metadata of the MCP server at `server_url`.
pub(crate) async fn resource(
    web: &Web,
    challenge: Option<&str>,
    server_url: &str,
) -> Result<ResourceMetadata, String> {
    let url = match challenge.and_then(resource_metadata_url) {
        Some(url) => url,
        None => well_known(&parse_url(server_url)?, "oauth-protected-resource"),
    };
    web.get_json(&url)
        .await
        .map_err(|e| format!("cannot read protected resource metadata: {e}"))
}

/// The `resource` to ask a token for: the metadata's, once checked to
/// cover `server_url`, else `server_url` itself.
pub(crate) fn checked_resource(
    metadata: &ResourceMetadata,
    server_url: &str,
) -> Result<String, String> {
    let Some(resource) = &metadata.resource else {
        return Ok(server_url.to_owned());
    };
    let (Ok(advertised), Ok(server)) = (Url::parse(resource), Url::parse(server_url)) else {
        return Err(format!("invalid protected resource {resource}"));
    };
    if covers(&advertised, &server) {
        return Ok(resource.clone());
    }
    Err(format!(
        "the protected resource {resource} does not match the server URL {server_url}"
    ))
}

/// Whether `resource` names `server`: the same origin, and a path that is
/// the server's path or a parent of it.
fn covers(resource: &Url, server: &Url) -> bool {
    let parent = resource.path().trim_end_matches('/');
    let path = server.path().trim_end_matches('/');
    let under = path == parent || path.starts_with(&format!("{parent}/"));
    resource.origin() == server.origin() && resource.query().is_none() && under
}

/// `text` as a URL OAuth traffic may use: `https`, or `http` to a
/// loopback host (a local authorization server).
pub(crate) fn secure_url(text: &str) -> Result<Url, String> {
    let url = parse_url(text)?;
    let loopback = match url.host() {
        Some(url::Host::Domain(name)) => name.eq_ignore_ascii_case("localhost"),
        Some(url::Host::Ipv4(ip)) => ip.is_loopback(),
        Some(url::Host::Ipv6(ip)) => ip.is_loopback(),
        None => false,
    };
    match url.scheme() {
        "https" => Ok(url),
        "http" if loopback => Ok(url),
        _ => Err(format!(
            "refusing OAuth URL {text}: it must use https (http only on a loopback host)"
        )),
    }
}

/// The metadata, when every endpoint it names is a [`secure_url`].
fn check_endpoints(metadata: ServerMetadata) -> Result<ServerMetadata, String> {
    secure_url(&metadata.authorization_endpoint)?;
    secure_url(&metadata.token_endpoint)?;
    if let Some(registration) = &metadata.registration_endpoint {
        secure_url(registration)?;
    }
    Ok(metadata)
}

/// The metadata of the authorization server `issuer`.
pub(crate) async fn server(web: &Web, issuer: &str) -> Result<ServerMetadata, String> {
    let url = secure_url(issuer)?;
    let mut last = String::new();
    for name in ["oauth-authorization-server", "openid-configuration"] {
        match web
            .get_json::<ServerMetadata>(&well_known(&url, name))
            .await
        {
            Ok(metadata) => return check_issuer(metadata, issuer).and_then(check_endpoints),
            Err(e) => last = e,
        }
    }
    Err(format!("cannot read authorization server metadata: {last}"))
}

/// The metadata, when its `issuer` is the URL it was looked up for.
fn check_issuer(metadata: ServerMetadata, issuer: &str) -> Result<ServerMetadata, String> {
    let same = |a: &str| a.trim_end_matches('/') == issuer.trim_end_matches('/');
    match metadata.issuer.as_deref() {
        Some(answered) if same(answered) => Ok(metadata),
        Some(answered) => Err(format!(
            "the authorization server metadata names issuer {answered}, not {issuer}"
        )),
        None => Err(format!(
            "the authorization server metadata of {issuer} has no issuer"
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_challenge_names_the_metadata() {
        assert_eq!(
            resource_metadata_url(
                r#"Bearer error="invalid_token", resource_metadata="http://h/.well-known/oauth-protected-resource/mcp""#
            )
            .as_deref(),
            Some("http://h/.well-known/oauth-protected-resource/mcp")
        );
        assert_eq!(
            resource_metadata_url("Bearer resource_metadata=http://h/m, scope=x").as_deref(),
            Some("http://h/m")
        );
        assert_eq!(resource_metadata_url("Bearer realm=x"), None);
    }

    fn resource(value: Option<&str>) -> ResourceMetadata {
        ResourceMetadata {
            resource: value.map(str::to_owned),
            authorization_servers: Vec::new(),
            scopes_supported: Vec::new(),
        }
    }

    #[test]
    fn the_resource_must_cover_the_server_url() {
        let server = "https://mcp.example/tenant/mcp";
        let ok = |r: &str| checked_resource(&resource(Some(r)), server);
        assert_eq!(ok(server).unwrap(), server);
        assert_eq!(
            ok("https://mcp.example/tenant/mcp/").unwrap(),
            "https://mcp.example/tenant/mcp/"
        );
        assert_eq!(
            ok("https://mcp.example/tenant").unwrap(),
            "https://mcp.example/tenant"
        );
        assert_eq!(ok("https://mcp.example/").unwrap(), "https://mcp.example/");
        assert_eq!(checked_resource(&resource(None), server).unwrap(), server);
        for wrong in [
            "https://api.other.example/mcp",
            "https://mcp.example/tenant/mcp/other",
            "https://mcp.example/ten",
            "http://mcp.example/tenant/mcp",
            "https://mcp.example:8443/tenant/mcp",
            "https://mcp.example/tenant/mcp?x=1",
            "not a url",
        ] {
            assert!(ok(wrong).is_err(), "{wrong} must be refused");
        }
        let err = ok("https://api.other.example/mcp").unwrap_err();
        assert_eq!(
            err,
            "the protected resource https://api.other.example/mcp does not match the server URL https://mcp.example/tenant/mcp"
        );
    }

    fn metadata(issuer: Option<&str>) -> ServerMetadata {
        ServerMetadata {
            issuer: issuer.map(str::to_owned),
            authorization_endpoint: "https://as.example/authorize".into(),
            token_endpoint: "https://as.example/token".into(),
            registration_endpoint: None,
            code_challenge_methods_supported: Vec::new(),
        }
    }

    #[test]
    fn the_issuer_must_be_the_url_looked_up() {
        let issuer = "https://as.example/tenant";
        assert!(check_issuer(metadata(Some(issuer)), issuer).is_ok());
        assert!(check_issuer(metadata(Some("https://as.example/tenant/")), issuer).is_ok());
        let err = check_issuer(metadata(Some("https://evil.example")), issuer).unwrap_err();
        assert_eq!(
            err,
            "the authorization server metadata names issuer https://evil.example, not https://as.example/tenant"
        );
        assert!(check_issuer(metadata(None), issuer).is_err());
    }

    #[test]
    fn oauth_urls_must_be_https_or_loopback_http() {
        for ok in [
            "https://as.example/authorize",
            "http://127.0.0.1:8080/authorize",
            "http://[::1]/token",
            "http://localhost/register",
        ] {
            assert!(secure_url(ok).is_ok(), "{ok}");
        }
        for bad in [
            "http://as.example/authorize",
            "file:///etc/passwd",
            "smb://host/share",
            "calculator:",
            "javascript:alert(1)",
        ] {
            let err = secure_url(bad).unwrap_err();
            assert!(
                err.starts_with(&format!("refusing OAuth URL {bad}")),
                "{err}"
            );
        }
    }

    #[test]
    fn every_endpoint_of_the_metadata_is_checked() {
        let issuer = "https://as.example/tenant";
        assert!(check_endpoints(metadata(Some(issuer))).is_ok());
        let mut opener = metadata(Some(issuer));
        opener.authorization_endpoint = "file:///Applications/Calculator.app".into();
        assert!(check_endpoints(opener).is_err());
        let mut plain = metadata(Some(issuer));
        plain.token_endpoint = "http://as.example/token".into();
        assert!(check_endpoints(plain).is_err());
        let mut register = metadata(Some(issuer));
        register.registration_endpoint = Some("http://as.example/register".into());
        assert!(check_endpoints(register).is_err());
    }

    #[test]
    fn well_known_urls_insert_the_path() {
        let url = Url::parse("https://as.example/tenant/").unwrap();
        assert_eq!(
            well_known(&url, "oauth-authorization-server"),
            "https://as.example/.well-known/oauth-authorization-server/tenant"
        );
        let root = Url::parse("https://as.example").unwrap();
        assert_eq!(
            well_known(&root, "openid-configuration"),
            "https://as.example/.well-known/openid-configuration"
        );
    }
}
