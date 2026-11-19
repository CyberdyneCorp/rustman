//! The rule catalog (contract-testing "Rule catalog is documented and
//! versioned"): every difference the diff reports names one of these rules,
//! each with a stable kebab-case id, a classification, a one-line
//! description and an example.

use std::fmt;

use serde::Serialize;

/// The catalog version recorded in every diff. It is bumped in the same
/// change that alters a classification (or adds or removes a rule); the
/// `catalog` test fails until it is.
pub const CATALOG_VERSION: u32 = 2;

/// How a difference affects existing clients.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Classification {
    /// Existing clients can fail.
    Breaking,
    /// Existing clients keep working.
    NonBreaking,
    /// The catalog cannot decide (composition, vendor extensions, ...).
    Unknown,
}

impl Classification {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Breaking => "breaking",
            Self::NonBreaking => "non-breaking",
            Self::Unknown => "unknown",
        }
    }
}

impl fmt::Display for Classification {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// One catalog entry, as listed by `rustman contract rules`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct RuleInfo {
    pub id: &'static str,
    pub classification: Classification,
    pub description: &'static str,
    pub example: &'static str,
}

macro_rules! catalog {
    ($($variant:ident => $id:literal, $class:ident, $description:literal, $example:literal;)*) => {
        /// A rule of the catalog.
        #[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
        pub enum RuleId {
            $($variant,)*
        }

        impl RuleId {
            /// Every rule, in catalog order.
            pub const ALL: &'static [RuleId] = &[$(RuleId::$variant,)*];

            /// The rule's catalog entry.
            pub fn info(self) -> &'static RuleInfo {
                match self {
                    $(RuleId::$variant => &RuleInfo {
                        id: $id,
                        classification: Classification::$class,
                        description: $description,
                        example: $example,
                    },)*
                }
            }
        }
    };
}

