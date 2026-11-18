//! OpenAPI documents served by the test server under `/openapi/<name>`, for
//! the import tests that read a document from a URL.

/// `/openapi/petstore.yaml`: a small OpenAPI 3.1 document in YAML, whose
/// relative server URL resolves against the URL it is read from.
pub const PETSTORE_YAML: &str = "\
openapi: 3.1.0
info:
  title: URL Petstore
  version: '1'
servers:
  - url: /v1
    description: Local
paths:
  /pets:
    get:
      operationId: listPets
      tags: [pets]
      responses:
        '200':
          description: Pets
";

/// `/openapi/pet.json`: a schema file, the target of remote `$ref`s.
pub const PET_SCHEMA_JSON: &str =
    r#"{"Pet": {"type": "object", "properties": {"name": {"type": "string"}}}}"#;

/// The body and content type served for `name`.
pub fn document(name: &str) -> Option<(&'static str, &'static str)> {
    match name {
        "petstore.yaml" => Some((PETSTORE_YAML, "application/yaml")),
        "pet.json" => Some((PET_SCHEMA_JSON, "application/json")),
        _ => None,
    }
}
