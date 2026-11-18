//! Security headers: HSTS on HTTPS, `X-Content-Type-Options: nosniff`, and
//! CORS that allows any origin with credentials.

use http::HeaderMap;

use super::{Detected, Observed};

pub fn missing(response: &Observed<'_>) -> Vec<Detected> {
    let headers = response.headers;
    let mut found = Vec::new();
    if response.https && !headers.contains_key(http::header::STRICT_TRANSPORT_SECURITY) {
        found.push(Detected::new(
            "missing-hsts",
            "no Strict-Transport-Security header",
        ));
    }
    let nosniff = value(headers, http::header::X_CONTENT_TYPE_OPTIONS.as_str())
        .is_some_and(|v| v.eq_ignore_ascii_case("nosniff"));
    if !nosniff {
        found.push(Detected::new(
            "missing-content-type-options",
            "no X-Content-Type-Options: nosniff header",
        ));
    }
    let any_origin = value(headers, "access-control-allow-origin") == Some("*");
    let credentials = value(headers, "access-control-allow-credentials")
        .is_some_and(|v| v.eq_ignore_ascii_case("true"));
    if any_origin && credentials {
        found.push(Detected::new(
            "permissive-cors",
            "Access-Control-Allow-Origin: * with Access-Control-Allow-Credentials: true",
        ));
    }
    found
}

fn value<'a>(headers: &'a HeaderMap, name: &str) -> Option<&'a str> {
    headers
        .get(name)
        .and_then(|v| v.to_str().ok())
        .map(str::trim)
}

#[cfg(test)]
mod tests {
    use super::*;
    use http::HeaderValue;

    fn rules(headers: &HeaderMap, https: bool) -> Vec<&'static str> {
        let observed = Observed {
            status: 200,
            headers,
            body: b"",
            https,
        };
        missing(&observed).into_iter().map(|d| d.rule).collect()
    }

    #[test]
    fn each_header_rule() {
        let mut headers = HeaderMap::new();
        assert_eq!(rules(&headers, false), ["missing-content-type-options"]);
        assert_eq!(
            rules(&headers, true),
            ["missing-hsts", "missing-content-type-options"]
        );
        headers.insert(
            "x-content-type-options",
            HeaderValue::from_static("nosniff"),
        );
        headers.insert(
            "strict-transport-security",
            HeaderValue::from_static("max-age=1"),
        );
        assert!(rules(&headers, true).is_empty());
        headers.insert("access-control-allow-origin", HeaderValue::from_static("*"));
        assert!(rules(&headers, true).is_empty());
        headers.insert(
            "access-control-allow-credentials",
            HeaderValue::from_static("true"),
        );
        assert_eq!(rules(&headers, true), ["permissive-cors"]);
    }
}
