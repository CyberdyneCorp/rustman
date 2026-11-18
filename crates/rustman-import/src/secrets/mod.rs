//! Secret extraction (design D2, task 1.4): the last transform before
//! writing. Literal secret values are replaced by `{{name}}` placeholders and
//! handed out as [`Secret`]s, to be stored as `environments` secrets.

mod detect;
mod scope;
mod scrub;
mod text;

use std::collections::HashMap;
use std::ops::Range;

use rustman_core::vars::{Segment, Template};
use rustman_core::{Environment, Secret};

use crate::ir::{
    Folder, ImportedAuth, ImportedBody, ImportedCollection, ImportedEnvironment, ImportedPart,
    ImportedRequest, Item, Location, Note, NoteKind, Pair, PartValue, Script, Variable,
};
use crate::slug::{slug, var_name};

pub use detect::{
    is_auth_scheme_value, is_known_token, is_secret_header_name, is_secret_name, known_tokens,
};
use detect::{is_credential_header, is_script_secret, is_secret, is_secret_variable};
use scope::Renames;
pub use scrub::{MIN_REPEAT, redact_tokens};

/// The environment file name (`environments/<name>.yaml`) for a source
/// environment or collection called `display`.
pub fn environment_name(display: &str) -> String {
    slug(display, "imported")
}

/// A secret moved out of the converted files.
#[derive(Clone, Debug)]
pub struct MovedSecret {
    pub secret: Secret<String>,
    /// The environment that lists it under `secrets`.
    pub environment: String,
    pub location: Location,
    /// A variable of a source environment file (subject to the conflict
    /// policy), rather than a value found in a request or collection.
    pub from_environment: bool,
}

/// What the workspace already holds, to reuse or avoid secret names.
pub struct Existing<'a> {
    pub environments: &'a [Environment],
    /// The stored value of a declared secret (environment, name).
    pub lookup: &'a dyn Fn(&str, &str) -> Option<Secret<String>>,
}

impl Existing<'_> {
    /// No environments and no stored secrets.
    pub fn none() -> Existing<'static> {
        Existing {
            environments: &[],
            lookup: &|_, _| None,
        }
    }
}

/// Replace every literal secret in `collection` by a placeholder. Secrets
/// found in requests and collection or folder variables go to `target`;
/// those of source environments stay in their own environment.
pub fn extract(
    collection: &mut ImportedCollection,
    target: &str,
    existing: &Existing<'_>,
) -> Vec<MovedSecret> {
    let mut extractor = Extractor::new(target, existing);
    extractor.scoped_variables(&mut collection.root, &Renames::new());
    extractor.folder(&mut collection.root);
    let mut notes = Vec::new();
    extractor.folder_scripts(&collection.root, &mut notes);
    for env in &mut collection.environments {
        extractor.environment(env);
    }
    collection.notes.extend(notes);
    collection.notes.extend(review_notes(collection));
    extractor.scrub(collection);
    extractor.moved
}

/// Whether extraction would still find a literal secret in `request`: the
/// writer's guard, so a parser bug cannot put a secret in a file.
pub fn request_has_literal(request: &ImportedRequest) -> bool {
    let existing = Existing::none();
    let mut probe = Extractor::new("", &existing);
    probe.request(&mut request.clone());
    !probe.moved.is_empty()
}

/// Whether extraction would still find a literal secret in the auth,
/// headers or variables of `folder` (not its items).
pub fn folder_has_literal(folder: &Folder) -> bool {
    let existing = Existing::none();
    let mut probe = Extractor::new("", &existing);
    let mut copy = Folder {
        items: Vec::new(),
        ..folder.clone()
    };
    probe.scoped_variables(&mut copy, &Renames::new());
    probe.folder(&mut copy);
    !probe.moved.is_empty()
}

struct Extractor<'a> {
    target: String,
    existing: &'a Existing<'a>,
    /// Names given out by this import, per environment, with their values.
    assigned: HashMap<(String, String), String>,
    /// The name each (environment, base name, value) was given, so a
    /// repeated value reuses its name without probing.
    by_value: HashMap<(String, String, String), String>,
    /// Per (environment, base name), the first suffix not yet given out by
    /// this import: every candidate below it is taken here, so probing
    /// starts there and naming stays linear.
    next_free: HashMap<(String, String), usize>,
    /// The first name each value was moved under, any base.
    names_by_value: HashMap<String, String>,
    moved: Vec<MovedSecret>,
}

/// Whether a candidate name can hold a value.
enum Slot {
    Free,
    SameValue,
    Taken,
}

impl<'a> Extractor<'a> {
    fn new(target: &str, existing: &'a Existing<'a>) -> Self {
        Self {
            target: target.to_owned(),
            existing,
            assigned: HashMap::new(),
            by_value: HashMap::new(),
            next_free: HashMap::new(),
            names_by_value: HashMap::new(),
            moved: Vec::new(),
        }
    }

    fn folder(&mut self, folder: &mut Folder) {
        let at = folder.location.clone();
        self.auth(folder.auth.as_mut(), &at);
        self.headers(&mut folder.headers, &at);
        for item in &mut folder.items {
            match item {
                Item::Folder(child) => self.folder(child),
                Item::Request(request) => self.request(request),
            }
        }
    }

