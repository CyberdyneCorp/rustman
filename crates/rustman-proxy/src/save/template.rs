//! Path templatization (spec: Path and id parameterization, design D5
//! step 2): segments that look like identifiers (integers, UUIDs, 24-hex
//! ObjectIds, ULIDs) become variables named after the previous segment in
//! singular form (`/users/42` -> `{{user_id}}`), or `{{id_N}}` when there
//! is no usable previous segment.
//!
//! Segments that look like secrets become secret variables instead, so
//! their values reach neither the URL, the file name nor the committed
//! environment (issue #104):
//!
//! - a segment holding a well-known token (`ghp_...`), or a compact random
//!   token (see [`is_secret_segment`]), such as the last segment of a
//!   Slack webhook `/services/T0.../B0.../<secret>`: `{{service_secret}}`
//!   after a plain word, `{{path_secret}}` otherwise;
//! - an unguessable identifier (UUID, ULID) after a segment that names a
//!   credential or a one-time link (`/reset/<uuid>`, `/tokens/<uuid>`);
//! - a token in any case (hex, lowercase and digits) after such a segment
//!   (`/password/reset/<64 hex>`: `{{reset_secret}}`), see
//!   [`is_capability_token`].
//!
//! Random-looking segments no rule catches are reported by the save's
//! entropy warnings.

use rustman_import::secrets::{is_known_token, is_secret_name};

/// A path with its identifier segments replaced.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Templated {
    /// The path with `{{name}}` placeholders: `/users/{{user_id}}`.
    pub path: String,
    /// The path with `:id` in place of each identifier (and `:secret` in
    /// place of each secret), for names and file names: `/users/:id`.
    pub shape: String,
    /// Each variable and the value it replaced, in path order.
    pub variables: Vec<(String, String)>,
    /// Each secret variable and the value it replaced, in path order.
    pub secrets: Vec<(String, String)>,
}

/// Templatize `path` (no query), segments kept as written.
pub fn templatize(path: &str) -> Templated {
    let mut out = Templated {
        path: String::new(),
        shape: String::new(),
        variables: Vec::new(),
        secrets: Vec::new(),
    };
    let mut previous: Option<&str> = None;
    let mut fallback = 0;
    for segment in path.split('/').skip(1) {
        out.path.push('/');
        out.shape.push('/');
        let identifier = is_identifier(segment);
        let capability_token =
            previous.is_some_and(names_a_capability) && is_capability_token(segment);
        if !identifier && (capability_token || is_secret_segment(segment)) {
            let base = previous
                .filter(|p| {
                    p.bytes()
                        .all(|b| b.is_ascii_alphabetic() || b == b'-' || b == b'_')
                })
                .and_then(singular_name)
                .unwrap_or_else(|| "path".to_owned());
            let name = out.unique(format!("{base}_secret"));
            out.path.push_str(&format!("{{{{{name}}}}}"));
            out.shape.push_str(":secret");
            out.secrets.push((name, segment.to_owned()));
            previous = None;
            continue;
        }
        if !identifier {
            out.path.push_str(segment);
            out.shape.push_str(segment);
            previous = Some(segment);
            continue;
        }
        let name = match previous.and_then(singular_name) {
            Some(base) => out.unique(format!("{base}_id")),
            None => {
                fallback += 1;
                out.unique(format!("id_{fallback}"))
            }
        };
        out.path.push_str(&format!("{{{{{name}}}}}"));
        out.shape.push_str(":id");
        let capability = previous.is_some_and(names_a_capability) && is_unguessable_id(segment);
        let list = if capability {
            &mut out.secrets
        } else {
            &mut out.variables
        };
        list.push((name, segment.to_owned()));
        previous = None;
    }
    if out.path.is_empty() {
        out.path.push('/');
        out.shape.push('/');
    }
    out
}

impl Templated {
    /// `name`, or `name_2`, `name_3`... when the path already uses it.
    fn unique(&self, name: String) -> String {
        let used = |n: &str| {
            self.variables
                .iter()
                .chain(&self.secrets)
                .any(|(t, _)| t == n)
        };
        if !used(&name) {
            return name;
        }
        (2..)
            .map(|n| format!("{name}_{n}"))
            .find(|candidate| !used(candidate))
            .expect("some suffix is free")
    }
}

