//! A GraphQL schema built from SDL or an introspection result, behind our
//! own type so the rest of Rustman does not depend on the
//! `apollo-compiler` API directly (design risk: API churn).

use std::path::Path;

use apollo_compiler::Schema;
use apollo_compiler::validation::{DiagnosticList, Valid};
use serde_json::Value;

use crate::error::GraphqlError;
use crate::introspection;

/// Diagnostics shown in an error or warning list, at most.
const SHOWN_DIAGNOSTICS: usize = 5;

/// A parsed schema.
///
/// SDL that does not parse is an error. A schema that parses but breaks a
/// validation rule is kept, with the problems in [`GraphqlSchema::warnings`]:
/// a server's introspection result is the schema it executes, even when it
/// is not strictly valid.
#[derive(Clone, Debug)]
pub struct GraphqlSchema {
    schema: Valid<Schema>,
    warnings: Vec<String>,
}

impl GraphqlSchema {
    /// Parse `sdl`; `origin` (a file name or endpoint) names it in messages.
    pub fn from_sdl(sdl: &str, origin: &str) -> Result<Self, GraphqlError> {
        let schema = Schema::parse(sdl, origin).map_err(|e| GraphqlError::InvalidSchema {
            origin: origin.to_owned(),
            message: summary(&e.errors),
        })?;
        Ok(match schema.validate() {
            Ok(schema) => Self {
                schema,
                warnings: Vec::new(),
            },
            Err(invalid) => Self {
                warnings: messages(&invalid.errors),
                schema: Valid::assume_valid(invalid.partial),
            },
        })
    }

    /// Build the schema of the introspection result `result` (a whole
    /// response, its `data`, or the `__schema` object).
    pub fn from_introspection(result: &Value, origin: &str) -> Result<Self, GraphqlError> {
        let parsed = introspection::parse(result)?;
        Self::from_sdl(&introspection::to_sdl(&parsed)?, origin)
    }

    /// Read a schema file: introspection JSON when it ends in `.json` or
    /// starts with `{`, SDL otherwise. `shown` names it in messages.
    pub fn from_file(path: &Path, shown: &str) -> Result<Self, GraphqlError> {
        let text = std::fs::read_to_string(path).map_err(|e| GraphqlError::Read {
            path: shown.to_owned(),
            message: e.to_string(),
        })?;
        let is_json =
            path.extension().is_some_and(|e| e == "json") || text.trim_start().starts_with('{');
        if !is_json {
            return Self::from_sdl(&text, shown);
        }
        let value: Value =
            serde_json::from_str(&text).map_err(|e| GraphqlError::InvalidSchema {
                origin: shown.to_owned(),
                message: e.to_string(),
            })?;
        Self::from_introspection(&value, shown)
    }

    /// The typed schema, for validation, completion and the explorer.
    pub fn schema(&self) -> &Valid<Schema> {
        &self.schema
    }

    /// Validation problems of a schema that was kept anyway.
    pub fn warnings(&self) -> &[String] {
        &self.warnings
    }

    /// The schema as SDL, without built-in definitions.
    pub fn sdl(&self) -> String {
        self.schema.to_string()
    }

    /// The SHA-256 of [`GraphqlSchema::sdl`], in lowercase hex.
    pub fn sdl_sha256(&self) -> String {
        sha256_hex(self.sdl().as_bytes())
    }

    /// Named types defined by the schema (built-in and introspection types
    /// excluded).
    pub fn type_count(&self) -> usize {
        self.schema
            .types
            .values()
            .filter(|t| !t.is_built_in())
            .count()
    }
}

/// The SHA-256 of `bytes`, in lowercase hex.
pub fn sha256_hex(bytes: &[u8]) -> String {
    let digest = ring::digest::digest(&ring::digest::SHA256, bytes);
    digest.as_ref().iter().map(|b| format!("{b:02x}")).collect()
}

/// Each diagnostic as `line:column: message`.
fn messages(errors: &DiagnosticList) -> Vec<String> {
    errors
        .iter()
        .map(|d| {
            let json = d.to_json();
            match json.locations.first() {
                Some(at) => format!("{}:{}: {}", at.line, at.column, json.message),
                None => json.message,
            }
        })
        .collect()
}

/// The first few diagnostics on one line.
fn summary(errors: &DiagnosticList) -> String {
    let all = messages(errors);
    let mut shown: Vec<String> = all.iter().take(SHOWN_DIAGNOSTICS).cloned().collect();
    if all.len() > SHOWN_DIAGNOSTICS {
        shown.push(format!("and {} more", all.len() - SHOWN_DIAGNOSTICS));
    }
    shown.join("; ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sdl_builds_a_schema_with_its_type_count() {
        let schema = GraphqlSchema::from_sdl(
            "type Query { user(id: ID!): User }\ntype User { name: String }",
            "s.graphql",
        )
        .unwrap();
        assert_eq!(schema.type_count(), 2);
        assert!(schema.warnings().is_empty());
        assert!(schema.sdl().contains("type User"));
        assert_eq!(schema.sdl_sha256().len(), 64);
    }

    #[test]
    fn syntax_errors_name_the_origin_and_position() {
        let err = GraphqlSchema::from_sdl("type Query {\n  a: Int\n", "api.graphql").unwrap_err();
        let text = err.to_string();
        assert!(
            text.starts_with("graphql: invalid schema api.graphql: 2:10: syntax error"),
            "{text}"
        );
    }

    #[test]
    fn schemas_breaking_validation_rules_are_kept_with_warnings() {
        let schema = GraphqlSchema::from_sdl("type Query { a: Missing }", "s.graphql").unwrap();
        assert_eq!(schema.warnings().len(), 1, "{:?}", schema.warnings());
        assert!(schema.warnings()[0].contains("Missing"));
    }

    #[test]
    fn the_hash_is_lowercase_hex_sha256() {
        assert_eq!(
            sha256_hex(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }
}