    fn request(&mut self, request: &mut ImportedRequest) {
        let at = request.location.clone();
        self.url(&mut request.url, "url", &at);
        self.headers(&mut request.headers, &at);
        self.pairs(&mut request.query, &at);
        self.auth(request.auth.as_mut(), &at);
        self.body(request.body.as_mut(), &at);
        self.request_variables(request);
    }

    /// The password (or token) in the userinfo of `url`, named
    /// `<prefix>_password` (or `<prefix>_token`).
    fn url(&mut self, url: &mut String, prefix: &str, at: &Location) {
        let Some(range) = text::userinfo_credentials(url) else {
            return;
        };
        let kind = if url[..range.start].ends_with(':') {
            "password"
        } else {
            "token"
        };
        let value = url[range.clone()].to_owned();
        let base = format!("{prefix}_{kind}");
        if let Some(placeholder) = self.placeholder(&value, &base, at) {
            url.replace_range(range, &placeholder);
        }
    }

    fn body(&mut self, body: Option<&mut ImportedBody>, at: &Location) {
        match body {
            Some(ImportedBody::Form { fields }) => self.pairs(fields, at),
            Some(ImportedBody::Multipart { parts }) => self.parts(parts, at),
            Some(ImportedBody::Json { text }) => self.embedded(text, text::key_values, at),
            Some(ImportedBody::Text { text }) => {
                self.embedded(text, text::key_values, at);
                self.embedded(text, text::form_values, at);
            }
            Some(ImportedBody::Graphql {
                variables: Some(variables),
                ..
            }) => self.embedded(variables, text::key_values, at),
            _ => {}
        }
    }

    fn parts(&mut self, parts: &mut [ImportedPart], at: &Location) {
        for part in parts.iter_mut() {
            if let PartValue::Text(value) = &mut part.value
                && is_secret(&part.name, value)
            {
                self.replace(value, &var_name(&part.name, "secret"), at);
            }
        }
    }

    /// Secret-named values inside a body's text, as `find` locates them.
    fn embedded(&mut self, text: &mut String, find: Finder, at: &Location) {
        let mut edits: Vec<(Range<usize>, String)> = Vec::new();
        let all = find(text, &|_| true);
        for found in all.into_iter().filter(|f| is_secret(&f.name, &f.value)) {
            let base = var_name(&found.name, "secret");
            if let Some(placeholder) = self.placeholder(&found.value, &base, at) {
                edits.push((found.range, placeholder));
            }
        }
        if !edits.is_empty() {
            edits.sort_by_key(|(range, _)| range.start);
            *text = splice(text, edits);
        }
    }

    /// Request-scoped variables (Postman path variables, Bruno request
    /// vars) move like collection variables; references follow renames.
    fn request_variables(&mut self, request: &mut ImportedRequest) {
        let at = request.location.clone();
        let mut renames = Renames::new();
        let mut kept = Vec::new();
        for mut var in std::mem::take(&mut request.variables) {
            if is_secret_variable(&var) && has_literal(&var.value) {
                let name = self.move_value(&var.name, &var.value, &at);
                renames.insert(var.name.clone(), name);
            } else {
                self.url(&mut var.value, &var_name(&var.name, "url"), &at);
                kept.push(var);
            }
        }
        request.variables = kept;
        scope::rename(request, &renames);
    }

    /// Literal secrets that scripts set: moved, so that every output
    /// redacts them (scripts themselves are kept as written).
    fn folder_scripts(&mut self, folder: &Folder, notes: &mut Vec<Note>) {
        self.scripts(&folder.scripts, notes);
        for item in &folder.items {
            match item {
                Item::Folder(child) => self.folder_scripts(child, notes),
                Item::Request(request) => self.scripts(&request.scripts, notes),
            }
        }
    }

    fn scripts(&mut self, scripts: &[Script], notes: &mut Vec<Note>) {
        for script in scripts {
            let mut moved = Vec::new();
            let all = text::script_values(&script.source, &|_| true);
            let secret = |f: &text::Found| is_script_secret(&f.name) || is_known_token(&f.value);
            for found in all.into_iter().filter(secret) {
                let (base, value) = script_secret(&found.name, &found.value);
                if has_literal(value) && !is_redaction_marker(value) {
                    self.move_value(&base, value, &script.location);
                    moved.push(value.to_owned());
                }
            }
            // `btoa('user:pw')` builds a Basic credential.
            for found in text::encoded_literals(&script.source) {
                if has_literal(&found.value) && !is_redaction_marker(&found.value) {
                    self.move_value("basic_credentials", &found.value, &script.location);
                    moved.push(found.value);
                }
            }
            for literal in text::string_literals(&script.source) {
                for token in known_tokens(literal) {
                    let value = &literal[token.range];
                    self.move_token(token.name, value, &script.location);
                    moved.push(value.to_owned());
                }
            }
            let kept = text::string_literals(&script.source)
                .any(|s| looks_secret(s) && !moved.iter().any(|m| s.contains(m.as_str())));
            if kept {
                let message = format!(
                    "a string in the {} script looks like a secret but was kept; review it",
                    script.event
                );
                notes.push(Note::new(NoteKind::Review, message, &script.location));
            }
        }
    }

