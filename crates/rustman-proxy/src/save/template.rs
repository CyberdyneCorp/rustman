//! Path templatization (spec: Path and id parameterization, design D5
//! step 2): segments that look like identifiers (integers, UUIDs, 24-hex
//! ObjectIds, ULIDs) become variables named after the previous segment in
//! singular form (`/users/42` -> `{{user_id}}`), or `{{id_N}}` when there
//! is no usable previous segment.

/// A path with its identifier segments replaced.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Templated {
    /// The path with `{{name}}` placeholders: `/users/{{user_id}}`.
    pub path: String,
    /// The path with `:id` in place of each identifier, for names and
    /// file names: `/users/:id`.
    pub shape: String,
    /// Each variable and the value it replaced, in path order.
    pub variables: Vec<(String, String)>,
}

/// Templatize `path` (no query), segments kept as written.
pub fn templatize(path: &str) -> Templated {
    let mut out = Templated {
        path: String::new(),
        shape: String::new(),
        variables: Vec::new(),
    };
    let mut previous: Option<&str> = None;
    let mut fallback = 0;
    for segment in path.split('/').skip(1) {
        out.path.push('/');
        out.shape.push('/');
        if !is_identifier(segment) {
            out.path.push_str(segment);
            out.shape.push_str(segment);
            previous = Some(segment);
            continue;
        }
        let name = match previous.and_then(singular_name) {
            Some(base) => unique(format!("{base}_id"), &out.variables),
            None => {
                fallback += 1;
                unique(format!("id_{fallback}"), &out.variables)
            }
        };
        out.path.push_str(&format!("{{{{{name}}}}}"));
        out.shape.push_str(":id");
        out.variables.push((name, segment.to_owned()));
        previous = None;
    }
    if out.path.is_empty() {
        out.path.push('/');
        out.shape.push('/');
    }
    out
}

/// `name`, or `name_2`, `name_3`... when the path already uses it.
fn unique(name: String, taken: &[(String, String)]) -> String {
    let used = |n: &str| taken.iter().any(|(t, _)| t == n);
    if !used(&name) {
        return name;
    }
    (2..)
        .map(|n| format!("{name}_{n}"))
        .find(|candidate| !used(candidate))
        .expect("some suffix is free")
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
}
