//! RFC 7616 Digest auth (runner-parity design D5): the challenge, the
//! response, and the nonces cached per (environment, host, realm).
//!
//! Supported: `MD5`, `MD5-sess`, `SHA-256`, `SHA-256-sess`, `qop=auth`, and
//! the RFC 2069 form without `qop`. `qop=auth-int` and `userhash` are not.

use std::collections::HashMap;
use std::sync::{Mutex, PoisonError};

use http::HeaderMap;
use http::header::WWW_AUTHENTICATE;
use md5::{Digest as _, Md5};
use ring::digest::{SHA256, digest};

use crate::error::RequestError;

/// The largest body a Digest request buffers to resend after the challenge.
pub(crate) const MAX_REPLAY_BYTES: u64 = 10 * 1024 * 1024;

/// The error for a body too large to resend.
pub(crate) fn body_too_large() -> RequestError {
    RequestError::Invalid("digest: body larger than 10 MiB cannot be replayed".into())
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Algorithm {
    Md5,
    Md5Sess,
    Sha256,
    Sha256Sess,
}

impl Algorithm {
    fn parse(name: &str) -> Result<Self, RequestError> {
        match name.to_ascii_uppercase().as_str() {
            "MD5" => Ok(Self::Md5),
            "MD5-SESS" => Ok(Self::Md5Sess),
            "SHA-256" => Ok(Self::Sha256),
            "SHA-256-SESS" => Ok(Self::Sha256Sess),
            _ => Err(RequestError::Auth(format!(
                "digest: unsupported algorithm {name}"
            ))),
        }
    }

    fn name(self) -> &'static str {
        match self {
            Self::Md5 => "MD5",
            Self::Md5Sess => "MD5-sess",
            Self::Sha256 => "SHA-256",
            Self::Sha256Sess => "SHA-256-sess",
        }
    }

    /// The order in which offered algorithms are chosen: SHA-256 first.
    fn preference(self) -> u8 {
        match self {
            Self::Sha256 | Self::Sha256Sess => 0,
            Self::Md5 | Self::Md5Sess => 1,
        }
    }

    fn is_session(self) -> bool {
        matches!(self, Self::Md5Sess | Self::Sha256Sess)
    }

    /// The lowercase hex hash of `data`.
    fn hash(self, data: &str) -> String {
        let bytes: Vec<u8> = match self {
            Self::Md5 | Self::Md5Sess => Md5::digest(data.as_bytes()).to_vec(),
            Self::Sha256 | Self::Sha256Sess => digest(&SHA256, data.as_bytes()).as_ref().to_vec(),
        };
        bytes.iter().map(|b| format!("{b:02x}")).collect()
    }
}

/// A `WWW-Authenticate: Digest` challenge.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Challenge {
    pub realm: String,
    pub nonce: String,
    pub opaque: Option<String>,
    pub algorithm: Algorithm,
    /// `qop=auth` was offered; `false` for the RFC 2069 form.
    pub qop_auth: bool,
    pub stale: bool,
}

impl Challenge {
    /// The Digest challenge among `headers`' `WWW-Authenticate` values, if
    /// there is one. A server may offer several (one per algorithm, RFC 7616
    /// §3.7): the first supported one wins, SHA-256 before MD5. When none is
    /// supported, the first one's error is returned.
    pub fn from_headers(headers: &HeaderMap) -> Result<Option<Self>, RequestError> {
        let mut parsed = headers
            .get_all(WWW_AUTHENTICATE)
            .iter()
            .filter_map(|v| v.to_str().ok())
            .flat_map(digest_params)
            .map(|params| Self::from_params(&params));
        let Some(first) = parsed.next() else {
            return Ok(None);
        };
        let best = std::iter::once(first.clone())
            .chain(parsed)
            .filter_map(Result::ok)
            .min_by_key(|c| c.algorithm.preference());
        best.map_or_else(|| first.map(Some), |c| Ok(Some(c)))
    }

