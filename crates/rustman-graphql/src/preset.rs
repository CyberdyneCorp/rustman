//! Blockchain endpoint presets (add-evm-toolkit "blockchain-graphql",
//! design D11): `geth` (EIP-1767) and `subgraph` (The Graph). A preset
//! never changes what is sent; it is detected from the schema or named by
//! the request file, and tells front ends which result leaves hold
//! blockchain scalars so they can format them for display.

use std::collections::BTreeMap;

use apollo_compiler::ExecutableDocument;
use apollo_compiler::executable::{Field, Operation, Selection, SelectionSet};
use apollo_compiler::schema::ExtendedType;
use rustman_core::model::GraphqlPreset;
use rustman_core::run::PresetRecord;

use crate::operation::DOCUMENT_PATH;
use crate::schema::GraphqlSchema;

/// The warning for `preset: geth` on a schema without geth's markers.
pub const NOT_GETH: &str = "schema does not look like geth EIP-1767";

/// The warning for `preset: subgraph` on a schema without `_meta`.
pub const NOT_SUBGRAPH: &str = "schema does not look like a subgraph";

/// The scalars presets format for display.
pub const BLOCKCHAIN_SCALARS: [&str; 6] = [
    "BigInt",
    "BigDecimal",
    "Long",
    "Bytes",
    "Bytes32",
    "Address",
];

/// How deep fragment spreads are followed (a cycle in a document that
/// does not validate stops here).
const MAX_DEPTH: usize = 32;

/// The preset `schema` looks like: `geth` when `Query` has `block` and
/// `logs` and the schema has the `Long` and `Bytes32` scalars, `subgraph`
/// when `Query` has `_meta`.
pub fn detect(schema: &GraphqlSchema) -> Option<GraphqlPreset> {
    let schema = schema.schema();
    let query = schema.schema_definition.query.as_ref()?;
    let Some(ExtendedType::Object(root)) = schema.types.get(query.as_str()) else {
        return None;
    };
    let has_field = |name: &str| root.fields.contains_key(name);
    let has_scalar = |name: &str| matches!(schema.types.get(name), Some(ExtendedType::Scalar(_)));
    if has_field("_meta") {
        return Some(GraphqlPreset::Subgraph);
    }
    let geth = has_field("block") && has_field("logs");
    (geth && has_scalar("Long") && has_scalar("Bytes32")).then_some(GraphqlPreset::Geth)
}

/// The preset in force: the request's `preset:` (with a warning when the
/// schema does not look like it), else the detected one.
pub fn choose(
    explicit: Option<GraphqlPreset>,
    schema: Option<&GraphqlSchema>,
) -> Option<PresetRecord> {
    let detected = schema.and_then(detect);
    let (preset, explicit) = match explicit {
        Some(preset) => (preset, true),
        None => (detected?, false),
    };
    let warning = match (explicit, schema) {
        (true, Some(_)) if detected != Some(preset) => Some(mismatch(preset).to_owned()),
        _ => None,
    };
    Some(PresetRecord {
        name: preset.as_str().to_owned(),
        explicit,
        warning,
        ..PresetRecord::default()
    })
}

/// The warning for an explicit `preset` the schema does not match.
pub fn mismatch(preset: GraphqlPreset) -> &'static str {
    match preset {
        GraphqlPreset::Geth => NOT_GETH,
        GraphqlPreset::Subgraph => NOT_SUBGRAPH,
    }
}

/// The preset named `name` (`geth`, `subgraph`).
pub fn parse(name: &str) -> Option<GraphqlPreset> {
    match name {
        "geth" => Some(GraphqlPreset::Geth),
        "subgraph" => Some(GraphqlPreset::Subgraph),
        _ => None,
    }
}

/// The blockchain scalar of each result leaf of the operation
/// `operation_name` in `document`, by response path: `transfers[].value`
/// for the `value` of each item of the list `transfers`. Aliases name the
/// path; fields the schema does not know are left out.
pub fn scalar_paths(
    schema: &GraphqlSchema,
    document: &str,
    operation_name: Option<&str>,
) -> BTreeMap<String, String> {
    let doc = match ExecutableDocument::parse(schema.schema(), document, DOCUMENT_PATH) {
        Ok(doc) => doc,
        Err(with_errors) => with_errors.partial,
    };
    let mut walker = Walker {
        doc: &doc,
        found: BTreeMap::new(),
    };
    if let Some(operation) = operation(&doc, operation_name) {
        walker.selection_set(&operation.selection_set, "", 0);
    }
    walker.found
}

