//! Which proxy a request goes through (design D2).
//!
//! Precedence: the request's `settings.proxy`, then `[http] proxy` from
//! `rustman.toml`, then the `HTTPS_PROXY`, `HTTP_PROXY`, `ALL_PROXY` and
//! `NO_PROXY` environment variables (either case). `none` bypasses every
//! proxy. `NO_PROXY` only applies to proxies taken from the environment.

use std::fmt;
use std::net::IpAddr;

use crate::error::RequestError;

/// How the client talks to a proxy.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ProxyKind {
    /// HTTP proxy: `CONNECT` tunnels, plain HTTP is forwarded.
    Http,
    /// The same, over TLS to the proxy.
    Https,
    /// SOCKS5; the proxy resolves the target host name.
    Socks5,
}

/// One proxy endpoint. `Debug` and `Display` never show credentials.
#[derive(Clone, PartialEq, Eq, Hash)]
pub struct ProxyUrl {
    pub kind: ProxyKind,
    pub host: String,
    pub port: u16,
    /// User name and password, percent-decoded.
    pub credentials: Option<(String, String)>,
}

impl ProxyUrl {
    /// Parse `scheme://[user:password@]host[:port]`; without a scheme,
    /// `http://` is assumed.
    pub fn parse(text: &str) -> Result<Self, RequestError> {
        let text = text.trim();
        let with_scheme = if text.contains("://") {
            text.to_owned()
        } else {
            format!("http://{text}")
        };
        let url = url::Url::parse(&with_scheme).map_err(|e| invalid_proxy(text, e))?;
        let (kind, default_port) = match url.scheme() {
            "http" => (ProxyKind::Http, 80),
            "https" => (ProxyKind::Https, 443),
            "socks5" | "socks5h" => (ProxyKind::Socks5, 1080),
            other => {
                return Err(invalid_proxy(text, format!("unsupported scheme '{other}'")));
            }
        };
        let host = url
            .host_str()
            .ok_or_else(|| invalid_proxy(text, "no host"))?
            .trim_start_matches('[')
            .trim_end_matches(']')
            .to_owned();
        Ok(Self {
            kind,
            host,
            port: url.port().unwrap_or(default_port),
            credentials: credentials(&url),
        })
    }
}

fn credentials(url: &url::Url) -> Option<(String, String)> {
    if url.username().is_empty() && url.password().is_none() {
        return None;
    }
    let decode = |s: &str| {
        percent_encoding::percent_decode_str(s)
            .decode_utf8_lossy()
            .into_owned()
    };
    Some((
        decode(url.username()),
        decode(url.password().unwrap_or_default()),
    ))
}

fn invalid_proxy(text: &str, err: impl fmt::Display) -> RequestError {
    // The text may hold credentials; only the part after `@` is shown.
    let shown = text.rsplit_once('@').map_or(text, |(_, host)| host);
    RequestError::Invalid(format!("invalid proxy URL '{shown}': {err}"))
}

impl fmt::Display for ProxyUrl {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let scheme = match self.kind {
            ProxyKind::Http => "http",
            ProxyKind::Https => "https",
            ProxyKind::Socks5 => "socks5",
        };
        if self.host.contains(':') {
            write!(f, "{scheme}://[{}]:{}", self.host, self.port)
        } else {
            write!(f, "{scheme}://{}:{}", self.host, self.port)
        }
    }
}

impl fmt::Debug for ProxyUrl {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "ProxyUrl({self})")
    }
}

/// The proxies for one request: one for `http` URLs, one for `https` URLs,
/// and the hosts that bypass them. The default is a direct connection.
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash)]
pub struct ProxySettings {
    pub http: Option<ProxyUrl>,
    pub https: Option<ProxyUrl>,
    pub no_proxy: NoProxy,
}

impl ProxySettings {
    /// Every request through `url`, or none at all for `none`.
    pub fn explicit(url: &str) -> Result<Self, RequestError> {
        if url.trim().eq_ignore_ascii_case("none") {
            return Ok(Self::default());
        }
        let proxy = ProxyUrl::parse(url)?;
        Ok(Self {
            http: Some(proxy.clone()),
            https: Some(proxy),
            no_proxy: NoProxy::default(),
        })
    }

    /// From the process environment.
    pub fn from_env() -> Result<Self, RequestError> {
        Self::from_env_with(|name| std::env::var(name).ok())
    }