    fn from_params(params: &HashMap<String, String>) -> Result<Self, RequestError> {
        let get = |name: &str| params.get(name).cloned();
        let qop = get("qop").unwrap_or_default();
        let offered: Vec<&str> = qop
            .split(',')
            .map(str::trim)
            .filter(|q| !q.is_empty())
            .collect();
        let qop_auth = offered.iter().any(|q| q.eq_ignore_ascii_case("auth"));
        if !offered.is_empty() && !qop_auth {
            return Err(RequestError::Auth(format!(
                "digest: unsupported qop {}",
                offered.join(", ")
            )));
        }
        if get("userhash").is_some_and(|v| v.eq_ignore_ascii_case("true")) {
            return Err(RequestError::Auth("digest: unsupported userhash".into()));
        }
        let algorithm = Algorithm::parse(get("algorithm").as_deref().unwrap_or("MD5"))?;
        Ok(Self {
            realm: get("realm").unwrap_or_default(),
            nonce: get("nonce")
                .ok_or_else(|| RequestError::Auth("digest: the challenge has no nonce".into()))?,
            opaque: get("opaque"),
            algorithm,
            qop_auth,
            stale: get("stale").is_some_and(|v| v.eq_ignore_ascii_case("true")),
        })
    }
}

/// The parameters of every `Digest` challenge in one header value, in order.
fn digest_params(value: &str) -> Vec<HashMap<String, String>> {
    let mut challenges = Vec::new();
    let mut rest = value;
    loop {
        let (scheme, after) = token(rest.trim_start_matches([' ', ',']));
        if scheme.is_empty() {
            return challenges;
        }
        let (params, next) = auth_params(after);
        if scheme.eq_ignore_ascii_case("Digest") {
            challenges.push(params);
        }
        rest = next;
    }
}

/// The leading token of `s` and what follows it.
fn token(s: &str) -> (&str, &str) {
    let end = s
        .find(|c: char| c.is_whitespace() || c == ',' || c == '=')
        .unwrap_or(s.len());
    s.split_at(end)
}

/// `name=value` pairs (lowercase names, unquoted values) up to the next
/// challenge, and the text from that challenge on.
fn auth_params(mut s: &str) -> (HashMap<String, String>, &str) {
    let mut params = HashMap::new();
    loop {
        let start = s.trim_start_matches([' ', '\t', ',']);
        let (name, after) = token(start);
        let Some(after) = after.trim_start().strip_prefix('=') else {
            return (params, start);
        };
        let (value, rest) = param_value(after.trim_start());
        params.insert(name.to_ascii_lowercase(), value);
        s = rest;
    }
}

/// A quoted string (with `\` escapes) or a token, and the text after it.
fn param_value(s: &str) -> (String, &str) {
    let Some(quoted) = s.strip_prefix('"') else {
        let end = s.find(',').unwrap_or(s.len());
        return (s[..end].trim().to_owned(), &s[end..]);
    };
    let mut value = String::new();
    let mut chars = quoted.char_indices();
    while let Some((i, c)) = chars.next() {
        match c {
            '\\' => value.extend(chars.next().map(|(_, c)| c)),
            '"' => return (value, &quoted[i + 1..]),
            c => value.push(c),
        }
    }
    (value, "")
}

/// What one `Authorization: Digest` answer is computed from.
pub(crate) struct Answer<'a> {
    pub username: &'a str,
    pub password: &'a str,
    pub method: &'a str,
    /// The request target: path and query.
    pub uri: &'a str,
    pub nc: u32,
    pub cnonce: &'a str,
}

impl Challenge {
    /// The `response` value: RFC 7616 section 3.4.1.
    pub fn response(&self, a: &Answer<'_>) -> String {
        let h = |data: &str| self.algorithm.hash(data);
        let mut ha1 = h(&format!("{}:{}:{}", a.username, self.realm, a.password));
        if self.algorithm.is_session() {
            ha1 = h(&format!("{ha1}:{}:{}", self.nonce, a.cnonce));
        }
        let ha2 = h(&format!("{}:{}", a.method, a.uri));
        if self.qop_auth {
            h(&format!(
                "{ha1}:{}:{:08x}:{}:auth:{ha2}",
                self.nonce, a.nc, a.cnonce
            ))
        } else {
            h(&format!("{ha1}:{}:{ha2}", self.nonce))
        }
    }

