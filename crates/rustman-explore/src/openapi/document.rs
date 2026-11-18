//! Parsing a document (JSON or YAML, told apart by content) and detecting its
//! OpenAPI version.

use std::path::Path;

use serde_json::Value;

use super::error::ImportError;

/// The OpenAPI line a document declares.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SpecVersion {
    V3_0,
    V3_1,
}

/// How a document was encoded.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Format {
    Json,
    Yaml,
}

/// The format of `text`: JSON when its first character (after a byte order
/// mark and whitespace) opens an object or array, otherwise YAML.
pub fn detect_format(text: &str) -> Format {
    let start = text.trim_start_matches('\u{feff}').trim_start();
    if start.starts_with('{') || start.starts_with('[') {
        Format::Json
    } else {
        Format::Yaml
    }
}

/// Parse `text` (named `source_name` in errors) as JSON or YAML.
pub fn parse(text: &str, source_name: &str) -> Result<(Format, Value), ImportError> {
    let text = text.trim_start_matches('\u{feff}');
    let format = detect_format(text);
    let parsed = match format {
        Format::Json => serde_json::from_str(text).map_err(|e| format!("invalid JSON: {e}")),
        Format::Yaml => rustman_core::format::from_yaml::<Value>(Path::new(source_name), text)
            .map_err(|e| match e.position {
                Some(at) => format!("invalid YAML at line {}: {}", at.line, e.message),
                None => format!("invalid YAML: {}", e.message),
            }),
    };
    let value = parsed.map_err(|message| ImportError::Parse {
        source_name: source_name.to_owned(),
        message,
    })?;
    Ok((format, value))
}

/// The version `doc` declares. Swagger 2.0 and other versions are rejected.
pub fn detect_version(doc: &Value, source_name: &str) -> Result<SpecVersion, ImportError> {
    if doc.get("swagger").is_some() {
        return Err(ImportError::Swagger2);
    }
    let declared = match doc.get("openapi") {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Number(n)) => n.to_string(),
        _ => return Err(ImportError::NotOpenApi(source_name.to_owned())),
    };
    match declared.split('.').take(2).collect::<Vec<_>>().as_slice() {
        ["3", "0"] => Ok(SpecVersion::V3_0),
        ["3", "1"] => Ok(SpecVersion::V3_1),
        _ => Err(ImportError::UnsupportedVersion(declared)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn version(text: &str) -> Result<SpecVersion, ImportError> {
        let (_, doc) = parse(text, "doc").unwrap();
        detect_version(&doc, "doc")
    }

    #[test]
    fn format_comes_from_content() {
        assert_eq!(detect_format("\u{feff}  {\"openapi\": 1}"), Format::Json);
        assert_eq!(detect_format("openapi: 3.1.0\n"), Format::Yaml);
        assert_eq!(detect_format("# comment\n{}"), Format::Yaml);
    }

    #[test]
    fn versions() {
        assert_eq!(version("openapi: 3.0.3").unwrap(), SpecVersion::V3_0);
        assert_eq!(
            version("{\"openapi\": \"3.1.0\"}").unwrap(),
            SpecVersion::V3_1
        );
        assert_eq!(version("openapi: 3.1").unwrap(), SpecVersion::V3_1);
        assert!(matches!(
            version("swagger: '2.0'"),
            Err(ImportError::Swagger2)
        ));
        assert!(matches!(
            version("openapi: 4.0.0"),
            Err(ImportError::UnsupportedVersion(v)) if v == "4.0.0"
        ));
        assert!(matches!(
            version("title: x"),
            Err(ImportError::NotOpenApi(_))
        ));
    }

    #[test]
    fn swagger_message_is_the_spec_text() {
        assert_eq!(
            ImportError::Swagger2.to_string(),
            "Swagger 2.0 is not supported; convert it to OpenAPI 3 first (e.g. with swagger2openapi)"
        );
    }

    #[test]
    fn parse_errors_name_the_source() {
        let err = parse("{\"a\": ", "api.json").unwrap_err();
        assert!(
            err.to_string().starts_with("api.json: invalid JSON"),
            "{err}"
        );
        let err = parse("a: [", "api.yaml").unwrap_err();
        assert!(
            err.to_string().starts_with("api.yaml: invalid YAML"),
            "{err}"
        );
    }
}
