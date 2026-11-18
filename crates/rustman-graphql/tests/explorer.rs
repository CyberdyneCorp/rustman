//! Schema explorer data (add-graphql-client task 3.5).

use rustman_graphql::{GraphqlSchema, TypeKind};

const LIBRARY: &str = include_str!("fixtures/library.graphql");

fn schema() -> GraphqlSchema {
    GraphqlSchema::from_sdl(LIBRARY, "library.graphql").unwrap()
}

fn pretty(value: &impl serde::Serialize) -> String {
    serde_json::to_string_pretty(value).unwrap()
}

#[test]
fn browsing_a_type_lists_every_field_with_signature_and_deprecation() {
    let user = schema().type_detail("User").unwrap();
    assert_eq!(user.kind, TypeKind::Object);
    assert_eq!(user.interfaces, ["Node"]);
    let signatures: Vec<String> = user.fields.iter().map(|f| f.signature()).collect();
    assert_eq!(
        signatures,
        [
            "id: ID!",
            "name: String",
            "email: String",
            "username: String",
            "handle: String",
            "role: Role!",
            "createdAt: DateTime",
            "books(first: Int = 5, after: String): [Book!]!",
        ]
    );
    let username = user.fields.iter().find(|f| f.name == "username").unwrap();
    assert_eq!(username.deprecated.as_deref(), Some("Use handle"));
    insta::assert_snapshot!("user_type", pretty(&user));
}

#[test]
fn the_overview_has_roots_types_and_directives() {
    let overview = schema().overview();
    assert_eq!(overview.query.as_deref(), Some("Query"));
    assert_eq!(overview.mutation.as_deref(), Some("Mutation"));
    assert_eq!(overview.subscription.as_deref(), Some("Subscription"));
    assert!(overview.types.iter().all(|t| !t.name.starts_with("__")));
    assert!(overview.directives.iter().any(|d| d.name == "cached"));
    insta::assert_snapshot!("overview", pretty(&overview));
}

#[test]
fn each_kind_exposes_its_own_details() {
    let schema = schema();
    let role = schema.type_detail("Role").unwrap();
    insta::assert_snapshot!("role_enum", pretty(&role));
    let input = schema.type_detail("CreateUserInput").unwrap();
    insta::assert_snapshot!("create_user_input", pretty(&input));
    let node = schema.type_detail("Node").unwrap();
    assert_eq!(node.possible_types, ["User", "Book"]);
    let union = schema.type_detail("SearchResult").unwrap();
    assert_eq!(union.possible_types, ["User", "Book"]);
    let date = schema.type_detail("DateTime").unwrap();
    assert_eq!(
        date.specified_by.as_deref(),
        Some("https://scalars.graphql.org/andimarek/date-time")
    );
    assert_eq!(schema.type_detail("String").unwrap().kind, TypeKind::Scalar);
    assert!(schema.type_detail("Nope").is_none());
}

#[test]
fn single_fields_include_meta_fields() {
    let schema = schema();
    let user = schema.field("Query", "user").unwrap();
    assert_eq!(user.signature(), "user(id: ID!): User");
    assert_eq!(user.description.as_deref(), Some("Find a user by id."));
    assert_eq!(
        schema.field("User", "__typename").unwrap().signature(),
        "__typename: String!"
    );
    assert!(schema.field("User", "nope").is_none());
}