    fn headers(&mut self, headers: &mut [Pair], at: &Location) {
        for header in headers {
            let lower = header.name.to_ascii_lowercase();
            match lower.as_str() {
                "authorization" | "proxy-authorization" => {
                    let base = format!("{}_token", var_name(&lower, "authorization"));
                    self.replace_credentials(&mut header.value, &base, at);
                }
                "cookie" => self.replace(&mut header.value, "cookie", at),
                _ if is_auth_scheme_value(&header.value) => {
                    let base = var_name(&header.name, "secret");
                    self.replace_credentials(&mut header.value, &base, at);
                }
                _ if is_secret_header_name(&header.name) || is_known_token(&header.value) => {
                    self.replace(&mut header.value, &var_name(&header.name, "secret"), at);
                }
                _ => {}
            }
        }
    }

    /// Query parameters and form fields with secret-like names.
    fn pairs(&mut self, pairs: &mut [Pair], at: &Location) {
        for pair in pairs.iter_mut().filter(|p| is_secret(&p.name, &p.value)) {
            self.replace(&mut pair.value, &var_name(&pair.name, "secret"), at);
        }
    }

    fn auth(&mut self, auth: Option<&mut ImportedAuth>, at: &Location) {
        match auth {
            Some(ImportedAuth::Basic { password, .. }) => self.replace(password, "password", at),
            Some(ImportedAuth::Bearer { token }) => self.replace(token, "token", at),
            Some(ImportedAuth::ApiKey { name, value, .. }) => {
                let base = var_name(name, "api_key");
                self.replace(value, &base, at);
            }
            Some(ImportedAuth::Oauth2(oauth)) => {
                if let Some(secret) = &mut oauth.client_secret {
                    self.replace(secret, "client_secret", at);
                }
            }
            Some(ImportedAuth::None) | None => {}
        }
    }

    /// The variables of a source environment: secret ones keep their name
    /// and move to that environment's secrets.
    fn environment(&mut self, env: &mut ImportedEnvironment) {
        let target = environment_name(&env.name);
        for var in &mut env.variables {
            let secret = is_secret_variable(var);
            if !secret {
                self.environment_url(var, &target, &env.location);
                continue;
            }
            if !has_literal(&var.value) {
                continue;
            }
            self.moved.push(MovedSecret {
                secret: Secret::new(&var.name, std::mem::take(&mut var.value)),
                environment: target.clone(),
                location: env.location.clone(),
                from_environment: true,
            });
            var.secret = true;
        }
    }

    /// The userinfo password of an environment variable holding a URL
    /// (`postgres://admin:pw@db/x`), moved to that environment's secrets
    /// as `<variable>_password`.
    fn environment_url(&mut self, var: &mut Variable, target: &str, at: &Location) {
        let Some(range) = text::userinfo_credentials(&var.value) else {
            return;
        };
        let value = var.value[range.clone()].to_owned();
        if !has_literal(&value) {
            return;
        }
        let name = format!("{}_password", var_name(&var.name, "url"));
        var.value.replace_range(range, &format!("{{{{{name}}}}}"));
        self.moved.push(MovedSecret {
            secret: Secret::new(&name, value),
            environment: target.to_owned(),
            location: at.clone(),
            from_environment: true,
        });
    }

    /// `Bearer abc` keeps its scheme: `Bearer {{name}}`.
    fn replace_credentials(&mut self, value: &mut String, base: &str, at: &Location) {
        let (scheme, credentials) = split_scheme(value);
        if has_literal(credentials) {
            let name = self.move_value(base, credentials, at);
            *value = format!("{scheme}{{{{{name}}}}}");
        }
    }

    fn replace(&mut self, value: &mut String, base: &str, at: &Location) {
        if let Some(placeholder) = self.placeholder(value, base, at) {
            *value = placeholder;
        }
    }

    /// The `{{name}}` placeholder for a literal `value`, moved as a secret.
    fn placeholder(&mut self, value: &str, base: &str, at: &Location) -> Option<String> {
        has_literal(value).then(|| format!("{{{{{}}}}}", self.move_value(base, value, at)))
    }

    /// The name of a token found by its shape: the name the same value was
    /// already moved under, else a new one after its kind.
    fn move_token(&mut self, kind: &str, value: &str, at: &Location) -> String {
        match self.names_by_value.get(value) {
            Some(name) => name.clone(),
            None => self.move_value(kind, value, at),
        }
    }

