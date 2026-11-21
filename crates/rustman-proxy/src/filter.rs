//! Which exchanges are kept in the capture list (spec: Capture filters).
//! Filtering only decides what is recorded: every request is still proxied.
//!
//! - `--include-host` / `--exclude-host`: host globs, case-insensitive.
//! - `--method`: methods, case-insensitive.
//! - `--content-type`: prefixes of the request or response media type.
//! - Static assets (images, fonts, CSS, JavaScript, media, source maps) are
//!   dropped by response content type or URL extension unless `keep_static`.

use globset::{GlobBuilder, GlobSet, GlobSetBuilder};

use crate::capture::{Exchange, Recorded};

/// URL extensions of static assets.
const STATIC_EXTENSIONS: &[&str] = &[
    "png", "jpg", "jpeg", "gif", "svg", "ico", "webp", "avif", "bmp", "woff", "woff2", "ttf",
    "otf", "eot", "css", "js", "mjs", "map", "mp4", "webm", "ogg", "mp3", "wav", "m4a", "mov",
];

/// The rules from the filter flags; the default keeps everything but
/// static assets.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FilterRules {
    pub include_hosts: Vec<String>,
    pub exclude_hosts: Vec<String>,
    pub methods: Vec<String>,
    pub content_types: Vec<String>,
    pub keep_static: bool,
}

/// [`FilterRules`] with their globs compiled.
#[derive(Clone, Debug)]
pub struct CaptureFilter {
    include: Option<GlobSet>,
    exclude: Option<GlobSet>,
    methods: Vec<String>,
    content_types: Vec<String>,
    keep_static: bool,
}

impl Default for CaptureFilter {
    fn default() -> Self {
        Self::new(&FilterRules::default()).expect("no globs to compile")
    }
}

impl CaptureFilter {
    /// The filter of `rules`; an invalid glob is reported with its flag.
    pub fn new(rules: &FilterRules) -> Result<Self, String> {
        Ok(Self {
            include: globs(&rules.include_hosts, "--include-host")?,
            exclude: globs(&rules.exclude_hosts, "--exclude-host")?,
            methods: rules
                .methods
                .iter()
                .map(|m| m.to_ascii_uppercase())
                .collect(),
            content_types: rules
                .content_types
                .iter()
                .map(|t| t.trim().to_ascii_lowercase())
                .collect(),
            keep_static: rules.keep_static,
        })
    }

    /// Whether `recorded` goes to the capture list.
    pub fn accepts(&self, recorded: &Recorded) -> bool {
        match recorded {
            Recorded::Exchange(exchange) => self.accepts_exchange(exchange),
            Recorded::Tunnel(tunnel) => self.accepts_host(&tunnel.host),
            Recorded::TlsRejected { host, .. } => self.accepts_host(host),
        }
    }

    fn accepts_exchange(&self, exchange: &Exchange) -> bool {
        let host = host_of(&exchange.url);
        let method_ok = self.methods.is_empty() || self.methods.contains(&exchange.method);
        self.accepts_host(&host)
            && method_ok
            && self.accepts_content_type(exchange)
            && (self.keep_static || !is_static(exchange))
    }

    /// Whether `host` (no port) passes the host globs.
    pub fn accepts_host(&self, host: &str) -> bool {
        let included = self.include.as_ref().is_none_or(|set| set.is_match(host));
        let excluded = self.exclude.as_ref().is_some_and(|set| set.is_match(host));
        included && !excluded
    }

    fn accepts_content_type(&self, exchange: &Exchange) -> bool {
        if self.content_types.is_empty() {
            return true;
        }
        [
            header(&exchange.request_headers, "content-type"),
            header(&exchange.response_headers, "content-type"),
        ]
        .into_iter()
        .flatten()
        .map(|t| t.trim().to_ascii_lowercase())
        .any(|t| self.content_types.iter().any(|p| t.starts_with(p.as_str())))
    }
}

fn globs(patterns: &[String], flag: &str) -> Result<Option<GlobSet>, String> {
    if patterns.is_empty() {
        return Ok(None);
    }
    let mut builder = GlobSetBuilder::new();
    for pattern in patterns {
        let glob = GlobBuilder::new(pattern)
            .case_insensitive(true)
            .build()
            .map_err(|e| format!("record: invalid {flag} glob {pattern:?}: {e}"))?;
        builder.add(glob);
    }
    builder
        .build()
        .map(Some)
        .map_err(|e| format!("record: {e}"))
}

/// The first value of the header `name` in `headers`.
pub(crate) fn header<'a>(headers: &'a [(String, String)], name: &str) -> Option<&'a str> {
    headers
        .iter()
        .find(|(n, _)| n.eq_ignore_ascii_case(name))
        .map(|(_, v)| v.as_str())
}

/// The host of an absolute URL, without port, user info or IPv6 brackets.
pub(crate) fn host_of(url: &str) -> String {
    let rest = url.split_once("://").map_or(url, |(_, rest)| rest);
    let authority = rest.split(['/', '?', '#']).next().unwrap_or_default();
    let authority = authority.rsplit_once('@').map_or(authority, |(_, h)| h);
    let host = match authority.strip_prefix('[') {
        Some(v6) => v6.split(']').next().unwrap_or_default(),
        None => authority.split(':').next().unwrap_or_default(),
    };
    host.to_ascii_lowercase()
}