catalog! {
    // Breaking.
    OperationRemoved => "operation-removed", Breaking,
        "An operation is removed.",
        "`DELETE /users/{id}` is in the old document only";
    ResponseStatusRemoved => "response-status-removed", Breaking,
        "A response status is removed from an operation.",
        "`GET /users/{id}` no longer declares `404`";
    ResponseMediaTypeRemoved => "response-media-type-removed", Breaking,
        "A response media type is removed.",
        "the `200` response drops `application/xml`";
    ResponseFieldRemoved => "response-field-removed", Breaking,
        "A response field is removed or renamed.",
        "`$.email` disappears from the `200` body";
    ResponseFieldBecameOptional => "response-field-became-optional", Breaking,
        "A response field is no longer required.",
        "`email` leaves the `required` list of the `200` body";
    ResponseFieldTypeChanged => "response-field-type-changed", Breaking,
        "A response field's type changes or accepts more types.",
        "`$.id` goes from `integer` to `string`";
    ResponseHeaderRemoved => "response-header-removed", Breaking,
        "A response header is removed.",
        "the `200` response no longer declares `X-Rate-Limit`";
    ResponseHeaderBecameOptional => "response-header-became-optional", Breaking,
        "A response header is no longer required.",
        "`X-Rate-Limit` changes to `required: false` in the `200` response";
    ResponseEnumNarrowed => "response-enum-narrowed", Breaking,
        "Values are removed from a response enum.",
        "`$.status` goes from `[active, suspended, deleted]` to `[active, suspended]`";
    RequestRequiredFieldAdded => "request-required-field-added", Breaking,
        "A request body field becomes required, or a new required field appears.",
        "`email` is added to the `required` list of the `POST /users` body";
    RequestFieldRemovedFromClosedSchema => "request-field-removed-from-closed-schema", Breaking,
        "A request body field is removed from a schema with `additionalProperties: false`; clients that still send it are rejected.",
        "`POST /users` no longer declares `nickname` and allows no other properties";
    RequestRequiredParameterAdded => "request-required-parameter-added", Breaking,
        "A new required parameter appears.",
        "`GET /users` gains a required query parameter `tenant`";
    ParameterBecameRequired => "parameter-became-required", Breaking,
        "An optional parameter becomes required.",
        "query parameter `limit` changes to `required: true`";
    RequestBodyBecameRequired => "request-body-became-required", Breaking,
        "A request body becomes required, or a new required body appears.",
        "`PATCH /users/{id}` changes its body to `required: true`";
    RequestMediaTypeRemoved => "request-media-type-removed", Breaking,
        "A request body media type is removed.",
        "`POST /users` no longer accepts `application/x-www-form-urlencoded`";
    RequestTypeChanged => "request-type-changed", Breaking,
        "A request field's type changes to one that rejects old values.",
        "`$.age` goes from `[integer, string]` to `integer`";
    RequestEnumNarrowed => "request-enum-narrowed", Breaking,
        "Values are removed from a request enum.",
        "query parameter `sort` goes from `[asc, desc]` to `[asc]`";
    RequestConstraintTightened => "request-constraint-tightened", Breaking,
        "A request constraint gets stricter (`maxLength`, `maximum`, `pattern`, `minItems`, `additionalProperties`, `items`, ...).",
        "`$.name` goes from `maxLength: 100` to `maxLength: 50`";
    ParameterLocationChanged => "parameter-location-changed", Breaking,
        "A parameter (or an API key) moves to another location.",
        "`api_key` moves from a query parameter to a header";
    SecurityRequirementRemoved => "security-requirement-removed", Breaking,
        "Every security alternative an operation accepted is removed.",
        "`GET /users` accepted `basicAuth` and now accepts only `bearerAuth`";
    // Non-breaking.
    OperationAdded => "operation-added", NonBreaking,
        "An operation is added.",
        "`GET /health` is in the new document only";
    OperationDeprecated => "operation-deprecated", NonBreaking,
        "An operation is marked deprecated.",
        "`GET /users/{id}` gains `deprecated: true`";
    PathParameterRenamed => "path-parameter-renamed", NonBreaking,
        "A path parameter is renamed; the path template is the same.",
        "`/users/{id}` becomes `/users/{userId}`";
    ResponseStatusAdded => "response-status-added", NonBreaking,
        "A response status is added to an operation.",
        "`GET /users/{id}` declares a new `429`";
    ResponseMediaTypeAdded => "response-media-type-added", NonBreaking,
        "A response media type is added.",
        "the `200` response adds `application/xml`";
    ResponseFieldAdded => "response-field-added", NonBreaking,
        "A response field is added to a schema that allows other properties.",
        "`$.nickname` appears in the `200` body";
    ResponseFieldBecameRequired => "response-field-became-required", NonBreaking,
        "A response field becomes required.",
        "`nickname` joins the `required` list of the `200` body";
    ResponseHeaderAdded => "response-header-added", NonBreaking,
        "A response header is added.",
        "the `200` response declares a new `X-Request-Id`";
    ResponseHeaderBecameRequired => "response-header-became-required", NonBreaking,
        "A response header becomes required.",
        "`X-Request-Id` changes to `required: true` in the `200` response";
    ResponseTypeNarrowed => "response-type-narrowed", NonBreaking,
        "A response field accepts fewer types.",
        "`$.nickname` goes from `[string, null]` to `string`";
    ResponseConstraintTightened => "response-constraint-tightened", NonBreaking,
        "A response constraint gets stricter.",
        "`$.name` goes from `maxLength: 100` to `maxLength: 50`";
    RequestOptionalFieldAdded => "request-optional-field-added", NonBreaking,
        "An optional request body field is added.",
        "`POST /users` accepts an optional `nickname`";
    RequestOptionalParameterAdded => "request-optional-parameter-added", NonBreaking,
        "An optional parameter is added.",
        "`GET /users` gains an optional query parameter `page`";
    RequestFieldBecameOptional => "request-field-became-optional", NonBreaking,
        "A request body field is no longer required.",
        "`email` leaves the `required` list of the `POST /users` body";
    ParameterBecameOptional => "parameter-became-optional", NonBreaking,
        "A required parameter becomes optional.",
        "header `X-Tenant` changes to `required: false`";
    RequestBodyAdded => "request-body-added", NonBreaking,
        "An optional request body is added.",
        "`DELETE /users/{id}` accepts an optional JSON body";
    RequestBodyBecameOptional => "request-body-became-optional", NonBreaking,
        "A required request body becomes optional.",
        "`PATCH /users/{id}` changes its body to `required: false`";
    RequestMediaTypeAdded => "request-media-type-added", NonBreaking,
        "A request body media type is added.",
        "`POST /users` also accepts `application/x-www-form-urlencoded`";
    RequestTypeWidened => "request-type-widened", NonBreaking,
        "A request field accepts more types.",
        "`$.age` goes from `integer` to `[integer, string]`";
    RequestEnumWidened => "request-enum-widened", NonBreaking,
        "Values are added to a request enum.",
        "query parameter `sort` goes from `[asc]` to `[asc, desc]`";
    RequestConstraintLoosened => "request-constraint-loosened", NonBreaking,
        "A request constraint gets looser.",
        "`$.name` goes from `maxLength: 50` to `maxLength: 100`";
    SecurityAlternativeAdded => "security-alternative-added", NonBreaking,
        "A security alternative is added to an operation.",
        "`GET /users` also accepts `apiKey`";
    DocsChanged => "docs-changed", NonBreaking,
        "Only documentation changes (`summary`, `description`, `title`, examples).",
        "the `GET /users` description is reworded";
    // Unknown.
    ResponseEnumWidened => "response-enum-widened", Unknown,
        "Values are added to a response enum; clients may not handle them.",
        "`$.status` goes from `[active]` to `[active, archived]`";
    ResponseConstraintLoosened => "response-constraint-loosened", Unknown,
        "A response constraint gets looser; clients may not handle the new values.",
        "`$.name` goes from `maxLength: 50` to `maxLength: 100`";
    ConstraintChanged => "constraint-changed", Unknown,
        "A constraint changes in a way that cannot be ordered exactly as stricter or looser.",
        "`$.amount` goes from `multipleOf: 3` to `multipleOf: 1e300`";
    ResponseFieldAddedToClosedSchema => "response-field-added-to-closed-schema", Unknown,
        "A response field is added to a schema with `additionalProperties: false`.",
        "`$.nickname` appears in a `200` body that allowed no other properties";
    RequestFieldRemoved => "request-field-removed", Unknown,
        "A request body field is removed; clients may still send it.",
        "`POST /users` no longer declares `nickname`";
    ParameterRemoved => "parameter-removed", Unknown,
        "A parameter is removed; clients may still send it.",
        "`GET /users` no longer declares the query parameter `page`";
    RequestBodyRemoved => "request-body-removed", Unknown,
        "A request body is removed; clients may still send one.",
        "`DELETE /users/{id}` no longer declares a body";
    SecurityAlternativeRemoved => "security-alternative-removed", Unknown,
        "Some, but not all, security alternatives of an operation are removed.",
        "`GET /users` accepted `basicAuth` or `bearerAuth` and now only `bearerAuth`";
    CompositionChanged => "composition-changed", Unknown,
        "A `oneOf`, `anyOf`, `allOf` or `not` composition changes.",
        "a `200` body goes from `oneOf: [A, B]` to `oneOf: [A, B, C]`";
    VendorExtensionChanged => "vendor-extension-changed", Unknown,
        "An `x-` vendor extension changes.",
        "`x-rate-limit` goes from `100` to `50`";
}

