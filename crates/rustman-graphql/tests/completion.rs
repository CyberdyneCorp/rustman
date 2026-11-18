//! Completion over half-typed documents (add-graphql-client task 3.6).
//! `|` marks the cursor in every case.

use rustman_graphql::{CompletionKind, GraphqlSchema, complete};

const LIBRARY: &str = include_str!("fixtures/library.graphql");

const USER_FIELDS: &[&str] = &[
    "id",
    "name",
    "email",
    "username",
    "handle",
    "role",
    "createdAt",
    "books",
    "__typename",
];

/// The labels offered at the `|` of `marked`.
fn labels(marked: &str) -> Vec<String> {
    let schema = GraphqlSchema::from_sdl(LIBRARY, "library.graphql").unwrap();
    let cursor = marked.find('|').expect("a cursor marker");
    let document = marked.replacen('|', "", 1);
    complete(&schema, &document, cursor)
        .into_iter()
        .map(|c| c.label)
        .collect()
}

#[test]
fn completion_cases() {
    let cases: &[(&str, &[&str])] = &[
        // Fields of the enclosing type.
        ("{ user(id: 1) { | } }", USER_FIELDS),
        ("{ user(id: 1) { na| } }", &["name"]),
        ("{ user(id: 1) { name e| } }", &["email"]),
        ("{ |", &["user", "users", "search", "node", "__typename"]),
        (
            "mutation { |",
            &["login", "upload", "createUser", "__typename"],
        ),
        ("subscription S { |", &["onMessage", "__typename"]),
        ("{ me: user(id: 1) { books { t| } } }", &["title"]),
        ("{ node(id: 1) { ... on Book { a| } } }", &["author"]),
        (
            "fragment F on Book { |",
            &["id", "title", "author", "__typename"],
        ),
        ("{ search(text: \"x\") { |", &["__typename"]),
        // Arguments of the enclosing field or directive.
        ("{ user(|) }", &["id"]),
        ("{ users(role: ADMIN, |) }", &["role", "first"]),
        ("{ user(id: 1) { books(f|) } }", &["first"]),
        ("{ user(id: 1) @cached(|) { id } }", &["ttl"]),
        // Enum values in enum-typed positions.
        ("{ users(role: |) }", &["ADMIN", "MEMBER", "GUEST"]),
        ("{ users(role: M|) }", &["MEMBER"]),
        ("{ users(first: |) }", &[]),
        // Variables declared by the operation.
        (
            "query($id: ID!, $first: Int = 3) { user(id: $|) }",
            &["id", "first"],
        ),
        (
            "query($id: ID!, $first: Int) { users(first: $f|) }",
            &["first"],
        ),
        ("query A($id: ID!) { a } query B { user(id: $|) }", &[]),
        // Directive names.
        (
            "{ user(id: 1) @|",
            &["cached", "skip", "include", "deprecated", "specifiedBy"],
        ),
        ("{ user(id: 1) @inc|", &["include"]),
        // Fragment names, defined anywhere in the document.
        (
            "{ user(id: 1) { ...| } } fragment UserBits on User { id }",
            &["UserBits"],
        ),
        // Nothing outside a selection set or argument list.
        ("|", &[]),
        ("query Q|", &[]),
    ];
    for (marked, expected) in cases {
        let mut got = labels(marked);
        let mut want: Vec<String> = expected.iter().map(|s| s.to_string()).collect();
        if marked.contains('@') {
            got.sort();
            want.sort();
        }
        assert_eq!(got, want, "{marked}");
    }
}

#[test]
fn candidates_carry_their_type_signature() {
    let schema = GraphqlSchema::from_sdl(LIBRARY, "library.graphql").unwrap();
    let document = "query($id: ID!) { users(role: ADMIN) { books }  user(id: $id) @cached }";
    let at = |needle: &str| document.find(needle).unwrap() + needle.len();
    let books = complete(&schema, document, at("{ books"));
    assert_eq!(books[0].kind, CompletionKind::Field);
    assert_eq!(
        books[0].detail,
        "books(first: Int = 5, after: String): [Book!]!"
    );
    let role = complete(&schema, document, at("role: ADMIN"));
    assert_eq!(
        (role[0].kind, role[0].detail.as_str()),
        (CompletionKind::EnumValue, "Role")
    );
    let variable = complete(&schema, document, at("(id: $id"));
    assert_eq!(
        (variable[0].kind, variable[0].detail.as_str()),
        (CompletionKind::Variable, "ID!")
    );
    let directive = complete(&schema, document, at("@cached"));
    assert_eq!(
        (directive[0].kind, directive[0].detail.as_str()),
        (CompletionKind::Directive, "@cached(ttl: Int = 60)")
    );
    let argument = complete(&schema, "{ user(", 7);
    assert_eq!(
        (argument[0].kind, argument[0].detail.as_str()),
        (CompletionKind::Argument, "id: ID!")
    );
    let fragment = complete(&schema, "{ ... } fragment F on User { id }", 5);
    assert_eq!(
        (fragment[0].kind, fragment[0].detail.as_str()),
        (CompletionKind::Fragment, "on User")
    );
}

#[test]
fn a_cursor_past_the_end_is_clamped() {
    let schema = GraphqlSchema::from_sdl(LIBRARY, "library.graphql").unwrap();
    let got = complete(&schema, "{ user(id: 1) { na", 1_000);
    assert_eq!(got.len(), 1);
    assert_eq!(got[0].label, "name");
}