    /// The `Authorization` header value answering this challenge.
    pub fn authorization(&self, a: &Answer<'_>) -> String {
        let mut value = format!(
            "Digest username=\"{}\", realm=\"{}\", uri=\"{}\", algorithm={}, nonce=\"{}\"",
            quote(a.username),
            quote(&self.realm),
            quote(a.uri),
            self.algorithm.name(),
            quote(&self.nonce),
        );
        if self.qop_auth {
            value.push_str(&format!(
                ", nc={:08x}, cnonce=\"{}\", qop=auth",
                a.nc,
                quote(a.cnonce)
            ));
        }
        value.push_str(&format!(", response=\"{}\"", self.response(a)));
        if let Some(opaque) = &self.opaque {
            value.push_str(&format!(", opaque=\"{}\"", quote(opaque)));
        }
        value
    }
}

fn quote(s: &str) -> String {
    s.replace('\\', "\\\\").replace('"', "\\\"")
}

/// A fresh client nonce.
pub(crate) fn cnonce() -> String {
    uuid::Uuid::new_v4().simple().to_string()
}

/// Where a nonce is reused: environment, host (with port) and realm.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct Key {
    env: String,
    host: String,
    realm: String,
}

struct Cached {
    challenge: Challenge,
    nc: u32,
    /// When it was last used, to pick the newest realm for a host.
    used: u64,
}

/// Digest nonces and their counts, shared by the requests of one engine.
#[derive(Default)]
pub(crate) struct NonceCache {
    entries: Mutex<HashMap<Key, Cached>>,
    clock: Mutex<u64>,
}

impl NonceCache {
    /// The newest challenge cached for `host` in `env`, with its next nonce
    /// count, which is taken.
    pub fn next(&self, env: &str, host: &str) -> Option<(Challenge, u32)> {
        let tick = self.tick();
        let mut entries = self.entries.lock().unwrap_or_else(PoisonError::into_inner);
        let cached = entries
            .iter_mut()
            .filter(|(k, _)| k.env == env && k.host == host)
            .max_by_key(|(_, c)| c.used)
            .map(|(_, c)| c)?;
        cached.nc += 1;
        cached.used = tick;
        Some((cached.challenge.clone(), cached.nc))
    }

    /// Cache `challenge`, just answered with nonce count 1.
    pub fn store(&self, env: &str, host: &str, challenge: Challenge) {
        let key = Key {
            env: env.to_owned(),
            host: host.to_owned(),
            realm: challenge.realm.clone(),
        };
        let used = self.tick();
        let mut entries = self.entries.lock().unwrap_or_else(PoisonError::into_inner);
        entries.insert(
            key,
            Cached {
                challenge,
                nc: 1,
                used,
            },
        );
    }

    fn tick(&self) -> u64 {
        let mut clock = self.clock.lock().unwrap_or_else(PoisonError::into_inner);
        *clock += 1;
        *clock
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use http::HeaderValue;

    /// The inputs of RFC 7616 section 3.9.1.
    fn rfc_challenge(algorithm: &str) -> Challenge {
        let header = format!(
            "Digest realm=\"http-auth@example.org\", qop=\"auth, auth-int\", algorithm={algorithm}, \
             nonce=\"7ypf/xlj9XXwfDPEoM4URrv/xwf94BcCAzFZH4GiTo0v\", \
             opaque=\"FQhe/qaU925kfnzjCev0ciny7QMkPqMAFRtzCUYo5tdS\""
        );
        let mut headers = HeaderMap::new();
        headers.insert(WWW_AUTHENTICATE, HeaderValue::from_str(&header).unwrap());
        Challenge::from_headers(&headers).unwrap().unwrap()
    }

    fn rfc_answer() -> Answer<'static> {
        Answer {
            username: "Mufasa",
            password: "Circle of Life",
            method: "GET",
            uri: "/dir/index.html",
            nc: 1,
            cnonce: "f2/wE4q74E6zIJEtWaHKaf5wv/H5QzzpXusqGemxURZJ",
        }
    }