/// Path segments after which an unguessable id is a credential: secret
/// names (`tokens`, `sessions`, `api-keys`) and the words of one-time links.
const CAPABILITY_WORDS: &[&str] = &[
    "reset",
    "resets",
    "verify",
    "verification",
    "verifications",
    "confirm",
    "confirmation",
    "confirmations",
    "activate",
    "activation",
    "activations",
    "invite",
    "invites",
    "invitation",
    "invitations",
    "magic",
    "magic-link",
    "magic-links",
    "recover",
    "recovery",
    "unsubscribe",
];

fn names_a_capability(segment: &str) -> bool {
    is_secret_name(segment)
        || CAPABILITY_WORDS
            .iter()
            .any(|w| w.eq_ignore_ascii_case(segment))
}

/// Whether `segment` is a UUID or a ULID: an id that cannot be guessed,
/// and so may work as a credential when it reaches a committed file.
pub fn is_unguessable_id(segment: &str) -> bool {
    is_uuid(segment) || is_ulid(segment)
}

/// Whether a path segment (not an identifier) looks like a secret: it
/// holds a well-known token, or it is a compact random token: at least 20
/// letters, digits, `-` or `_`, with uppercase and lowercase letters and
/// digits, less than half of it made of words (runs of three lowercase
/// letters or more, capitalized or not). So camelCase names such as
/// `listOrganizationMembersV2`, lowercase slugs and hex hashes stay plain.
pub fn is_secret_segment(segment: &str) -> bool {
    if is_known_token(segment) {
        return true;
    }
    let bytes = segment.as_bytes();
    if bytes.len() < 20
        || !bytes
            .iter()
            .all(|b| b.is_ascii_alphanumeric() || *b == b'-' || *b == b'_')
    {
        return false;
    }
    let has = |f: fn(&u8) -> bool| bytes.iter().any(f);
    if !(has(u8::is_ascii_uppercase) && has(u8::is_ascii_lowercase) && has(u8::is_ascii_digit)) {
        return false;
    }
    word_bytes(bytes) * 2 < bytes.len()
}

/// Whether a segment after a credential or one-time-link word
/// (`/reset/<token>`, `/verify/<token>`) is a token, in any case: at least
/// 16 letters, digits, `-` or `_`, with letters and digits, less than half
/// of it made of words. So the hex and lowercase tokens that
/// [`is_secret_segment`] leaves alone are caught there (a 64-hex Laravel
/// reset token), while `/reset/password-form` stays plain.
fn is_capability_token(segment: &str) -> bool {
    let bytes = segment.as_bytes();
    bytes.len() >= 16
        && bytes
            .iter()
            .all(|b| b.is_ascii_alphanumeric() || *b == b'-' || *b == b'_')
        && bytes.iter().any(u8::is_ascii_digit)
        && bytes.iter().any(u8::is_ascii_alphabetic)
        && word_bytes(bytes) * 2 < bytes.len()
}

/// The number of bytes of `bytes` in words: runs of three lowercase
/// letters or more, with the uppercase letter just before them.
fn word_bytes(bytes: &[u8]) -> usize {
    let mut total = 0;
    let mut i = 0;
    while i < bytes.len() {
        let start = i;
        if bytes[i].is_ascii_uppercase() {
            i += 1;
        }
        let lower_start = i;
        while i < bytes.len() && bytes[i].is_ascii_lowercase() {
            i += 1;
        }
        if i - lower_start >= 3 {
            total += i - start;
        }
        if i == start {
            i += 1;
        }
    }
    total
}

/// Whether `segment` is an integer, a UUID, an ObjectId or a ULID.
pub fn is_identifier(segment: &str) -> bool {
    is_integer(segment) || is_uuid(segment) || is_object_id(segment) || is_ulid(segment)
}

fn is_integer(s: &str) -> bool {
    !s.is_empty() && s.len() <= 20 && s.bytes().all(|b| b.is_ascii_digit())
}

fn is_uuid(s: &str) -> bool {
    let groups: Vec<&str> = s.split('-').collect();
    let lengths = [8, 4, 4, 4, 12];
    groups.len() == lengths.len()
        && groups
            .iter()
            .zip(lengths)
            .all(|(g, n)| g.len() == n && g.bytes().all(|b| b.is_ascii_hexdigit()))
}

fn is_object_id(s: &str) -> bool {
    s.len() == 24 && s.bytes().all(|b| b.is_ascii_hexdigit())
}

