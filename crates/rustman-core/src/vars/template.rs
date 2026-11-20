//! Parsing `{{name}}` templates into segments (design D8).

use std::fmt;

use super::dynamic::Dynamic;
use crate::secrets::provider::{PREFIX as SECRET_PREFIX, SecretRef};

/// One piece of a parsed template.
#[derive(Clone, Debug, PartialEq)]
pub enum Segment {
    Literal(String),
    /// `{{name}}`: a variable looked up through the precedence chain.
    Var(String),
    /// `{{$fn args}}`: a value generated for every occurrence.
    Dynamic(Dynamic),
    /// `{{secret:<provider>/<path>[#field]}}`: a provider secret, resolved
    /// only when a request is sent.
    Secret(SecretRef),
}

/// A parsed template.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Template {
    segments: Vec<Segment>,
}

/// A template that cannot be parsed.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct TemplateError(pub String);

impl Template {
    /// Parse `input`. `\{{` stands for a literal `{{`; every other `{{` opens
    /// a placeholder that must be closed by `}}`.
    pub fn parse(input: &str) -> Result<Self, TemplateError> {
        let mut builder = Builder::default();
        let mut rest = input;
        while let Some(start) = rest.find("{{") {
            let (before, after) = rest.split_at(start);
            let after = &after[2..];
            if let Some(text) = before.strip_suffix('\\') {
                builder.literal(text);
                builder.literal("{{");
                rest = after;
                continue;
            }
            builder.literal(before);
            let end = after.find("}}").ok_or_else(|| {
                TemplateError(format!(
                    "unclosed '{{{{' in '{input}' (write '\\{{{{' for a literal '{{{{')"
                ))
            })?;
            builder.push(placeholder(&after[..end])?);
            rest = &after[end + 2..];
        }
        builder.literal(rest);
        Ok(builder.finish())
    }

    pub fn segments(&self) -> &[Segment] {
        &self.segments
    }

    /// True when the template has no placeholder.
    pub fn is_literal(&self) -> bool {
        self.segments
            .iter()
            .all(|s| matches!(s, Segment::Literal(_)))
    }
}

#[derive(Default)]
struct Builder {
    segments: Vec<Segment>,
    literal: String,
}

impl Builder {
    fn literal(&mut self, text: &str) {
        self.literal.push_str(text);
    }

    fn push(&mut self, segment: Segment) {
        self.flush();
        self.segments.push(segment);
    }

    fn flush(&mut self) {
        if !self.literal.is_empty() {
            let text = std::mem::take(&mut self.literal);
            self.segments.push(Segment::Literal(text));
        }
    }

    fn finish(mut self) -> Template {
        self.flush();
        Template {
            segments: self.segments,
        }
    }
}

/// The inside of `{{ ... }}`, surrounding whitespace allowed.
fn placeholder(inner: &str) -> Result<Segment, TemplateError> {
    let trimmed = inner.trim();
    if let Some(reference) = trimmed.strip_prefix(SECRET_PREFIX) {
        return SecretRef::parse(reference)
            .map(Segment::Secret)
            .map_err(TemplateError);
    }
    if trimmed.starts_with('$') {
        return Dynamic::parse(trimmed)
            .map(Segment::Dynamic)
            .map_err(TemplateError);
    }
    if is_variable_name(trimmed) {
        Ok(Segment::Var(trimmed.to_owned()))
    } else {
        Err(TemplateError(format!(
            "invalid variable name '{{{{{inner}}}}}'"
        )))
    }
}

/// Whether `{{name}}` can reference a variable called `name`: non-empty, no
/// whitespace and no braces. A leading `$` names a dynamic variable instead.
pub fn is_variable_name(name: &str) -> bool {
    !name.is_empty()
        && !name
            .chars()
            .any(|c| c.is_whitespace() || c == '{' || c == '}')
}

