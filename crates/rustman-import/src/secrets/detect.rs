//! What counts as a secret: by name (variables, headers, fields, keys), by
//! auth scheme (`Bearer x`), and by the shape of well-known token formats.

use std::sync::LazyLock;

use regex::Regex;

use crate::ir::Variable;

/// Variable, header, parameter and field names that hold secrets.
static SECRET_NAME: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)token|secret|password|passwd|api[_-]?key|client[_-]?secret")
        .expect("the secret-name pattern is valid")
});

/// Further names that usually carry credentials, beyond the spec pattern:
/// sessions, cookies, keys, OAuth authorization codes (`code`), signatures
/// (`sig`, `X-Sig`), one-time passwords and short password names (`pass`).
static EXTRA_SECRET_NAME: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?i)session|bearer|credential|cookie|jwt|access[_-]?key|private[_-]?key|^(auth|key|sid)$|^(auth[_-]?)?code$|(^|[_-])(sig|pass|otp)$|signature|passcode|passphrase",
    )
    .expect("the extra secret-name pattern is valid")
});

/// Whole words of a name that mark a secret: `pwd`, `pass`, `pin`, `otp`,
/// `sid`, PKCE `verifier`s and CSRF tokens. Matched against the name's
/// segments only (`X-Pwd`, `pinCode`, `code_verifier`), never as a
/// substring, so `spinner` or `bypass` stay plain.
const SECRET_SEGMENTS: &[&str] = &[
    "pwd", "pass", "passwd", "pin", "otp", "sid", "verifier", "csrf", "xsrf",
];

/// Header names ending in a credential word, such as `X-Custom-Auth` or
/// `X-Upload-Signature`.
static SECRET_HEADER_SUFFIX: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)-(auth|token|key|secret|signature)$")
        .expect("the secret-header pattern is valid")
});

/// Auth schemes whose value is a credential, wherever it appears.
const AUTH_SCHEMES: &[&str] = &[
    "bearer",
    "basic",
    "digest",
    "token",
    "apikey",
    "bot",
    "ssws",
    "negotiate",
    "ntlm",
];

/// One well-known token format: the variable name its values get, and
/// the pattern of the token.
struct TokenFormat {
    name: &'static str,
    pattern: &'static str,
}

/// High-confidence token formats, found in any text whatever its name.
const TOKEN_FORMATS: &[TokenFormat] = &[
    TokenFormat {
        name: "github_token",
        pattern: r"\b(?:gh[pousr]_[A-Za-z0-9]{20,}|github_pat_\w{20,})",
    },
    TokenFormat {
        name: "gitlab_token",
        pattern: r"\bglpat-[\w-]{20,}",
    },
    TokenFormat {
        name: "slack_token",
        pattern: r"\bxox[abposr]-[\w-]{10,}",
    },
    TokenFormat {
        name: "stripe_key",
        pattern: r"\b[sr]k_live_\w{10,}",
    },
    TokenFormat {
        name: "aws_access_key",
        pattern: r"\b(?:AKIA|ASIA)[0-9A-Z]{16}\b",
    },
    TokenFormat {
        name: "google_api_key",
        pattern: r"\bAIza[0-9A-Za-z_-]{35}",
    },
    TokenFormat {
        name: "jwt",
        pattern: r"\beyJ[\w-]{8,}\.eyJ[\w-]{8,}\.[\w-]+",
    },
    TokenFormat {
        name: "private_key",
        pattern: r"-----BEGIN [A-Z ]*PRIVATE KEY-----[\s\S]*?-----END [A-Z ]*PRIVATE KEY-----",
    },
];

/// All [`TOKEN_FORMATS`] in one pattern, one capture group per format.
static KNOWN_TOKEN: LazyLock<Regex> = LazyLock::new(|| {
    let groups: Vec<String> = TOKEN_FORMATS
        .iter()
        .map(|f| format!("({})", f.pattern))
        .collect();
    Regex::new(&groups.join("|")).expect("the known-token pattern is valid")
});

/// A well-known token found in a text.
pub struct Token {
    pub range: std::ops::Range<usize>,
    /// The variable name for its kind (`github_token`, `jwt`, ...).
    pub name: &'static str,
}

/// Every well-known token in `text`, in order.
pub fn known_tokens(text: &str) -> Vec<Token> {
    KNOWN_TOKEN
        .captures_iter(text)
        .filter_map(|c| {
            let (i, m) = c
                .iter()
                .skip(1)
                .enumerate()
                .find_map(|(i, m)| Some((i, m?)))?;
            Some(Token {
                range: m.range(),
                name: TOKEN_FORMATS[i].name,
            })
        })
        .collect()
}

/// Whether a variable, header or field called `name` holds a secret: the
/// spec pattern, session, cookie, credential and key-like names, and names
/// with a secret word as one of their segments.
pub fn is_secret_name(name: &str) -> bool {
    SECRET_NAME.is_match(name) || EXTRA_SECRET_NAME.is_match(name) || has_secret_segment(name)
}