/// 26 Crockford base32 characters, the first at most `7`, with at least
/// one digit and one letter (so a plain 26-letter word is not an id).
fn is_ulid(s: &str) -> bool {
    const ALPHABET: &[u8] = b"0123456789ABCDEFGHJKMNPQRSTVWXYZ";
    let upper = s.to_ascii_uppercase();
    upper.len() == 26
        && upper.as_bytes()[0] <= b'7'
        && upper.bytes().all(|b| ALPHABET.contains(&b))
        && upper.bytes().any(|b| b.is_ascii_digit())
        && upper.bytes().any(|b| b.is_ascii_alphabetic())
}

/// The variable base for a collection segment: `users` -> `user`,
/// `categories` -> `category`; `None` when nothing usable is left.
fn singular_name(segment: &str) -> Option<String> {
    let name = rustman_import::slug::var_name(segment, "");
    if name.is_empty() || name.starts_with(|c: char| c.is_ascii_digit()) {
        return None;
    }
    Some(singular(&name))
}

fn singular(word: &str) -> String {
    if let Some(stem) = word.strip_suffix("ies").filter(|s| !s.is_empty()) {
        return format!("{stem}y");
    }
    let sibilant = ["sses", "xes", "zes", "ches", "shes"];
    if sibilant.iter().any(|end| word.ends_with(end)) {
        return word[..word.len() - 2].to_owned();
    }
    let keeps = ["ss", "us", "is"].iter().any(|end| word.ends_with(end));
    if word.ends_with('s') && !keeps && word.len() > 1 {
        return word[..word.len() - 1].to_owned();
    }
    word.to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A path, its templated form, its shape and its variables.
    type Case<'a> = (&'a str, &'a str, &'a str, &'a [(&'a str, &'a str)]);

    #[test]
    fn identifier_segments_become_named_variables() {
        let uuid = "9b2c4d1e-7f3a-4b5c-8d9e-0f1a2b3c4d5e";
        let cases: &[Case<'_>] = &[
            (
                "/users/42",
                "/users/{{user_id}}",
                "/users/:id",
                &[("user_id", "42")],
            ),
            (
                &format!("/orders/{uuid}/items"),
                "/orders/{{order_id}}/items",
                "/orders/:id/items",
                &[("order_id", uuid)],
            ),
            (
                "/posts/507f1f77bcf86cd799439011",
                "/posts/{{post_id}}",
                "/posts/:id",
                &[("post_id", "507f1f77bcf86cd799439011")],
            ),
            (
                "/events/01ARZ3NDEKTSV4RRFFQ69G5FAV",
                "/events/{{event_id}}",
                "/events/:id",
                &[("event_id", "01ARZ3NDEKTSV4RRFFQ69G5FAV")],
            ),
            (
                "/categories/3",
                "/categories/{{category_id}}",
                "/categories/:id",
                &[("category_id", "3")],
            ),
            (
                "/addresses/3",
                "/addresses/{{address_id}}",
                "/addresses/:id",
                &[("address_id", "3")],
            ),
            (
                "/status/3",
                "/status/{{status_id}}",
                "/status/:id",
                &[("status_id", "3")],
            ),
            ("/42", "/{{id_1}}", "/:id", &[("id_1", "42")]),
            (
                "/a/1/2",
                "/a/{{a_id}}/{{id_1}}",
                "/a/:id/:id",
                &[("a_id", "1"), ("id_1", "2")],
            ),
            (
                "/users/1/users/2",
                "/users/{{user_id}}/users/{{user_id_2}}",
                "/users/:id/users/:id",
                &[("user_id", "1"), ("user_id_2", "2")],
            ),
            ("/v1/users", "/v1/users", "/v1/users", &[]),
            ("/", "/", "/", &[]),
            ("", "/", "/", &[]),
        ];
        for (path, templated, shape, vars) in cases {
            let t = templatize(path);
            assert_eq!(t.path, *templated, "{path}");
            assert_eq!(t.shape, *shape, "{path}");
            let vars: Vec<(String, String)> = vars
                .iter()
                .map(|(n, v)| ((*n).to_owned(), (*v).to_owned()))
                .collect();
            assert_eq!(t.variables, vars, "{path}");
        }
    }

    #[test]
    fn identifier_kinds() {
        for id in [
            "0",
            "17",
            "9B2C4D1E-7F3A-4B5C-8D9E-0F1A2B3C4D5E",
            "507f1f77bcf86cd799439011",
            "01arz3ndektsv4rrffq69g5fav",
        ] {
            assert!(is_identifier(id), "{id}");
        }
        for word in [
            "users",
            "v1",
            "",
            "abcdefghijklmnopqrstuvwxyz",
            "9b2c4d1e-7f3a-4b5c-8d9e",
            "507f1f77bcf86cd79943901z",
            "81ARZ3NDEKTSV4RRFFQ69G5FAV",
            "123456789012345678901",
        ] {
            assert!(!is_identifier(word), "{word}");
        }
    }

    const SLACK_SECRET: &str = "Xk7pQ2mZ9rT4vB8nL3wY6sJd";
    const RESET: &str = "9b2c4d1e-7f3a-4b5c-8d9e-0f1a2b3c4d5e";

    #[test]
    fn secret_looking_segments_become_secret_variables() {
        let t = templatize(&format!("/services/T0AAAAAAA/B0BBBBBBB/{SLACK_SECRET}"));
        assert_eq!(t.path, "/services/T0AAAAAAA/B0BBBBBBB/{{path_secret}}");
        assert_eq!(t.shape, "/services/T0AAAAAAA/B0BBBBBBB/:secret");
        assert_eq!(
            t.secrets,
            [("path_secret".to_owned(), SLACK_SECRET.to_owned())]
        );
        assert!(t.variables.is_empty());

        let t = templatize(&format!("/hooks/{SLACK_SECRET}/{SLACK_SECRET}x"));
        assert_eq!(t.path, "/hooks/{{hook_secret}}/{{path_secret}}");

        let token = "ghp_q8ZK3vLw0pTn6RxYb2HsJm9CdFgA5eUi7oQ1";
        let t = templatize(&format!("/repos/{token}/x"));
        assert_eq!(t.path, "/repos/{{repo_secret}}/x");
        assert_eq!(t.secrets[0].1, token);
    }

    #[test]
    fn unguessable_ids_after_credential_words_are_secret() {
        let t = templatize(&format!("/password/reset/{RESET}"));
        assert_eq!(t.path, "/password/reset/{{reset_id}}");
        assert_eq!(t.shape, "/password/reset/:id");
        assert_eq!(t.secrets, [("reset_id".to_owned(), RESET.to_owned())]);
        assert!(t.variables.is_empty());

        let t = templatize("/tokens/01ARZ3NDEKTSV4RRFFQ69G5FAV");
        assert_eq!(t.secrets.len(), 1, "{t:?}");
        // A numeric id names a record, not a credential.
        let t = templatize("/api-keys/3/reset/42");
        assert!(t.secrets.is_empty(), "{t:?}");
        assert_eq!(t.variables.len(), 2);
        // Hex and lowercase tokens after such a word are secrets too.
        let hex = "5f4dcc3b5aa765d61d8327deb882cf995f4dcc3b5aa765d61d8327deb882cf99";
        let t = templatize(&format!("/password/reset/{hex}"));
        assert_eq!(t.path, "/password/reset/{{reset_secret}}");
        assert_eq!(t.shape, "/password/reset/:secret");
        assert_eq!(t.secrets, [("reset_secret".to_owned(), hex.to_owned())]);
        let t = templatize("/verify/k3j4h5g6f7d8s9a0q1w2e3r4t5y6u7i8");
        assert_eq!(t.path, "/verify/{{verify_secret}}");
        for plain in [
            "/reset/password-form",
            "/verify/email-address-2024",
            &format!("/commits/{hex}"),
        ] {
            let t = templatize(plain);
            assert!(t.secrets.is_empty(), "{plain}: {t:?}");
        }
        // A UUID under an ordinary collection stays a plain variable.
        let t = templatize(&format!("/orders/{RESET}"));
        assert!(t.secrets.is_empty());
        assert_eq!(t.variables, [("order_id".to_owned(), RESET.to_owned())]);
    }

    #[test]
    fn ordinary_segments_are_not_secrets() {
        for segment in [
            "listOrganizationMembersV2",
            "getUserProfileSettings2024",
            "iPhone15ProMaxSpecificationsAndReviews",
            "my-blog-post-title-2024-01-15",
            "da39a3ee5e6b4b0d3255bfef95601890afd80709",
            "DA39A3EE5E6B4B0D3255BFEF95601890AFD80709",
            "T0AAAAAAA",
            "B0BBBBBBB",
            "dQw4w9WgXcQ",
            "users",
            "v1",
            "index.html",
            "ApplicationSettingsV2Beta3",
        ] {
            assert!(!is_secret_segment(segment), "{segment}");
        }
        for segment in [
            SLACK_SECRET,
            "abcDEF123ghiJKL456mnoPQR",
            "q8ZK3vLw0pTn6RxYb2HsJm9CdFgA5eUi7oQ1",
        ] {
            assert!(is_secret_segment(segment), "{segment}");
        }
    }
}
