//! Origins: the scheme, host and port a request goes to.

use std::fmt;

use serde::Deserialize;
use url::Url;

/// The scheme, host and port of a URL. Two URLs share an origin only when all
/// three are equal; the port is the scheme's default when the URL has none.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Origin {
    scheme: String,
    host: String,
    port: u16,
}

impl Origin {
    /// The origin of an absolute `http` or `https` URL.
    pub fn of(url: &str) -> Result<Self, String> {
        let parsed = Url::parse(url).map_err(|e| format!("invalid URL '{url}': {e}"))?;
        Self::of_url(&parsed).ok_or_else(|| format!("'{url}' is not an http or https URL"))
    }

    /// Parse an origin written as `scheme://host[:port]`, as in
    /// `[safety].allowed_hosts` and `--allow-host`. A path, query, fragment or
    /// credentials are rejected so that an entry can't look narrower than it is.
    pub fn parse(text: &str) -> Result<Self, String> {
        let expected = || {
            format!(
                "invalid origin '{text}': expected scheme://host[:port], such as https://api.example.com"
            )
        };
        let parsed = Url::parse(text).map_err(|_| expected())?;
        let bare = parsed.path() == "/"
            && parsed.query().is_none()
            && parsed.fragment().is_none()
            && parsed.username().is_empty()
            && parsed.password().is_none();
        match Self::of_url(&parsed) {
            Some(origin) if bare => Ok(origin),
            _ => Err(expected()),
        }
    }

    fn of_url(url: &Url) -> Option<Self> {
        let scheme = url.scheme();
        if scheme != "http" && scheme != "https" {
            return None;
        }
        Some(Self {
            scheme: scheme.to_owned(),
            host: super::production::normal_host(url.host_str()?),
            port: url.port_or_known_default()?,
        })
    }

    pub fn scheme(&self) -> &str {
        &self.scheme
    }

    /// The host, lowercased and without a trailing dot (`api.example.com.`
    /// is `api.example.com`); IPv6 addresses keep their brackets.
    pub fn host(&self) -> &str {
        &self.host
    }

    pub fn port(&self) -> u16 {
        self.port
    }
}

/// `scheme://host:port`, always with the port.
impl fmt::Display for Origin {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}://{}:{}", self.scheme, self.host, self.port)
    }
}

impl<'de> Deserialize<'de> for Origin {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let text = String::deserialize(deserializer)?;
        Self::parse(&text).map_err(serde::de::Error::custom)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_port_defaults_to_the_scheme_default() {
        let origin = Origin::of("https://API.Example.com/users?page=2").unwrap();
        assert_eq!(origin.scheme(), "https");
        assert_eq!(origin.host(), "api.example.com");
        assert_eq!(origin.port(), 443);
        assert_eq!(origin, Origin::of("https://api.example.com:443/").unwrap());
        assert_eq!(Origin::of("http://h/").unwrap().port(), 80);
        assert_eq!(origin.to_string(), "https://api.example.com:443");
    }

    #[test]
    fn scheme_host_and_port_all_count() {
        let base = Origin::of("https://api.example.com/").unwrap();
        for other in [
            "http://api.example.com/",
            "https://api.example.com:8443/",
            "https://other.example.net/",
            "https://api.example.com.evil.net/",
        ] {
            assert_ne!(base, Origin::of(other).unwrap(), "{other}");
        }
    }

    #[test]
    fn a_trailing_dot_names_the_same_origin() {
        let origin = Origin::of("https://api.example.com./x").unwrap();
        assert_eq!(origin.host(), "api.example.com");
        assert_eq!(origin, Origin::of("https://api.example.com/").unwrap());
        assert_eq!(origin, Origin::parse("https://API.example.com.").unwrap());
    }

    #[test]
    fn ipv6_hosts_keep_brackets() {
        let origin = Origin::of("http://[::1]:8080/x").unwrap();
        assert_eq!(origin.host(), "[::1]");
        assert_eq!(origin.port(), 8080);
    }

    #[test]
    fn non_http_urls_have_no_origin() {
        for url in ["ftp://h/x", "file:///etc/passwd", "not a url", "/relative"] {
            assert!(Origin::of(url).is_err(), "{url}");
        }
    }

    #[test]
    fn parse_accepts_only_bare_origins() {
        assert_eq!(
            Origin::parse("https://staging.example.com:8443").unwrap(),
            Origin::of("https://staging.example.com:8443/").unwrap()
        );
        assert!(Origin::parse("https://staging.example.com/").is_ok());
        for bad in [
            "staging.example.com",
            "https://staging.example.com/v1",
            "https://staging.example.com?x=1",
            "https://user@staging.example.com",
            "ftp://staging.example.com",
            "",
        ] {
            let err = Origin::parse(bad).unwrap_err();
            assert!(
                err.contains("expected scheme://host[:port]"),
                "{bad}: {err}"
            );
        }
    }
}