    /// From environment variables read through `lookup`. Lowercase names win
    /// over uppercase ones; empty values are ignored.
    pub fn from_env_with(lookup: impl Fn(&str) -> Option<String>) -> Result<Self, RequestError> {
        let var = |name: &str| {
            [name.to_ascii_lowercase(), name.to_owned()]
                .iter()
                .filter_map(|n| lookup(n))
                .find(|v| !v.trim().is_empty())
        };
        let parse = |name: &str| var(name).map(|v| ProxyUrl::parse(&v)).transpose();
        let all = parse("ALL_PROXY")?;
        Ok(Self {
            http: parse("HTTP_PROXY")?.or_else(|| all.clone()),
            https: parse("HTTPS_PROXY")?.or(all),
            no_proxy: NoProxy::parse(&var("NO_PROXY").unwrap_or_default()),
        })
    }

    /// The first of the request's proxy, the workspace's, and the
    /// environment (read through `env`).
    pub fn select(
        request: Option<&str>,
        workspace: Option<&str>,
        env: impl Fn(&str) -> Option<String>,
    ) -> Result<Self, RequestError> {
        match request.or(workspace) {
            Some(url) => Self::explicit(url),
            None => Self::from_env_with(env),
        }
    }

    /// The proxy for a request to `host` over `https` or plain HTTP.
    pub fn route(&self, https: bool, host: &str) -> Option<&ProxyUrl> {
        let proxy = if https { &self.https } else { &self.http };
        proxy.as_ref().filter(|_| !self.no_proxy.matches(host))
    }
}

/// `NO_PROXY`: `*`, domain names (matching subdomains too, with or without a
/// leading `.`), IP addresses and CIDR ranges, separated by commas.
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash)]
pub struct NoProxy {
    all: bool,
    entries: Vec<Bypass>,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
enum Bypass {
    Domain(String),
    Network(IpAddr, u8),
}

impl NoProxy {
    pub fn parse(text: &str) -> Self {
        let mut no_proxy = Self::default();
        for entry in text.split(',').map(str::trim).filter(|e| !e.is_empty()) {
            if entry == "*" {
                no_proxy.all = true;
            } else {
                no_proxy.entries.push(Bypass::parse(entry));
            }
        }
        no_proxy
    }

    pub fn matches(&self, host: &str) -> bool {
        let host = host
            .trim_start_matches('[')
            .trim_end_matches(']')
            .to_ascii_lowercase();
        let ip = host.parse::<IpAddr>().ok();
        self.all || self.entries.iter().any(|e| e.matches(&host, ip))
    }
}

impl Bypass {
    fn parse(entry: &str) -> Self {
        let entry = entry.trim_start_matches('*').trim_start_matches('.');
        let (addr, prefix) = entry.split_once('/').unwrap_or((entry, ""));
        let addr = addr.trim_start_matches('[').trim_end_matches(']');
        match addr.parse::<IpAddr>() {
            Ok(ip) => {
                let max = if ip.is_ipv4() { 32 } else { 128 };
                let len = prefix.parse().map_or(max, |n: u8| n.min(max));
                Self::Network(ip, len)
            }
            Err(_) => Self::Domain(entry.to_ascii_lowercase()),
        }
    }