/// Whether one segment of `name` (split at punctuation and lowercase to
/// uppercase changes) is a [`SECRET_SEGMENTS`] word.
fn has_secret_segment(name: &str) -> bool {
    segments(name).any(|s| SECRET_SEGMENTS.iter().any(|w| w.eq_ignore_ascii_case(s)))
}

/// `X-Api_pinCode` → `X`, `Api`, `pin`, `Code`.
fn segments(name: &str) -> impl Iterator<Item = &str> {
    let mut cuts = Vec::new();
    let mut prev: Option<char> = None;
    for (i, c) in name.char_indices() {
        if let Some(p) = prev
            && p.is_ascii_lowercase()
            && c.is_ascii_uppercase()
        {
            cuts.push(i);
        }
        prev = Some(c);
    }
    let mut start = 0;
    let mut pieces = Vec::new();
    for cut in cuts.into_iter().chain(std::iter::once(name.len())) {
        pieces.push(&name[start..cut]);
        start = cut;
    }
    pieces
        .into_iter()
        .flat_map(|p| p.split(|c: char| !c.is_ascii_alphanumeric()))
        .filter(|s| !s.is_empty())
}

/// Whether `value` contains a token of a well-known credential format.
pub fn is_known_token(value: &str) -> bool {
    KNOWN_TOKEN.is_match(value)
}

/// Whether the value `value` called `name` is a secret, by its name or by
/// its shape.
pub(super) fn is_secret(name: &str, value: &str) -> bool {
    is_secret_name(name) || is_known_token(value)
}

/// Whether `value` starts with an HTTP auth scheme and a credential
/// (`Bearer x`, `Token x`, `SSWS x`, ... any case; see [`AUTH_SCHEMES`]):
/// a credential whatever it is called.
pub fn is_auth_scheme_value(value: &str) -> bool {
    let Some((scheme, rest)) = value.trim_start().split_once(' ') else {
        return false;
    };
    !rest.trim().is_empty() && AUTH_SCHEMES.iter().any(|s| s.eq_ignore_ascii_case(scheme))
}

/// Whether the variable holds a secret: [`is_secret`], a value that starts
/// with an auth scheme, or the source marked it secret.
pub(super) fn is_secret_variable(var: &Variable) -> bool {
    var.secret || is_secret(&var.name, &var.value) || is_auth_scheme_value(&var.value)
}

/// Whether a header called `name` holds a secret: a secret name or a name
/// ending in `-auth`, `-token`, `-key`, `-secret` or `-signature`.
pub fn is_secret_header_name(name: &str) -> bool {
    is_secret_name(name) || SECRET_HEADER_SUFFIX.is_match(name)
}

/// Whether a script setting `name` sets a secret: a secret name, or a
/// credential header such as `Authorization`. Dotted names
/// (`headers.Authorization`) are judged by their last part.
pub(super) fn is_script_secret(name: &str) -> bool {
    let last = name.rsplit('.').next().unwrap_or(name);
    is_secret_name(last) || is_credential_header(last)
}

pub(super) fn is_credential_header(name: &str) -> bool {
    name.eq_ignore_ascii_case("authorization") || name.eq_ignore_ascii_case("proxy-authorization")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn secret_words_match_whole_segments_only() {
        for name in [
            "pwd",
            "X-Pwd",
            "db_pass",
            "pinCode",
            "user-pin",
            "otp",
            "sid",
            "code_verifier",
            "codeVerifier",
            "refresh_token",
            "private_key",
            "client_secret",
            "session",
            "cookie",
            "X-CSRF-Token",
        ] {
            assert!(is_secret_name(name), "{name}");
        }
        for name in [
            "spinner",
            "mapping",
            "bypass",
            "compass",
            "passenger",
            "side",
            "options",
            "status_code",
            "zip-code",
        ] {
            assert!(!is_secret_name(name), "{name}");
        }
    }

    #[test]
    fn auth_schemes_cover_token_apikey_bot_and_ssws() {
        for value in [
            "Bearer abc",
            "token abc",
            "ApiKey k",
            "Bot b",
            "SSWS s",
            "  Digest username=x",
        ] {
            assert!(is_auth_scheme_value(value), "{value}");
        }
        for value in ["Bearer", "Token ", "plain words", "Basically", ""] {
            assert!(!is_auth_scheme_value(value), "{value}");
        }
    }

    #[test]
    fn token_formats_are_found_with_their_kind() {
        let text = "a ghp_abcdefghijklmnopqrstuvwxyz0123456789 b \
                    ASIAABCDEFGHIJKLMNOP AIzaSyA1234567890abcdefghijklmnopqrstuv \
                    -----BEGIN RSA PRIVATE KEY-----\nMIIx\n-----END RSA PRIVATE KEY-----";
        let names: Vec<&str> = known_tokens(text).iter().map(|t| t.name).collect();
        assert_eq!(
            names,
            [
                "github_token",
                "aws_access_key",
                "google_api_key",
                "private_key"
            ]
        );
        assert!(known_tokens("ghp_short AKIA123 eyJhbGci").is_empty());
    }
}
