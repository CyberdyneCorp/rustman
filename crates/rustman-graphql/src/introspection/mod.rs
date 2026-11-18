//! The standard introspection query and its result.
//!
//! [`INTROSPECTION_QUERY`] asks for everything the explorer and validation
//! need, including `isDeprecated`, `deprecationReason`, `specifiedByURL`
//! and `isRepeatable`. Servers that predate those fields reject it; they get
//! [`LEGACY_INTROSPECTION_QUERY`] instead (see [`needs_legacy_query`]).

mod model;
mod sdl;

pub use model::IntrospectionSchema;
pub use sdl::to_sdl;

use serde::Deserialize as _;
use serde_json::Value;

use crate::error::GraphqlError;

/// The operation name both introspection queries use.
pub const OPERATION_NAME: &str = "IntrospectionQuery";

/// The full introspection query (GraphQL October 2021 and later drafts).
pub const INTROSPECTION_QUERY: &str = r"query IntrospectionQuery {
  __schema {
    description
    queryType { name }
    mutationType { name }
    subscriptionType { name }
    types { ...FullType }
    directives {
      name
      description
      isRepeatable
      locations
      args(includeDeprecated: true) { ...InputValue }
    }
  }
}

fragment FullType on __Type {
  kind
  name
  description
  specifiedByURL
  fields(includeDeprecated: true) {
    name
    description
    args(includeDeprecated: true) { ...InputValue }
    type { ...TypeRef }
    isDeprecated
    deprecationReason
  }
  inputFields(includeDeprecated: true) { ...InputValue }
  interfaces { ...TypeRef }
  enumValues(includeDeprecated: true) {
    name
    description
    isDeprecated
    deprecationReason
  }
  possibleTypes { ...TypeRef }
}

fragment InputValue on __InputValue {
  name
  description
  type { ...TypeRef }
  defaultValue
  isDeprecated
  deprecationReason
}

fragment TypeRef on __Type {
  kind
  name
  ofType {
    kind
    name
    ofType {
      kind
      name
      ofType {
        kind
        name
        ofType {
          kind
          name
          ofType {
            kind
            name
            ofType {
              kind
              name
              ofType { kind name }
            }
          }
        }
      }
    }
  }
}
";

/// The introspection query of the GraphQL June 2018 specification: no
/// schema description, `specifiedByURL`, `isRepeatable`, or deprecated
/// arguments and input fields.
pub const LEGACY_INTROSPECTION_QUERY: &str = r"query IntrospectionQuery {
  __schema {
    queryType { name }
    mutationType { name }
    subscriptionType { name }
    types { ...FullType }
    directives {
      name
      description
      locations
      args { ...InputValue }
    }
  }
}

fragment FullType on __Type {
  kind
  name
  description
  fields(includeDeprecated: true) {
    name
    description
    args { ...InputValue }
    type { ...TypeRef }
    isDeprecated
    deprecationReason
  }
  inputFields { ...InputValue }
  interfaces { ...TypeRef }
  enumValues(includeDeprecated: true) {
    name
    description
    isDeprecated
    deprecationReason
  }
  possibleTypes { ...TypeRef }
}

fragment InputValue on __InputValue {
  name
  description
  type { ...TypeRef }
  defaultValue
}

fragment TypeRef on __Type {
  kind
  name
  ofType {
    kind
    name
    ofType {
      kind
      name
      ofType {
        kind
        name
        ofType {
          kind
          name
          ofType {
            kind
            name
            ofType {
              kind
              name
              ofType { kind name }
            }
          }
        }
      }
    }
  }
}
";

/// Names in an error message that show the server does not know a field
/// or argument of the full query, so the legacy query may work.
const NEWER_FIELDS: [&str; 4] = [
    "\"specifiedByURL\"",
    "\"isRepeatable\"",
    "\"includeDeprecated\"",
    "\"description\" on type \"__Schema\"",
];

/// Whether `errors` (the `errors` of a response without `data`) reject a
/// field or argument only the full query uses.
pub fn needs_legacy_query(errors: &[String]) -> bool {
    errors
        .iter()
        .any(|message| NEWER_FIELDS.iter().any(|field| message.contains(field)))
}

/// The `__schema` object of an introspection result: either a whole
/// response (`{"data": {"__schema": ...}}`), its `data`, or the object
/// itself.
pub fn schema_object(result: &Value) -> Option<&Value> {
    let data = result.get("data").unwrap_or(result);
    match data.get("__schema") {
        Some(schema) if schema.is_object() => Some(schema),
        _ => None,
    }
}

/// Parse the introspection result `result` (see [`schema_object`]).
pub fn parse(result: &Value) -> Result<IntrospectionSchema, GraphqlError> {
    let schema = schema_object(result)
        .ok_or_else(|| GraphqlError::InvalidIntrospection("no __schema object".into()))?;
    IntrospectionSchema::deserialize(schema)
        .map_err(|e| GraphqlError::InvalidIntrospection(e.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn the_schema_object_is_found_at_any_level() {
        let schema = json!({"queryType": {"name": "Query"}, "types": []});
        for result in [
            json!({"data": {"__schema": schema}}),
            json!({"__schema": schema}),
        ] {
            assert_eq!(schema_object(&result), Some(&schema));
        }
        assert_eq!(schema_object(&json!({"data": null})), None);
        let err = parse(&json!({"errors": []})).unwrap_err();
        assert_eq!(
            err.to_string(),
            "graphql: invalid introspection result: no __schema object"
        );
    }

    #[test]
    fn only_errors_about_newer_fields_retry_with_the_legacy_query() {
        let unknown = "Cannot query field \"specifiedByURL\" on type \"__Type\".".to_owned();
        assert!(needs_legacy_query(&[unknown]));
        let disabled = "GraphQL introspection is not allowed".to_owned();
        assert!(!needs_legacy_query(&[disabled]));
    }

    #[test]
    fn both_queries_are_valid_against_the_introspection_schema() {
        let schema =
            apollo_compiler::Schema::parse_and_validate("type Query { a: Int }", "s").unwrap();
        for query in [INTROSPECTION_QUERY, LEGACY_INTROSPECTION_QUERY] {
            apollo_compiler::ExecutableDocument::parse_and_validate(&schema, query, "q.graphql")
                .unwrap();
        }
    }
}