    /// Give `value` a secret name in the target environment and record it.
    fn move_value(&mut self, base: &str, value: &str, at: &Location) -> String {
        let by_value = (self.target.clone(), base.to_owned(), value.to_owned());
        if let Some(name) = self.by_value.get(&by_value) {
            return name.clone();
        }
        let name = self.name_for(base, value);
        self.by_value.insert(by_value, name.clone());
        let key = (self.target.clone(), name.clone());
        let fresh = !self.assigned.contains_key(&key);
        self.assigned.insert(key, value.to_owned());
        self.advance_free(base);
        self.names_by_value
            .entry(value.to_owned())
            .or_insert_with(|| name.clone());
        if fresh {
            self.moved.push(MovedSecret {
                secret: Secret::new(&name, value.to_owned()),
                environment: self.target.clone(),
                location: at.clone(),
                from_environment: false,
            });
        }
        name
    }

    /// `base`, else `base_2`, `base_3`...: the first name that is free or
    /// already holds `value`.
    fn name_for(&self, base: &str, value: &str) -> String {
        let key = (self.target.clone(), base.to_owned());
        let mut n = self.next_free.get(&key).copied().unwrap_or(1);
        loop {
            let candidate = candidate(base, n);
            match self.slot(&candidate, value) {
                Slot::Free | Slot::SameValue => return candidate,
                Slot::Taken => n += 1,
            }
        }
    }

    /// Move `next_free` for `base` past the names this import gave out.
    fn advance_free(&mut self, base: &str) {
        let key = (self.target.clone(), base.to_owned());
        let mut n = self.next_free.get(&key).copied().unwrap_or(1);
        while self
            .assigned
            .contains_key(&(self.target.clone(), candidate(base, n)))
        {
            n += 1;
        }
        self.next_free.insert(key, n);
    }

    fn slot(&self, name: &str, value: &str) -> Slot {
        let key = (self.target.clone(), name.to_owned());
        if let Some(assigned) = self.assigned.get(&key) {
            return if assigned == value {
                Slot::SameValue
            } else {
                Slot::Taken
            };
        }
        let env = self
            .existing
            .environments
            .iter()
            .find(|e| e.name == self.target);
        let Some(env) = env else {
            return Slot::Free;
        };
        if env.is_secret(name) {
            let stored = (self.existing.lookup)(&self.target, name);
            return match stored {
                Some(secret) if secret.expose() == value => Slot::SameValue,
                _ => Slot::Taken,
            };
        }
        if env.variables.get(name).is_some() {
            Slot::Taken
        } else {
            Slot::Free
        }
    }
}

/// Split an `Authorization` value into its scheme (with the space) and
/// credentials; a value without a scheme is all credentials.
fn split_scheme(value: &str) -> (&str, &str) {
    match value.split_once(' ') {
        Some((scheme, rest))
            if !scheme.is_empty() && scheme.chars().all(|c| c.is_ascii_alphanumeric()) =>
        {
            (&value[..=scheme.len()], rest.trim_start())
        }
        _ => ("", value),
    }
}

/// Whether `value` holds literal text beyond placeholders and whitespace.
pub fn has_literal(value: &str) -> bool {
    match Template::parse(value) {
        Ok(template) => template.segments().iter().any(|s| match s {
            Segment::Literal(text) => !text.trim().is_empty(),
            Segment::Var(_) | Segment::Dynamic(_) => false,
        }),
        Err(_) => !value.trim().is_empty(),
    }
}

/// A value an earlier import already redacted, such as `«redacted:token»`.
fn is_redaction_marker(value: &str) -> bool {
    value.starts_with("«redacted:") && value.ends_with('»')
}

/// Locates named values in a body's text.
type Finder = fn(&str, &dyn Fn(&str) -> bool) -> Vec<text::Found>;

/// The secret name and value a script sets: a credential header keeps its
/// scheme out of the value, like an `Authorization` header.
fn script_secret<'v>(name: &str, value: &'v str) -> (String, &'v str) {
    if is_credential_header(name) {
        let lower = name.to_ascii_lowercase();
        let base = format!("{}_token", var_name(&lower, "authorization"));
        return (base, split_scheme(value).1);
    }
    (var_name(name, "secret"), value)
}

/// `base`, then `base_2`, `base_3`...
fn candidate(base: &str, n: usize) -> String {
    if n == 1 {
        base.to_owned()
    } else {
        format!("{base}_{n}")
    }
}

/// `text` with each range replaced, in one pass; `edits` are in order and
/// do not overlap.
fn splice(text: &str, edits: Vec<(Range<usize>, String)>) -> String {
    let mut out = String::with_capacity(text.len());
    let mut at = 0;
    for (range, replacement) in edits {
        out.push_str(&text[at..range.start]);
        out.push_str(&replacement);
        at = range.end;
    }
    out.push_str(&text[at..]);
    out
}

/// Values that look like secrets (long, letters and digits, no spaces) but
/// were kept: listed for review, by name only.
fn review_notes(collection: &ImportedCollection) -> Vec<Note> {
    let mut notes = Vec::new();
    let mut check = |vars: &[Variable], at: &Location| {
        for var in vars.iter().filter(|v| !v.secret && looks_secret(&v.value)) {
            let message = format!(
                "variable '{}' looks like a secret but was kept as a plain value; review it",
                var.name
            );
            notes.push(Note::new(NoteKind::Review, message, at));
        }
    };
    let mut folders = vec![&collection.root];
    while let Some(folder) = folders.pop() {
        check(&folder.variables, &folder.location);
        folders.extend(folder.items.iter().filter_map(|item| match item {
            Item::Folder(child) => Some(child.as_ref()),
            Item::Request(_) => None,
        }));
    }
    for env in &collection.environments {
        check(&env.variables, &env.location);
    }
    notes
}

