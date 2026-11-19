//! `{{variable}}` interpolation in expected values (assertions spec
//! "Variable interpolation in assertions").

use serde_json::Value;

use crate::redact::Redactor;
use crate::vars::{Resolver, Template};

/// An interpolated expected JSON value.
pub(crate) struct Expected {
    pub value: Value,
    /// The original was a string with a `{{placeholder}}`: compare it with a
    /// number or boolean through that value's string form.
    pub templated: bool,
}

impl Expected {
    /// `value` used as written.
    pub fn literal(value: &Value) -> Self {
        Self {
            value: value.clone(),
            templated: false,
        }
    }
}

/// Interpolates expected values and remembers the secrets used, so they are
/// redacted from every message.
pub(crate) struct Interp<'r, 'a> {
    resolver: &'r Resolver<'a>,
    redactor: Redactor,
}

impl<'r, 'a> Interp<'r, 'a> {
    pub fn new(resolver: &'r Resolver<'a>, redactor: &Redactor) -> Self {
        Self {
            resolver,
            redactor: redactor.clone(),
        }
    }

    pub fn into_redactor(self) -> Redactor {
        self.redactor
    }

    /// `input` with its placeholders resolved. The error lists every
    /// unresolved name.
    pub fn text(&mut self, input: &str) -> Result<String, String> {
        if !input.contains("{{") {
            return Ok(input.to_owned());
        }
        let mut interpolator = self.resolver.interpolator();
        let resolved = interpolator.text(input);
        let (resolved, secrets) = interpolator.finish(resolved).map_err(|e| e.to_string())?;
        for secret in &secrets {
            self.redactor.add(secret);
        }
        Ok(resolved.text)
    }

    /// `value` with every string in it interpolated.
    pub fn value(&mut self, value: &Value) -> Result<Expected, String> {
        let templated = matches!(value, Value::String(s) if self.text_is_templated(s));
        Ok(Expected {
            value: self.walk(value)?,
            templated,
        })
    }

    fn text_is_templated(&self, s: &str) -> bool {
        Template::parse(s).is_ok_and(|t| !t.is_literal())
    }

    fn walk(&mut self, value: &Value) -> Result<Value, String> {
        Ok(match value {
            Value::String(s) => Value::String(self.text(s)?),
            Value::Array(items) => Value::Array(
                items
                    .iter()
                    .map(|v| self.walk(v))
                    .collect::<Result<_, _>>()?,
            ),
            Value::Object(map) => Value::Object(
                map.iter()
                    .map(|(k, v)| Ok((k.clone(), self.walk(v)?)))
                    .collect::<Result<_, String>>()?,
            ),
            other => other.clone(),
        })
    }
}