    #[test]
    fn the_rfc_7616_sha256_vector_matches() {
        let challenge = rfc_challenge("SHA-256");
        assert_eq!(
            challenge.response(&rfc_answer()),
            "753927fa0e85d155564e2e272a28d1802ca10daf4496794697cf8db5856cb6c1"
        );
    }

    #[test]
    fn the_rfc_7616_md5_vector_matches() {
        let challenge = rfc_challenge("MD5");
        assert_eq!(
            challenge.response(&rfc_answer()),
            "8ca523f5e9506fed4657c9700eebdbec"
        );
    }

    #[test]
    fn session_variants_hash_the_nonces_into_ha1() {
        // HA1 = H(H(user:realm:password):nonce:cnonce), computed by hand.
        let challenge = Challenge {
            algorithm: Algorithm::Md5Sess,
            ..rfc_challenge("MD5")
        };
        let a = rfc_answer();
        let h = |s: &str| Algorithm::Md5.hash(s);
        let ha1 = h(&format!(
            "{}:{}:{}",
            h("Mufasa:http-auth@example.org:Circle of Life"),
            challenge.nonce,
            a.cnonce
        ));
        let ha2 = h("GET:/dir/index.html");
        let expected = h(&format!(
            "{ha1}:{}:00000001:{}:auth:{ha2}",
            challenge.nonce, a.cnonce
        ));
        assert_eq!(challenge.response(&a), expected);
        let sha = Challenge {
            algorithm: Algorithm::Sha256Sess,
            ..challenge
        };
        assert_eq!(sha.response(&a).len(), 64);
    }

    #[test]
    fn the_authorization_header_carries_every_field() {
        let header = rfc_challenge("SHA-256").authorization(&rfc_answer());
        assert_eq!(
            header,
            "Digest username=\"Mufasa\", realm=\"http-auth@example.org\", uri=\"/dir/index.html\", \
             algorithm=SHA-256, nonce=\"7ypf/xlj9XXwfDPEoM4URrv/xwf94BcCAzFZH4GiTo0v\", \
             nc=00000001, cnonce=\"f2/wE4q74E6zIJEtWaHKaf5wv/H5QzzpXusqGemxURZJ\", qop=auth, \
             response=\"753927fa0e85d155564e2e272a28d1802ca10daf4496794697cf8db5856cb6c1\", \
             opaque=\"FQhe/qaU925kfnzjCev0ciny7QMkPqMAFRtzCUYo5tdS\""
        );
    }

    fn parse(values: &[&str]) -> Result<Option<Challenge>, RequestError> {
        let mut headers = HeaderMap::new();
        for value in values {
            headers.append(WWW_AUTHENTICATE, HeaderValue::from_str(value).unwrap());
        }
        Challenge::from_headers(&headers)
    }

    #[test]
    fn challenges_are_found_among_others() {
        let c = parse(&["Basic realm=\"x\", Digest realm=\"a\\\"b\", nonce=n1, stale=TRUE"])
            .unwrap()
            .unwrap();
        assert_eq!((c.realm.as_str(), c.nonce.as_str()), ("a\"b", "n1"));
        assert!(c.stale && !c.qop_auth);
        assert_eq!(c.algorithm, Algorithm::Md5);
        let c = parse(&["Bearer", "Digest nonce=\"n\", qop=\"auth\""])
            .unwrap()
            .unwrap();
        assert!(c.qop_auth);
        assert_eq!(parse(&["Basic realm=\"x\""]).unwrap(), None);
    }

    #[test]
    fn unsupported_challenges_are_errors() {
        let err = parse(&["Digest nonce=\"n\", qop=\"auth-int\""]).unwrap_err();
        assert_eq!(err.to_string(), "digest: unsupported qop auth-int");
        let err = parse(&["Digest nonce=\"n\", qop=auth, userhash=true"]).unwrap_err();
        assert_eq!(err.to_string(), "digest: unsupported userhash");
        let err = parse(&["Digest nonce=\"n\", algorithm=SHA-512-256"]).unwrap_err();
        assert_eq!(err.to_string(), "digest: unsupported algorithm SHA-512-256");
        let err = parse(&["Digest realm=\"r\""]).unwrap_err();
        assert_eq!(err.to_string(), "digest: the challenge has no nonce");
    }