impl fmt::Display for Template {
    /// Writes the template back in source form.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for segment in &self.segments {
            match segment {
                Segment::Literal(text) => f.write_str(&text.replace("{{", "\\{{"))?,
                Segment::Var(name) => write!(f, "{{{{{name}}}}}")?,
                Segment::Dynamic(d) => write!(f, "{{{{{d}}}}}")?,
                Segment::Secret(r) => write!(f, "{{{{{SECRET_PREFIX}{r}}}}}")?,
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lit(s: &str) -> Segment {
        Segment::Literal(s.to_owned())
    }

    fn var(s: &str) -> Segment {
        Segment::Var(s.to_owned())
    }

    fn parse(s: &str) -> Vec<Segment> {
        Template::parse(s).unwrap().segments().to_vec()
    }

    #[test]
    fn literals_and_variables() {
        assert_eq!(
            parse("{{base_url}}/users/{{ id }}?x=1"),
            [var("base_url"), lit("/users/"), var("id"), lit("?x=1")]
        );
        assert_eq!(parse("plain"), [lit("plain")]);
        assert!(parse("").is_empty());
        assert!(Template::parse("a}}b{").unwrap().is_literal());
    }

    #[test]
    fn names_are_case_sensitive_and_may_contain_dots() {
        assert_eq!(
            parse("{{Token}}{{user.id}}"),
            [var("Token"), var("user.id")]
        );
    }

    #[test]
    fn escaped_braces_are_literal() {
        assert_eq!(parse("\\{{not_a_var}}"), [lit("{{not_a_var}}")]);
        assert_eq!(parse("a\\{{b}} {{c}}"), [lit("a{{b}} "), var("c")]);
        assert!(Template::parse("\\{{x}}").unwrap().is_literal());
    }

    #[test]
    fn dynamic_placeholders() {
        assert_eq!(
            parse("{{$randomInt 5 7}}"),
            [Segment::Dynamic(Dynamic::RandomInt { min: 5, max: 7 })]
        );
        assert_eq!(parse("{{ $uuid }}"), [Segment::Dynamic(Dynamic::Uuid)]);
    }

    #[test]
    fn secret_references_are_placeholders() {
        let header = parse("Bearer {{secret:vault/kv/app/stripe#api_key}}");
        assert_eq!(header[0], lit("Bearer "));
        let Segment::Secret(reference) = &header[1] else {
            panic!("{header:?}");
        };
        assert_eq!(reference.to_string(), "vault/kv/app/stripe#api_key");
        assert!(!Template::parse("{{ secret:ci/KEY }}").unwrap().is_literal());
        let spaced = parse("{{ secret:op/Eng Team/Stripe Key/token }}");
        let Segment::Secret(reference) = &spaced[0] else {
            panic!("{spaced:?}");
        };
        assert_eq!(reference.path, "Eng Team/Stripe Key/token");
        assert_eq!(
            Template::parse("{{base_url}}/{{secret:vault}}")
                .unwrap_err()
                .to_string(),
            "invalid secret reference 'secret:vault' (expected secret:<provider>/<path>)"
        );
    }

    #[test]
    fn unclosed_placeholder_is_an_error() {
        let err = Template::parse("{{base_url/users").unwrap_err();
        assert_eq!(
            err.to_string(),
            "unclosed '{{' in '{{base_url/users' (write '\\{{' for a literal '{{')"
        );
    }

    #[test]
    fn invalid_names_are_errors() {
        for bad in ["{{}}", "{{ }}", "{{a b}}", "{{{a}}}"] {
            assert!(Template::parse(bad).is_err(), "{bad}");
        }
        assert_eq!(
            Template::parse("{{a b}}").unwrap_err().to_string(),
            "invalid variable name '{{a b}}'"
        );
    }

    #[test]
    fn display_round_trips() {
        for text in [
            "{{a}}/x\\{{y}}",
            "{{$randomInt 5 7}}-{{$env HOME}}",
            "Bearer {{secret:vault/kv/app#pw}}",
            "plain",
        ] {
            let template = Template::parse(text).unwrap();
            assert_eq!(template.to_string(), text);
            assert_eq!(Template::parse(&template.to_string()).unwrap(), template);
        }
    }
}