    fn matches(&self, host: &str, ip: Option<IpAddr>) -> bool {
        match (self, ip) {
            (Self::Domain(domain), _) => {
                host == domain
                    || host
                        .strip_suffix(domain.as_str())
                        .is_some_and(|rest| rest.ends_with('.'))
            }
            (Self::Network(net, len), Some(ip)) => same_prefix(*net, ip, *len),
            (Self::Network(..), None) => false,
        }
    }
}

fn same_prefix(net: IpAddr, ip: IpAddr, len: u8) -> bool {
    let (a, b, bits) = match (net, ip) {
        (IpAddr::V4(a), IpAddr::V4(b)) => (u32::from(a).into(), u32::from(b).into(), 32),
        (IpAddr::V6(a), IpAddr::V6(b)) => (u128::from(a), u128::from(b), 128),
        _ => return false,
    };
    let shift = bits - u32::from(len);
    shift >= bits || (a >> shift) == (b >> shift)
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;

    fn env(vars: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
        let map: HashMap<String, String> = vars
            .iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
            .collect();
        move |name| map.get(name).cloned()
    }

    #[test]
    fn parses_schemes_ports_and_credentials() {
        let p = ProxyUrl::parse("http://us%40r:p%3Aw@127.0.0.1:3128").unwrap();
        assert_eq!(
            (p.kind, p.host.as_str(), p.port),
            (ProxyKind::Http, "127.0.0.1", 3128)
        );
        assert_eq!(p.credentials, Some(("us@r".into(), "p:w".into())));
        assert_eq!(format!("{p:?}"), "ProxyUrl(http://127.0.0.1:3128)");
        let p = ProxyUrl::parse("socks5h://[::1]").unwrap();
        assert_eq!((p.kind, p.port), (ProxyKind::Socks5, 1080));
        assert_eq!(p.to_string(), "socks5://[::1]:1080");
        let p = ProxyUrl::parse("proxy.local:8080").unwrap();
        assert_eq!((p.kind, p.port), (ProxyKind::Http, 8080));
        assert_eq!(ProxyUrl::parse("https://p").unwrap().port, 443);
    }

    #[test]
    fn invalid_proxies_hide_credentials() {
        let err = ProxyUrl::parse("ftp://user:s3cr3t@p:21").unwrap_err();
        assert_eq!(
            err.to_string(),
            "invalid proxy URL 'p:21': unsupported scheme 'ftp'"
        );
    }

    #[test]
    fn environment_variables_pick_a_proxy_per_scheme() {
        let s = ProxySettings::from_env_with(env(&[
            ("HTTPS_PROXY", "http://secure:1"),
            ("http_proxy", "http://plain:2"),
            ("HTTP_PROXY", "http://ignored:3"),
        ]))
        .unwrap();
        assert_eq!(s.route(true, "x").unwrap().host, "secure");
        assert_eq!(s.route(false, "x").unwrap().host, "plain");
        let s = ProxySettings::from_env_with(env(&[("all_proxy", "socks5://s:9")])).unwrap();
        assert_eq!(s.route(true, "x").unwrap().kind, ProxyKind::Socks5);
        assert_eq!(s.route(false, "x").unwrap().port, 9);
        let s = ProxySettings::from_env_with(env(&[("HTTPS_PROXY", "  ")])).unwrap();
        assert_eq!(s, ProxySettings::default());
    }

    #[test]
    fn no_proxy_bypasses_matching_hosts() {
        let s = ProxySettings::from_env_with(env(&[
            ("HTTPS_PROXY", "http://127.0.0.1:3128"),
            ("NO_PROXY", "internal.local, .corp,10.0.0.0/8,::1"),
        ]))
        .unwrap();
        for host in [
            "api.internal.local",
            "internal.local",
            "a.b.corp",
            "10.2.3.4",
            "[::1]",
        ] {
            assert!(s.route(true, host).is_none(), "{host}");
        }
        for host in [
            "notinternal.local",
            "corp.example",
            "11.0.0.1",
            "example.com",
        ] {
            assert!(s.route(true, host).is_some(), "{host}");
        }
    }

    #[test]
    fn a_star_bypasses_everything() {
        let no_proxy = NoProxy::parse("*");
        assert!(no_proxy.matches("anything.example"));
        assert!(!NoProxy::parse("").matches("anything.example"));
    }

    #[test]
    fn cidr_prefixes_compare_the_leading_bits() {
        let ip = |s: &str| s.parse::<IpAddr>().unwrap();
        assert!(same_prefix(ip("192.168.0.0"), ip("192.168.255.1"), 16));
        assert!(!same_prefix(ip("192.168.0.0"), ip("192.169.0.1"), 16));
        assert!(same_prefix(ip("1.2.3.4"), ip("9.9.9.9"), 0));
        assert!(!same_prefix(ip("::1"), ip("1.2.3.4"), 0));
    }

    #[test]
    fn explicit_and_workspace_proxies_win_over_the_environment() {
        let vars = env(&[("HTTPS_PROXY", "http://env:1"), ("NO_PROXY", "*")]);
        let s = ProxySettings::select(Some("http://req:2"), Some("http://ws:3"), &vars).unwrap();
        assert_eq!(s.route(true, "x").unwrap().host, "req");
        let s = ProxySettings::select(None, Some("http://ws:3"), &vars).unwrap();
        assert_eq!(s.route(true, "x").unwrap().host, "ws");
        let s = ProxySettings::select(Some("none"), Some("http://ws:3"), &vars).unwrap();
        assert_eq!(s, ProxySettings::default());
        let s = ProxySettings::select(None, None, &vars).unwrap();
        assert!(s.route(true, "x").is_none());
    }
}