impl RuleId {
    pub fn id(self) -> &'static str {
        self.info().id
    }

    pub fn classification(self) -> Classification {
        self.info().classification
    }

    /// The rule with `id`.
    pub fn from_id(id: &str) -> Option<RuleId> {
        Self::ALL.iter().copied().find(|rule| rule.id() == id)
    }
}

impl fmt::Display for RuleId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.id())
    }
}

impl Serialize for RuleId {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.id())
    }
}

/// One entry of `rustman contract rules --json`.
#[derive(Debug, Serialize)]
pub struct ListedRule {
    pub id: &'static str,
    pub classification: Classification,
    pub description: &'static str,
    pub example: &'static str,
    pub catalog_version: u32,
}

/// The catalog as listed by `rustman contract rules --json`: a JSON array,
/// each entry carrying the catalog version.
pub fn listing() -> Vec<ListedRule> {
    RuleId::ALL
        .iter()
        .map(|rule| {
            let info = rule.info();
            ListedRule {
                id: info.id,
                classification: info.classification,
                description: info.description,
                example: info.example,
                catalog_version: CATALOG_VERSION,
            }
        })
        .collect()
}

/// The catalog as a table: one line per rule, columns aligned.
pub fn table() -> String {
    let id_width = width(|info| info.id.len());
    let class_width = width(|info| info.classification.as_str().len());
    let mut out = format!("rule catalog version {CATALOG_VERSION}\n\n");
    out.push_str(&format!(
        "{:id_width$}  {:class_width$}  DESCRIPTION\n",
        "RULE", "CLASSIFICATION"
    ));
    for rule in RuleId::ALL {
        let info = rule.info();
        out.push_str(&format!(
            "{:id_width$}  {:class_width$}  {}\n",
            info.id,
            info.classification.as_str(),
            info.description
        ));
    }
    out
}

fn width(len: impl Fn(&RuleInfo) -> usize) -> usize {
    RuleId::ALL
        .iter()
        .map(|rule| len(rule.info()))
        .max()
        .unwrap_or(0)
        .max("CLASSIFICATION".len())
}

/// `docs/contract-rules.md`, generated from the catalog.
pub fn markdown() -> String {
    let mut out = MARKDOWN_HEAD.replace("{version}", &CATALOG_VERSION.to_string());
    for class in [
        Classification::Breaking,
        Classification::NonBreaking,
        Classification::Unknown,
    ] {
        out.push_str(&format!("\n## {}\n\n", heading(class)));
        out.push_str("| Rule | Description | Example |\n|------|-------------|---------|\n");
        for rule in RuleId::ALL.iter().filter(|r| r.classification() == class) {
            let info = rule.info();
            out.push_str(&format!(
                "| `{}` | {} | {} |\n",
                info.id,
                info.description.replace('|', "\\|"),
                info.example.replace('|', "\\|")
            ));
        }
    }
    out
}