/// At least 24 characters, no whitespace, placeholder or URL scheme, and
/// both letters and digits.
fn looks_secret(value: &str) -> bool {
    value.chars().count() >= 24
        && !value.contains("{{")
        && !value.contains("://")
        && !value.chars().any(char::is_whitespace)
        && value.chars().any(|c| c.is_ascii_alphabetic())
        && value.chars().any(|c| c.is_ascii_digit())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ir::{ApiKeyIn, Grant, ImportedOAuth2, VisitStrings};
    use rustman_core::model::{VarValue, Variables};

    fn at() -> Location {
        Location::item("c.json", "Login")
    }

    fn collection_with(request: ImportedRequest) -> ImportedCollection {
        ImportedCollection {
            name: "Billing API".into(),
            root: Folder {
                items: vec![Item::Request(Box::new(request))],
                location: Location::item("c.json", ""),
                ..Folder::default()
            },
            ..ImportedCollection::default()
        }
    }

    fn first_request(c: &mut ImportedCollection) -> &mut ImportedRequest {
        c.root.requests_mut().remove(0)
    }

    #[test]
    fn secret_names_follow_the_spec_pattern() {
        for name in [
            "token",
            "API_KEY",
            "api-key",
            "apikey",
            "Password",
            "passwd",
            "client_secret",
        ] {
            assert!(is_secret_name(name), "{name}");
        }
        for name in ["user", "base_url", "id"] {
            assert!(!is_secret_name(name), "{name}");
        }
    }

    #[test]
    fn codes_signatures_and_one_time_passwords_are_secret_names() {
        for name in [
            "code",
            "auth_code",
            "sig",
            "X-Sig",
            "pass",
            "otp",
            "signature",
        ] {
            assert!(is_secret_name(name), "{name}");
        }
        for name in ["status_code", "zip-code", "bypass", "signal", "compass"] {
            assert!(!is_secret_name(name), "{name}");
        }
    }

    #[test]
    fn well_known_token_formats_are_secret_whatever_their_name() {
        for value in [
            "ghp_abcdefghijklmnopqrstuvwxyz012345",
            "token ghp_abcdefghijklmnopqrstuvwxyz012345",
            "sk_live_abcdefghijklmnop",
            "xoxb-1234567890-abcdef",
            "AKIAABCDEFGHIJKLMNOP",
            "eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxMjM0In0.sig",
        ] {
            assert!(is_known_token(value), "{value}");
        }
        for value in ["sk_test_x", "ghp_short", "https://x.test", "eyJhbGci"] {
            assert!(!is_known_token(value), "{value}");
        }
    }

    #[test]
    fn known_tokens_and_new_names_are_moved_from_every_place() {
        let token = "ghp_abcdefghijklmnopqrstuvwxyz012345";
        let mut c = collection_with(ImportedRequest {
            headers: vec![Pair::new("X-Custom", token)],
            query: vec![Pair::new("code", "c-1"), Pair::new("sig", "s-1")],
            body: Some(ImportedBody::Json {
                text: r#"{"user":{"credentials":{"pass":"p-1"}},"otp":"o-1","n":"sk_live_abcdefghijklmnop"}"#
                    .into(),
            }),
            scripts: vec![Script {
                event: "prerequest".into(),
                source: "pm.request.headers.add({key:'X-Sig', value:'s-2'})".into(),
                location: at(),
            }],
            location: at(),
            ..ImportedRequest::default()
        });
        c.root.variables = vec![Variable {
            name: "stripe_sk".into(),
            value: "sk_live_abcdefghijklmnop".into(),
            secret: false,
        }];
        let moved = extract(&mut c, "local", &Existing::none());
        let values: Vec<&str> = moved.iter().map(|m| m.secret.expose().as_str()).collect();
        for value in [
            token,
            "c-1",
            "s-1",
            "p-1",
            "o-1",
            "sk_live_abcdefghijklmnop",
            "s-2",
        ] {
            assert!(values.contains(&value), "{value} not in {values:?}");
        }
        assert!(c.root.variables.is_empty());
        let r = first_request(&mut c);
        assert_eq!(r.headers[0].value, "{{x_custom}}");
        let Some(ImportedBody::Json { text }) = &r.body else {
            panic!()
        };
        assert!(!text.contains("p-1") && !text.contains("o-1"), "{text}");
    }

    #[test]
    fn a_literal_bearer_header_becomes_a_placeholder() {
        let mut c = collection_with(ImportedRequest {
            headers: vec![
                Pair::new("Authorization", "Bearer eyJhbGciOiJIUzI1NiJ9.x.y"),
                Pair::new("Accept", "application/json"),
            ],
            location: at(),
            ..ImportedRequest::default()
        });
        let moved = extract(&mut c, "local", &Existing::none());
        let request = first_request(&mut c);
        assert_eq!(request.headers[0].value, "Bearer {{authorization_token}}");
        assert_eq!(request.headers[1].value, "application/json");
        assert_eq!(moved.len(), 1);
        assert_eq!(moved[0].secret.name(), "authorization_token");
        assert_eq!(moved[0].secret.expose(), "eyJhbGciOiJIUzI1NiJ9.x.y");
        assert_eq!(moved[0].environment, "local");
        assert_eq!(moved[0].location, at());
    }

    #[test]
    fn placeholders_are_not_secrets() {
        let mut c = collection_with(ImportedRequest {
            headers: vec![Pair::new("Authorization", "Bearer {{token}}")],
            auth: Some(ImportedAuth::Basic {
                username: "me".into(),
                password: "{{password}}".into(),
            }),
            ..ImportedRequest::default()
        });
        assert!(extract(&mut c, "local", &Existing::none()).is_empty());
        assert_eq!(first_request(&mut c).headers[0].value, "Bearer {{token}}");
    }

    #[test]
    fn auth_fields_cookies_and_key_parameters_are_moved() {
        let mut c = collection_with(ImportedRequest {
            headers: vec![
                Pair::new("Cookie", "session=abc123"),
                Pair::new("X-API-Key", "k-123456"),
            ],
            query: vec![Pair::new("api_key", "q-999"), Pair::new("page", "2")],
            auth: Some(ImportedAuth::Oauth2(ImportedOAuth2 {
                grant: Grant::ClientCredentials,
                token_url: "https://auth/token".into(),
                auth_url: None,
                client_id: "cid".into(),
                client_secret: Some("cs-1".into()),
                scopes: vec![],
            })),
            body: Some(ImportedBody::Form {
                fields: vec![Pair::new("username", "me"), Pair::new("password", "pw-1")],
            }),
            ..ImportedRequest::default()
        });
        let moved = extract(&mut c, "local", &Existing::none());
        let names: Vec<&str> = moved.iter().map(|m| m.secret.name()).collect();
        assert_eq!(
            names,
            [
                "cookie",
                "x_api_key",
                "api_key",
                "client_secret",
                "password"
            ]
        );
        let r = first_request(&mut c);
        assert_eq!(r.headers[0].value, "{{cookie}}");
        assert_eq!(r.query[1].value, "2");
        let Some(ImportedBody::Form { fields }) = &r.body else {
            panic!()
        };
        assert_eq!(fields[0].value, "me");
        assert_eq!(fields[1].value, "{{password}}");
    }

    #[test]
    fn equal_values_share_a_name_and_different_ones_get_suffixes() {
        let bearer = |token: &str| ImportedAuth::Bearer {
            token: token.into(),
        };
        let mut c = ImportedCollection::default();
        for token in ["t-one", "t-two", "t-one"] {
            c.root.items.push(Item::Request(Box::new(ImportedRequest {
                auth: Some(bearer(token)),
                ..ImportedRequest::default()
            })));
        }
        let moved = extract(&mut c, "local", &Existing::none());
        let names: Vec<&str> = moved.iter().map(|m| m.secret.name()).collect();
        assert_eq!(names, ["token", "token_2"]);
        let tokens: Vec<_> = c
            .root
            .requests_mut()
            .into_iter()
            .map(|r| r.auth.clone())
            .collect();
        assert_eq!(tokens[2], Some(bearer("{{token}}")));
        assert_eq!(tokens[1], Some(bearer("{{token_2}}")));
    }

    #[test]
    fn existing_names_are_reused_only_for_the_same_value() {
        let envs = [Environment {
            name: "local".into(),
            variables: Variables::new(vec![("password".into(), VarValue::String("x".into()))]),
            secrets: vec!["token".into(), "api_key".into()],
            production: false,
        }];
        let lookup = |_: &str, name: &str| {
            (name == "token").then(|| Secret::new("token", "same".to_owned()))
        };
        let existing = Existing {
            environments: &envs,
            lookup: &lookup,
        };
        let mut c = ImportedCollection::default();
        let auths = [
            ImportedAuth::Bearer {
                token: "same".into(),
            },
            ImportedAuth::ApiKey {
                name: "api_key".into(),
                value: "new".into(),
                location: ApiKeyIn::Query,
            },
            ImportedAuth::Basic {
                username: "u".into(),
                password: "p".into(),
            },
        ];
        for auth in auths {
            c.root.items.push(Item::Request(Box::new(ImportedRequest {
                auth: Some(auth),
                ..ImportedRequest::default()
            })));
        }
        let moved = extract(&mut c, "local", &existing);
        let names: Vec<&str> = moved.iter().map(|m| m.secret.name()).collect();
        assert_eq!(names, ["token", "api_key_2", "password_2"]);
    }

    #[test]
    fn secret_collection_variables_move_and_references_follow_renames() {
        let envs = [Environment {
            name: "local".into(),
            variables: Variables::new(vec![("api_key".into(), VarValue::String("x".into()))]),
            ..Environment::default()
        }];
        let existing = Existing {
            environments: &envs,
            lookup: &|_, _| None,
        };
        let mut c = collection_with(ImportedRequest {
            headers: vec![Pair::new("X-Key", "{{api_key}}")],
            ..ImportedRequest::default()
        });
        c.root.variables = vec![
            Variable {
                name: "api_key".into(),
                value: "abc".into(),
                secret: false,
            },
            Variable {
                name: "base_url".into(),
                value: "https://x".into(),
                secret: false,
            },
        ];
        let moved = extract(&mut c, "local", &existing);
        assert_eq!(moved[0].secret.name(), "api_key_2");
        assert_eq!(c.root.variables.len(), 1);
        assert_eq!(first_request(&mut c).headers[0].value, "{{api_key_2}}");
    }

    #[test]
    fn environment_secrets_keep_their_names_and_lose_their_values() {
        let mut c = ImportedCollection {
            environments: vec![ImportedEnvironment {
                name: "Staging".into(),
                variables: vec![
                    Variable {
                        name: "api_key".into(),
                        value: "s3cr3t".into(),
                        secret: true,
                    },
                    Variable {
                        name: "host".into(),
                        value: "a1b2c3d4e5f6a1b2c3d4e5f6a1".into(),
                        secret: false,
                    },
                ],
                location: Location::item("staging.json", ""),
            }],
            ..ImportedCollection::default()
        };
        let moved = extract(&mut c, "unused", &Existing::none());
        assert_eq!(moved.len(), 1);
        assert_eq!(moved[0].environment, "staging");
        assert!(moved[0].from_environment);
        let api_key = &c.environments[0].variables[0];
        assert!(api_key.secret && api_key.value.is_empty());
        // The long value that was kept is listed for review, without its value.
        assert_eq!(c.notes.len(), 1);
        assert_eq!(c.notes[0].kind, NoteKind::Review);
        assert!(!c.notes[0].message.contains("a1b2c3"));
    }

    #[test]
    fn request_variables_move_and_the_url_follows_a_rename() {
        let mut c = collection_with(ImportedRequest {
            url: "{{base}}/statements/{{token}}".into(),
            auth: Some(ImportedAuth::Bearer {
                token: "t-collection".into(),
            }),
            variables: vec![
                Variable {
                    name: "token".into(),
                    value: "LEAK4path".into(),
                    secret: false,
                },
                Variable {
                    name: "org".into(),
                    value: "acme".into(),
                    secret: false,
                },
            ],
            ..ImportedRequest::default()
        });
        let moved = extract(&mut c, "local", &Existing::none());
        let values: Vec<_> = moved.iter().map(|m| m.secret.expose().as_str()).collect();
        assert_eq!(values, ["t-collection", "LEAK4path"]);
        let r = first_request(&mut c);
        assert_eq!(r.url, "{{base}}/statements/{{token_2}}");
        assert_eq!(r.variables.len(), 1);
        assert_eq!(r.variables[0].name, "org");
        assert!(!request_has_literal(r));
    }

    #[test]
    fn url_userinfo_credentials_are_moved() {
        let mut c = collection_with(ImportedRequest {
            url: "https://admin:LEAK1pass@api.example.com/x".into(),
            ..ImportedRequest::default()
        });
        let probe = first_request(&mut c).clone();
        assert!(request_has_literal(&probe), "the writer guard sees it");
        let moved = extract(&mut c, "local", &Existing::none());
        assert_eq!(moved[0].secret.name(), "url_password");
        assert_eq!(moved[0].secret.expose(), "LEAK1pass");
        let r = first_request(&mut c);
        assert_eq!(r.url, "https://admin:{{url_password}}@api.example.com/x");
        let mut c = collection_with(ImportedRequest {
            url: "https://ghp_LEAK2@github.com/x".into(),
            ..ImportedRequest::default()
        });
        extract(&mut c, "local", &Existing::none());
        assert_eq!(
            first_request(&mut c).url,
            "https://{{url_token}}@github.com/x"
        );
    }

    #[test]
    fn secret_keys_inside_body_text_are_moved() {
        let bodies = [
            ImportedBody::Json {
                text: r#"{"username": "u", "password": "LEAK-json"}"#.into(),
            },
            ImportedBody::Text {
                text: "user=u&password=LEAK-form".into(),
            },
            ImportedBody::Graphql {
                query: "query { me { id } }".into(),
                variables: Some(r#"{"token": "LEAK-gql"}"#.into()),
            },
        ];
        let expected = [
            r#"{"username": "u", "password": "{{password}}"}"#,
            "user=u&password={{password}}",
            r#"{"token": "{{token}}"}"#,
        ];
        for (body, expected) in bodies.into_iter().zip(expected) {
            let mut c = collection_with(ImportedRequest {
                body: Some(body),
                ..ImportedRequest::default()
            });
            let moved = extract(&mut c, "local", &Existing::none());
            assert_eq!(moved.len(), 1);
            assert!(moved[0].secret.expose().starts_with("LEAK-"));
            let mut text = String::new();
            first_request(&mut c).body.visit_strings(&mut |s| {
                if s.starts_with('{') || s.contains('=') {
                    text = s.clone();
                }
            });
            assert_eq!(text, expected);
        }
    }

    #[test]
    fn literals_set_by_scripts_are_moved_and_others_flagged() {
        let script = |source: &str| Script {
            event: "prerequest".into(),
            source: source.into(),
            location: at(),
        };
        let mut c = collection_with(ImportedRequest {
            scripts: vec![script(
                "pm.environment.set('apiSecret','LEAK13script');\n\
                 req.setHeader(\"Authorization\", \"Bearer LEAK14bearer\");\n\
                 pm.environment.set('jwt', '«redacted:jwt»');",
            )],
            ..ImportedRequest::default()
        });
        c.root.scripts = vec![script("const id = 'a1b2c3d4e5f6a1b2c3d4e5f6a1b2';")];
        let moved = extract(&mut c, "local", &Existing::none());
        let pairs: Vec<_> = moved
            .iter()
            .map(|m| (m.secret.name(), m.secret.expose().as_str()))
            .collect();
        assert_eq!(
            pairs,
            [
                ("apisecret", "LEAK13script"),
                ("authorization_token", "LEAK14bearer")
            ]
        );
        // The script itself is kept; outputs redact the moved values.
        assert!(
            first_request(&mut c).scripts[0]
                .source
                .contains("LEAK13script")
        );
        let review: Vec<_> = c
            .notes
            .iter()
            .filter(|n| n.kind == NoteKind::Review)
            .collect();
        assert_eq!(review.len(), 1);
        assert!(!review[0].message.contains("a1b2"));
    }

    #[test]
    fn session_cookie_and_key_names_are_secrets_too() {
        for name in [
            "X-Session",
            "X-Access-Key",
            "Set-Cookie",
            "auth",
            "key",
            "bearer",
            "credentials",
            "session",
        ] {
            assert!(is_secret_name(name), "{name}");
        }
        for name in ["Idempotency-Key", "keyword", "author", "monkey"] {
            assert!(!is_secret_name(name), "{name}");
        }
    }

    #[test]
    fn schemes_and_literals() {
        assert_eq!(split_scheme("Bearer abc"), ("Bearer ", "abc"));
        assert_eq!(split_scheme("abc"), ("", "abc"));
        assert_eq!(split_scheme("a=b; c d"), ("", "a=b; c d"));
        assert!(has_literal("abc{{x}}"));
        assert!(!has_literal(" {{x}} {{$uuid}}"));
        assert!(!has_literal(""));
    }

    #[test]
    fn auth_scheme_values_and_credential_header_suffixes_are_secret() {
        for value in [
            "Bearer abc",
            "basic dTpw",
            "  DIGEST username=x",
            "bearer {{t}}x",
        ] {
            assert!(is_auth_scheme_value(value), "{value}");
        }
        for value in ["Bearerabc", "Basically", "plain", ""] {
            assert!(!is_auth_scheme_value(value), "{value}");
        }
        for name in [
            "X-Custom-Auth",
            "x-session-token",
            "X-Partner-Key",
            "X-App-Secret",
            "X-Upload-Signature",
        ] {
            assert!(is_secret_header_name(name), "{name}");
        }
        for name in ["X-Author", "X-Keyboard", "Accept", "X-Auth-Mode"] {
            assert!(!is_secret_header_name(name), "{name}");
        }
        let mut c = collection_with(ImportedRequest {
            headers: vec![
                Pair::new("X-Custom-Auth", "ca-1"),
                Pair::new("X-Partner-Key", "pk-1"),
                Pair::new("X-Forwarded-Credential", "Bearer fw-1"),
                Pair::new("X-Trace", "t-1"),
            ],
            location: at(),
            ..ImportedRequest::default()
        });
        c.root.variables = vec![Variable {
            name: "upstream".into(),
            value: "Basic dXNlcjpwdw==".into(),
            secret: false,
        }];
        c.environments = vec![ImportedEnvironment {
            name: "Prod".into(),
            variables: vec![Variable {
                name: "header_value".into(),
                value: "Digest username=\"u\", response=\"r\"".into(),
                secret: false,
            }],
            location: at(),
        }];
        let moved = extract(&mut c, "local", &Existing::none());
        let values: Vec<&str> = moved.iter().map(|m| m.secret.expose().as_str()).collect();
        for value in ["ca-1", "pk-1", "fw-1", "Basic dXNlcjpwdw=="] {
            assert!(values.contains(&value), "{value} not in {values:?}");
        }
        assert!(
            values.iter().any(|v| v.starts_with("Digest ")),
            "{values:?}"
        );
        assert!(c.root.variables.is_empty());
        assert!(c.environments[0].variables[0].secret);
        let r = first_request(&mut c);
        assert_eq!(r.headers[0].value, "{{x_custom_auth}}");
        assert_eq!(r.headers[1].value, "{{x_partner_key}}");
        assert_eq!(r.headers[2].value, "Bearer {{x_forwarded_credential}}");
        assert_eq!(r.headers[3].value, "t-1");
    }
}