/// An image, font, stylesheet, script, media file or source map, by
/// response content type or URL extension.
pub fn is_static(exchange: &Exchange) -> bool {
    let mime = header(&exchange.response_headers, "content-type")
        .unwrap_or_default()
        .split(';')
        .next()
        .unwrap_or_default()
        .trim()
        .to_ascii_lowercase();
    static_type(&mime) || static_extension(&exchange.url)
}

fn static_type(mime: &str) -> bool {
    ["image/", "font/", "audio/", "video/"]
        .iter()
        .any(|p| mime.starts_with(p))
        || mime == "text/css"
        || mime.ends_with("javascript")
        || mime.ends_with("ecmascript")
}

fn static_extension(url: &str) -> bool {
    let rest = url.split_once("://").map_or(url, |(_, rest)| rest);
    let path = rest.split(['?', '#']).next().unwrap_or_default();
    let Some((_, last)) = path.rsplit_once('/') else {
        return false;
    };
    last.rsplit_once('.')
        .is_some_and(|(_, ext)| STATIC_EXTENSIONS.contains(&ext.to_ascii_lowercase().as_str()))
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;
    use crate::capture::{BodyCapture, Tunnel};

    fn exchange(method: &str, url: &str, response_type: &str) -> Recorded {
        let mut response_headers = Vec::new();
        if !response_type.is_empty() {
            response_headers.push(("Content-Type".to_owned(), response_type.to_owned()));
        }
        Recorded::Exchange(Box::new(Exchange {
            method: method.into(),
            url: url.into(),
            version: "HTTP/1.1".into(),
            request_headers: Vec::new(),
            request_body: BodyCapture::default(),
            status: Some(200),
            response_headers,
            response_body: BodyCapture::default(),
            duration: Duration::ZERO,
            intercepted: false,
            error: None,
        }))
    }

    fn filter(rules: FilterRules) -> CaptureFilter {
        CaptureFilter::new(&rules).unwrap()
    }

    fn strings(list: &[&str]) -> Vec<String> {
        list.iter().map(|&s| s.to_owned()).collect()
    }

    #[test]
    fn static_assets_are_dropped_by_type_or_extension() {
        let cases = [
            ("http://a.test/app.js", "", false),
            ("http://a.test/logo.png?v=2", "", false),
            ("http://a.test/logo", "image/png", false),
            ("http://a.test/f.woff2", "", false),
            ("http://a.test/x.css", "text/css", false),
            ("http://a.test/v", "video/mp4", false),
            ("http://a.test/app.js.map", "application/json", false),
            (
                "http://a.test/a",
                "application/javascript; charset=utf-8",
                false,
            ),
            ("http://a.test/api/items", "application/json", true),
            ("http://a.test/api.v1/items", "application/json", true),
            ("http://a.test", "", true),
        ];
        let default = CaptureFilter::default();
        let keep = filter(FilterRules {
            keep_static: true,
            ..FilterRules::default()
        });
        for (url, mime, kept) in cases {
            let recorded = exchange("GET", url, mime);
            assert_eq!(default.accepts(&recorded), kept, "{url} {mime}");
            assert!(keep.accepts(&recorded), "--keep-static keeps {url}");
        }
    }

    #[test]
    fn host_method_and_content_type_filters() {
        let rules = FilterRules {
            include_hosts: strings(&["*.example.com"]),
            exclude_hosts: strings(&["ads.example.com"]),
            methods: strings(&["get", "POST"]),
            content_types: strings(&["application/json"]),
            keep_static: false,
        };
        let f = filter(rules);
        let json = "application/json; charset=utf-8";
        let cases = [
            ("GET", "https://api.example.com/v1", json, true),
            ("GET", "https://API.Example.com:8443/v1", json, true),
            ("POST", "https://api.example.com/v1", json, true),
            ("DELETE", "https://api.example.com/v1", json, false),
            ("GET", "https://ads.example.com/v1", json, false),
            ("GET", "https://www.google.com/", json, false),
            ("GET", "https://api.example.com/page", "text/html", false),
            ("GET", "https://api.example.com/none", "", false),
        ];
        for (method, url, mime, kept) in cases {
            assert_eq!(
                f.accepts(&exchange(method, url, mime)),
                kept,
                "{method} {url} {mime}"
            );
        }
    }

    #[test]
    fn tunnels_and_rejections_follow_the_host_filters() {
        let f = filter(FilterRules {
            exclude_hosts: strings(&["*.google.com"]),
            ..FilterRules::default()
        });
        let tunnel = |host: &str| {
            Recorded::Tunnel(Tunnel {
                host: host.into(),
                port: 443,
                bytes_up: 0,
                bytes_down: 0,
                duration: Duration::ZERO,
                error: None,
            })
        };
        assert!(!f.accepts(&tunnel("www.google.com")));
        assert!(f.accepts(&tunnel("api.example.com")));
        let rejected = Recorded::TlsRejected {
            host: "mail.google.com".into(),
            port: 443,
        };
        assert!(!f.accepts(&rejected));
    }

    #[test]
    fn hosts_come_from_urls() {
        assert_eq!(host_of("https://u:p@API.test:8443/a?b"), "api.test");
        assert_eq!(host_of("http://[::1]:9000/x"), "::1");
        assert_eq!(host_of("http://a.test"), "a.test");
    }

    #[test]
    fn invalid_globs_name_their_flag() {
        let err = CaptureFilter::new(&FilterRules {
            exclude_hosts: strings(&["a["]),
            ..FilterRules::default()
        })
        .unwrap_err();
        assert!(
            err.starts_with("record: invalid --exclude-host glob"),
            "{err}"
        );
    }
}
