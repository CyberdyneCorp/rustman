//! Source formats and content-based detection (`auto`).

use std::path::Path;

use serde_json::Value;

use crate::error::ImportError;
use crate::ir::ImportedCollection;
use crate::{bruno, har, http, insomnia, postman};

/// A source format this build can import.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Format {
    /// A Postman Collection v2.0 or v2.1 export.
    Postman,
    /// A Postman environment export.
    PostmanEnv,
    /// An Insomnia v4 JSON or v5 YAML export.
    Insomnia,
    /// A Bruno collection folder, or a single `.bru` file.
    Bruno,
    /// A HAR 1.2 capture.
    Har,
    /// `.http` / `.rest` files.
    Http,
}

impl Format {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Postman => "postman",
            Self::PostmanEnv => "postman-env",
            Self::Insomnia => "insomnia",
            Self::Bruno => "bruno",
            Self::Har => "har",
            Self::Http => "http",
        }
    }

    /// Parse `text`, read from `file` (as shown in messages), in this format.
    /// A HAR is read with the default filter; files next to an `.http` file
    /// are not read (see [`crate::load`] for both).
    pub fn parse(self, text: &str, file: &str) -> Result<ImportedCollection, ImportError> {
        match self {
            Self::Postman => postman::parse_collection(text, file),
            Self::PostmanEnv => postman::parse_environment(text, file),
            Self::Insomnia => insomnia::parse(text, file),
            Self::Bruno => bruno::parse_file(text, file),
            Self::Har => har::parse(text, file, &har::HarFilter::default()),
            Self::Http => http::parse_file(text, file),
        }
    }
}

/// The format of `text`, judged from its content (never the file name).
pub fn detect(text: &str) -> Option<Format> {
    match serde_json::from_str::<Value>(text) {
        Ok(doc) => detect_json(&doc),
        Err(_) if insomnia::is_v5(text) => Some(Format::Insomnia),
        Err(_) if bruno::is_bru(text) => Some(Format::Bruno),
        Err(_) if http::is_http(text) => Some(Format::Http),
        Err(_) => None,
    }
}

fn detect_json(doc: &Value) -> Option<Format> {
    if postman::is_collection(doc) {
        Some(Format::Postman)
    } else if postman::is_environment(doc) {
        Some(Format::PostmanEnv)
    } else if insomnia::is_v4(doc) {
        Some(Format::Insomnia)
    } else if har::is_har(doc) {
        Some(Format::Har)
    } else {
        None
    }
}

/// The format of the folder `dir`: a Bruno collection (`bruno.json`) or a
/// folder of `.http` / `.rest` files.
pub fn detect_dir(dir: &Path) -> Option<Format> {
    if dir.join(bruno::MARKER).is_file() {
        Some(Format::Bruno)
    } else if http::has_http_files(dir) {
        Some(Format::Http)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn postman_collections_are_detected_by_schema() {
        let v21 = r#"{"info": {"name": "a", "schema": "https://schema.getpostman.com/json/collection/v2.1.0/collection.json"}, "item": []}"#;
        assert_eq!(detect(v21), Some(Format::Postman));
        let v20 = v21.replace("v2.1.0", "v2.0.0");
        assert_eq!(detect(&v20), Some(Format::Postman));
        let v1 = v21.replace("v2.1.0", "v1.0.0");
        assert_eq!(detect(&v1), None);
    }

    #[test]
    fn postman_environments_are_detected() {
        let env = r#"{"name": "Staging", "values": [], "_postman_variable_scope": "environment"}"#;
        assert_eq!(detect(env), Some(Format::PostmanEnv));
    }

    #[test]
    fn other_formats_are_detected_by_content() {
        let v4 = r#"{"_type": "export", "__export_format": 4, "resources": []}"#;
        assert_eq!(detect(v4), Some(Format::Insomnia));
        let v5 = "type: collection.insomnia.rest/5.0\nname: A\ncollection: []\n";
        assert_eq!(detect(v5), Some(Format::Insomnia));
        assert_eq!(
            detect(r#"{"log": {"version": "1.2", "entries": []}}"#),
            Some(Format::Har)
        );
        let bru = "meta {\n  name: a\n}\n\nget {\n  url: https://x\n}\n";
        assert_eq!(detect(bru), Some(Format::Bruno));
        assert_eq!(detect("GET https://x\n"), Some(Format::Http));
        assert_eq!(detect("### A\nPOST {{host}}/a\n"), Some(Format::Http));
    }

    #[test]
    fn unknown_content_is_not_detected() {
        assert_eq!(detect(r#"{"data": [1, 2]}"#), None);
        assert_eq!(detect("[\"GET https://x\"]"), None);
        assert_eq!(detect("just some text\nget the milk\n"), None);
        assert_eq!(detect(""), None);
    }
}