    /// Regression: only the first `Digest` challenge was read, so a server
    /// offering an unsupported one first (or MD5 before SHA-256) failed or
    /// got the weaker answer.
    #[test]
    fn the_first_supported_challenge_wins_sha256_first() {
        let c = parse(&[
            "Digest realm=\"r\", nonce=\"a\", algorithm=SHA-512-256",
            "Digest realm=\"r\", nonce=\"b\", algorithm=MD5, qop=\"auth\"",
            "Digest realm=\"r\", nonce=\"c\", algorithm=SHA-256, qop=\"auth\"",
        ])
        .unwrap()
        .unwrap();
        assert_eq!((c.nonce.as_str(), c.algorithm), ("c", Algorithm::Sha256));
        let one_header = "Digest nonce=\"m\", algorithm=MD5, Digest nonce=\"s\", \
                          algorithm=SHA-256-sess";
        let c = parse(&[one_header]).unwrap().unwrap();
        assert_eq!(
            (c.nonce.as_str(), c.algorithm),
            ("s", Algorithm::Sha256Sess)
        );
        let c = parse(&[
            "Digest nonce=\"x\", qop=\"auth-int\"",
            "Digest nonce=\"m1\", algorithm=MD5",
            "Digest nonce=\"m2\", algorithm=MD5",
        ])
        .unwrap()
        .unwrap();
        assert_eq!(c.nonce, "m1", "the first supported one among equals");
        let err = parse(&[
            "Digest nonce=\"x\", algorithm=SHA-512-256",
            "Digest nonce=\"y\", qop=\"auth-int\"",
        ])
        .unwrap_err();
        assert_eq!(err.to_string(), "digest: unsupported algorithm SHA-512-256");
    }

    /// Regression: every unsupported qop was reported as `auth-int`.
    #[test]
    fn an_unknown_qop_is_reported_by_its_name() {
        let err = parse(&["Digest nonce=\"n\", qop=\"auth-conf\""]).unwrap_err();
        assert_eq!(err.to_string(), "digest: unsupported qop auth-conf");
        let err = parse(&["Digest nonce=\"n\", qop=\"auth-int, x-y\""]).unwrap_err();
        assert_eq!(err.to_string(), "digest: unsupported qop auth-int, x-y");
    }

    #[test]
    fn the_rfc_2069_form_has_no_counts() {
        let c = parse(&["Digest realm=\"r\", nonce=\"n\""])
            .unwrap()
            .unwrap();
        let header = c.authorization(&rfc_answer());
        assert!(
            !header.contains("nc=") && !header.contains("qop"),
            "{header}"
        );
        let h = |s: &str| Algorithm::Md5.hash(s);
        let expected = h(&format!(
            "{}:n:{}",
            h("Mufasa:r:Circle of Life"),
            h("GET:/dir/index.html")
        ));
        assert_eq!(c.response(&rfc_answer()), expected);
    }

    #[test]
    fn the_cache_counts_per_host_and_environment() {
        let cache = NonceCache::default();
        let challenge = rfc_challenge("MD5");
        assert!(cache.next("staging", "api:80").is_none());
        cache.store("staging", "api:80", challenge.clone());
        assert_eq!(cache.next("staging", "api:80").unwrap().1, 2);
        assert_eq!(cache.next("staging", "api:80").unwrap().1, 3);
        assert!(cache.next("prod", "api:80").is_none());
        assert!(cache.next("staging", "other:80").is_none());
        let newer = Challenge {
            realm: "other".into(),
            ..challenge
        };
        cache.store("staging", "api:80", newer);
        let (picked, nc) = cache.next("staging", "api:80").unwrap();
        assert_eq!((picked.realm.as_str(), nc), ("other", 2));
    }
}