fn operation<'d>(doc: &'d ExecutableDocument, name: Option<&str>) -> Option<&'d Operation> {
    match name {
        Some(name) => doc.operations.get(Some(name)).ok().map(|o| o.as_ref()),
        None => doc.operations.iter().next().map(|o| o.as_ref()),
    }
}

struct Walker<'d> {
    doc: &'d ExecutableDocument,
    found: BTreeMap<String, String>,
}

impl Walker<'_> {
    fn selection_set(&mut self, set: &SelectionSet, prefix: &str, depth: usize) {
        if depth > MAX_DEPTH {
            return;
        }
        for selection in &set.selections {
            match selection {
                Selection::Field(field) => self.field(field, prefix, depth),
                Selection::InlineFragment(inline) => {
                    self.selection_set(&inline.selection_set, prefix, depth + 1);
                }
                Selection::FragmentSpread(spread) => {
                    if let Some(fragment) = self.doc.fragments.get(&spread.fragment_name) {
                        self.selection_set(&fragment.selection_set, prefix, depth + 1);
                    }
                }
            }
        }
    }

    fn field(&mut self, field: &Field, prefix: &str, depth: usize) {
        let ty = &field.definition.ty;
        let list = if ty.is_list() { "[]" } else { "" };
        let path = match prefix {
            "" => format!("{}{list}", field.response_key()),
            _ => format!("{prefix}.{}{list}", field.response_key()),
        };
        let named = ty.inner_named_type().as_str();
        if field.selection_set.selections.is_empty() {
            if BLOCKCHAIN_SCALARS.contains(&named) {
                self.found.insert(path, named.to_owned());
            }
            return;
        }
        self.selection_set(&field.selection_set, &path, depth + 1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SUBGRAPH: &str = r#"
        scalar BigInt
        scalar Bytes
        type _Block_ { number: Int! }
        type _Meta_ { block: _Block_! hasIndexingErrors: Boolean! }
        type Token { id: ID! totalSupply: BigInt! decimals: Int! owner: Bytes! }
        type Query { _meta: _Meta_ tokens(first: Int): [Token!]! token(id: ID!): Token }
    "#;

    const GETH: &str = r#"
        scalar Long
        scalar Bytes32
        scalar BigInt
        scalar Address
        type Block { number: Long! hash: Bytes32! gasUsed: Long! }
        type Log { index: Long! }
        type Query { block(number: Long): Block logs(filter: String): [Log!]! }
    "#;

    fn schema(sdl: &str) -> GraphqlSchema {
        GraphqlSchema::from_sdl(sdl, "test").unwrap()
    }

    #[test]
    fn detects_presets_from_markers() {
        assert_eq!(detect(&schema(SUBGRAPH)), Some(GraphqlPreset::Subgraph));
        assert_eq!(detect(&schema(GETH)), Some(GraphqlPreset::Geth));
        let plain = schema("type Query { block: Int logs: Int }");
        assert_eq!(detect(&plain), None, "no Long/Bytes32 scalars");
    }

    #[test]
    fn explicit_presets_win_with_a_warning() {
        let plain = schema("type Query { hello: String }");
        let chosen = choose(Some(GraphqlPreset::Geth), Some(&plain)).unwrap();
        assert_eq!(chosen.name, "geth");
        assert!(chosen.explicit);
        assert_eq!(chosen.warning.as_deref(), Some(NOT_GETH));
        let quiet = choose(Some(GraphqlPreset::Geth), Some(&schema(GETH))).unwrap();
        assert_eq!(quiet.warning, None);
        let unknown = choose(Some(GraphqlPreset::Subgraph), None).unwrap();
        assert_eq!(unknown.warning, None, "no schema: nothing to compare");
        let detected = choose(None, Some(&schema(SUBGRAPH))).unwrap();
        assert_eq!(
            (detected.name.as_str(), detected.explicit),
            ("subgraph", false)
        );
        assert_eq!(choose(None, Some(&plain)), None);
        assert_eq!(parse("geth"), Some(GraphqlPreset::Geth));
        assert_eq!(parse("other"), None);
    }

    #[test]
    fn scalar_paths_follow_aliases_lists_and_fragments() {
        let document = r#"
            query Q { all: tokens(first: 2) { ...T supply: totalSupply } one: token(id: "1") { owner } }
            fragment T on Token { id decimals owner }
        "#;
        let paths = scalar_paths(&schema(SUBGRAPH), document, Some("Q"));
        let expected: BTreeMap<String, String> = [
            ("all[].owner", "Bytes"),
            ("all[].supply", "BigInt"),
            ("one.owner", "Bytes"),
        ]
        .into_iter()
        .map(|(k, v)| (k.to_owned(), v.to_owned()))
        .collect();
        assert_eq!(paths, expected);
    }
}
