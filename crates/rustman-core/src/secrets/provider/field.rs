//! `#field` selection from a structured (JSON or YAML) secret.

use std::path::Path;

use serde_json::Value;

use super::error::ProviderError;
use crate::format::from_yaml;

/// The value of the dotted key path `field` in `raw`, read as JSON, else
/// as YAML. Strings are returned as they are, numbers and booleans as
/// text, objects and arrays as JSON. `raw` is never part of an error.
pub fn select(raw: &str, field: &str) -> Result<String, ProviderError> {
    let missing = || ProviderError::FieldNotFound(field.to_owned());
    let document: Value = serde_json::from_str(raw)
        .ok()
        .or_else(|| from_yaml(Path::new("secret"), raw).ok())
        .ok_or_else(missing)?;
    let found = lookup(&document, field).ok_or_else(missing)?;
    Ok(text(found))
}

/// `field` as a whole key first (keys may contain dots), else as a path.
fn lookup<'a>(document: &'a Value, field: &str) -> Option<&'a Value> {
    if let Some(value) = document.get(field) {
        return Some(value);
    }
    field.split('.').try_fold(document, |node, key| match node {
        Value::Array(items) => key.parse::<usize>().ok().and_then(|i| items.get(i)),
        _ => node.get(key),
    })
}

fn text(value: &Value) -> String {
    match value {
        Value::String(s) => s.clone(),
        Value::Null => String::new(),
        other => other.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fields_come_from_json_or_yaml() {
        let json = r#"{"password":"p","port":5432,"db":{"user":"u"},"a.b":"dotted"}"#;
        assert_eq!(select(json, "password").unwrap(), "p");
        assert_eq!(select(json, "port").unwrap(), "5432");
        assert_eq!(select(json, "db.user").unwrap(), "u");
        assert_eq!(select(json, "a.b").unwrap(), "dotted");
        assert_eq!(select(json, "db").unwrap(), r#"{"user":"u"}"#);
        let yaml = "db:\n  password: y\nlist: [a, b]\n";
        assert_eq!(select(yaml, "db.password").unwrap(), "y");
        assert_eq!(select(yaml, "list.1").unwrap(), "b");
    }

    #[test]
    fn missing_fields_never_show_the_secret() {
        let err = select(r#"{"password":"s3cr3t"}"#, "user").unwrap_err();
        assert_eq!(err, ProviderError::FieldNotFound("user".into()));
        assert!(!err.to_string().contains("s3cr3t"));
        let err = select("s3cr3t: [", "user").unwrap_err();
        assert!(!err.to_string().contains("s3cr3t"));
    }
}