fn heading(class: Classification) -> &'static str {
    match class {
        Classification::Breaking => "Breaking",
        Classification::NonBreaking => "Non-breaking",
        Classification::Unknown => "Unknown",
    }
}

const MARKDOWN_HEAD: &str = "\
# Contract rule catalog

<!-- Generated from crates/rustman-contract/src/catalog.rs; regenerate with
RUSTMAN_UPDATE_DOCS=1 cargo test -p rustman-contract --test catalog -->

`rustman contract diff` compares two OpenAPI 3.0/3.1 documents and reports every difference with one of the rules below. `rustman contract rules` prints the same list (`--json` for a JSON array).

Catalog version: **{version}**. The version is recorded in every diff and changes whenever a rule is added, removed or reclassified.

## How differences are classified

Documents are compared after loading them as [OpenAPI import](import-openapi.md) does: references bundled, OpenAPI 3.0 schemas rewritten to 3.1 (`nullable: true` is the same as a `null` type) and path-item parameters merged into each operation. Operations are matched by method and path template, ignoring parameter names (`/users/{id}` and `/users/{userId}` are the same operation, reported as `path-parameter-renamed`). Path parameters are matched by position, other parameters by name and location (header names ignore case).

Schemas are compared with a direction:

- **Request** schemas (parameters and request bodies) break clients when they get stricter: a new required field, a narrowed enum, a tighter constraint, a type that rejects old values.
- **Response** schemas break clients when they lose guarantees: a removed field, a field that is no longer required, a type that accepts new kinds of values, a narrowed enum. A widened response enum or a looser response constraint may surprise clients and is `unknown`.

`$ref`s are followed (each difference points to where the change is, such as `/components/schemas/User/properties/email`), and recursive schemas are compared once per level. The members of an `allOf` add up to one object: their properties, `required` lists and types are compared like those of any object, so moving a field between members, or wrapping a schema in an `allOf`, is not a change, and a field that disappears with a removed member is. What else an `allOf` member says (constraints, enums, nested `oneOf`), and any change inside `oneOf`, `anyOf` and `not`, is not proven equivalent or compatible: it is reported as `composition-changed` (`unknown`).

Response headers are compared like parameters, by name (ignoring case), and their schemas in the response direction.

`items` and `additionalProperties` are constraints too. Absent, `true` and an empty schema accept anything, `false` accepts nothing, and a schema is in between: a move towards `false` (`additionalProperties` absent -> `{type: integer}` -> `false`, or `items` appearing) is stricter, a move the other way looser, and two schemas are compared member by member. `additionalProperties: false` in an `allOf` member closes the whole object. `multipleOf` is compared by exact decimal divisibility: `10 -> 5` is looser (every multiple of 10 is a multiple of 5), `5 -> 10` stricter, and `4 -> 6` both, so it is breaking in a request and `unknown` in a response. Values too large or too precise to compare exactly are `constraint-changed`.

A removed response status that the new document still covers is not `response-status-removed`: OpenAPI lets a range (`1XX` to `5XX`) stand for every code of its class that is not declared, and `default` for every status that is not declared otherwise. So when `200` gives way to `2XX` (or, without `2XX`, to `default`), and when a range gives way to `default`, the old response is compared with the one that now covers it, as `response 200 (now 2XX)`.

Each difference carries a JSON pointer into each document. When an element exists in one document only, the pointer into the other is the closest enclosing element (for a removed operation, its path item or `/paths`).
";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_are_unique_and_kebab_case() {
        let mut ids: Vec<&str> = RuleId::ALL.iter().map(|r| r.id()).collect();
        assert!(ids.iter().all(|id| {
            id.chars().all(|c| c.is_ascii_lowercase() || c == '-')
                && !id.starts_with('-')
                && !id.ends_with('-')
        }));
        let count = ids.len();
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(ids.len(), count);
    }

    #[test]
    fn ids_round_trip() {
        for rule in RuleId::ALL {
            assert_eq!(RuleId::from_id(rule.id()), Some(*rule));
        }
        assert_eq!(RuleId::from_id("no-such-rule"), None);
    }

    #[test]
    fn every_rule_has_a_description_and_an_example() {
        for rule in RuleId::ALL {
            let info = rule.info();
            assert!(info.description.ends_with('.'), "{}", info.id);
            assert!(!info.example.is_empty(), "{}", info.id);
            assert!(!info.description.contains('\n'), "{}", info.id);
        }
    }

    #[test]
    fn the_table_lists_every_rule_with_the_version() {
        let table = table();
        assert!(table.starts_with(&format!("rule catalog version {CATALOG_VERSION}\n")));
        assert_eq!(table.lines().count(), RuleId::ALL.len() + 3);
        assert!(table.contains("operation-removed"));
    }
}
