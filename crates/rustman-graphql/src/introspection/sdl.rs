//! Introspection result to SDL text (design D2). The text is parsed by
//! `apollo-compiler` afterwards, which reports anything malformed; this
//! module only has to write every definition faithfully.
//!
//! Built-in scalars, introspection types (`__*`) and the built-in
//! directives are left out: every schema has them.

use std::fmt::Write as _;

use super::model::{
    DirectiveDef, EnumValue, Field, FullType, InputValue, IntrospectionSchema, TypeKind, TypeRef,
};
use crate::error::GraphqlError;

const BUILT_IN_SCALARS: [&str; 5] = ["String", "Int", "Float", "Boolean", "ID"];
const BUILT_IN_DIRECTIVES: [&str; 4] = ["skip", "include", "deprecated", "specifiedBy"];

/// Type references deeper than this are rejected (the standard query asks
/// for seven levels).
const MAX_TYPE_DEPTH: usize = 32;

/// The SDL text of `schema`.
pub fn to_sdl(schema: &IntrospectionSchema) -> Result<String, GraphqlError> {
    let mut out = String::new();
    write_schema_definition(&mut out, schema)?;
    for ty in schema.types.iter().filter(|t| !is_built_in_type(t)) {
        out.push('\n');
        write_type(&mut out, ty)?;
    }
    for directive in schema
        .directives
        .iter()
        .filter(|d| !is_built_in_directive(d))
    {
        out.push('\n');
        write_directive(&mut out, directive)?;
    }
    if needs_one_of_declaration(schema) {
        out.push_str("\ndirective @oneOf on INPUT_OBJECT\n");
    }
    Ok(out)
}

fn invalid(message: impl Into<String>) -> GraphqlError {
    GraphqlError::InvalidIntrospection(message.into())
}

fn is_built_in_type(ty: &FullType) -> bool {
    ty.name.starts_with("__")
        || (ty.kind == TypeKind::Scalar && BUILT_IN_SCALARS.contains(&ty.name.as_str()))
}

fn is_built_in_directive(directive: &DirectiveDef) -> bool {
    BUILT_IN_DIRECTIVES.contains(&directive.name.as_str())
}

/// An input object uses `@oneOf` but the server lists no such directive.
fn needs_one_of_declaration(schema: &IntrospectionSchema) -> bool {
    schema.types.iter().any(|t| t.is_one_of == Some(true))
        && !schema.directives.iter().any(|d| d.name == "oneOf")
}

fn write_schema_definition(
    out: &mut String,
    schema: &IntrospectionSchema,
) -> Result<(), GraphqlError> {
    let query = schema
        .query_type
        .as_ref()
        .ok_or_else(|| invalid("the schema has no query type"))?;
    write_description(out, "", schema.description.as_deref());
    out.push_str("schema {\n");
    let _ = writeln!(out, "  query: {}", query.name);
    if let Some(mutation) = &schema.mutation_type {
        let _ = writeln!(out, "  mutation: {}", mutation.name);
    }
    if let Some(subscription) = &schema.subscription_type {
        let _ = writeln!(out, "  subscription: {}", subscription.name);
    }
    out.push_str("}\n");
    Ok(())
}

fn write_type(out: &mut String, ty: &FullType) -> Result<(), GraphqlError> {
    write_description(out, "", ty.description.as_deref());
    match ty.kind {
        TypeKind::Scalar => write_scalar(out, ty),
        TypeKind::Object => write_fields_type(out, "type", ty)?,
        TypeKind::Interface => write_fields_type(out, "interface", ty)?,
        TypeKind::Union => write_union(out, ty)?,
        TypeKind::Enum => write_enum(out, ty),
        TypeKind::InputObject => write_input(out, ty)?,
        TypeKind::List | TypeKind::NonNull => {
            return Err(invalid(format!(
                "type '{}' has a wrapper kind in the type list",
                ty.name
            )));
        }
    }
    Ok(())
}

fn write_scalar(out: &mut String, ty: &FullType) {
    let _ = write!(out, "scalar {}", ty.name);
    if let Some(url) = &ty.specified_by_url {
        let _ = write!(out, " @specifiedBy(url: {})", string_literal(url));
    }
    out.push('\n');
}

fn write_fields_type(out: &mut String, keyword: &str, ty: &FullType) -> Result<(), GraphqlError> {
    let _ = write!(out, "{keyword} {}", ty.name);
    let interfaces = named_list(ty.interfaces.as_deref().unwrap_or_default())?;
    if !interfaces.is_empty() {
        let _ = write!(out, " implements {}", interfaces.join(" & "));
    }
    let fields = ty.fields.as_deref().unwrap_or_default();
    if fields.is_empty() {
        out.push('\n');
        return Ok(());
    }
    out.push_str(" {\n");
    for field in fields {
        write_field(out, field)?;
    }
    out.push_str("}\n");
    Ok(())
}

fn write_field(out: &mut String, field: &Field) -> Result<(), GraphqlError> {
    write_description(out, "  ", field.description.as_deref());
    let _ = write!(out, "  {}", field.name);
    write_arguments(out, &field.args)?;
    let _ = write!(out, ": {}", type_ref(&field.ty)?);
    write_deprecated(
        out,
        field.is_deprecated,
        field.deprecation_reason.as_deref(),
    );
    out.push('\n');
    Ok(())
}

fn write_arguments(out: &mut String, args: &[InputValue]) -> Result<(), GraphqlError> {
    if args.is_empty() {
        return Ok(());
    }
    let args: Vec<String> = args.iter().map(input_value).collect::<Result<_, _>>()?;
    let _ = write!(out, "({})", args.join(", "));
    Ok(())
}

/// One argument or input field on a single line, its description inline.
fn input_value(value: &InputValue) -> Result<String, GraphqlError> {
    let mut out = String::new();
    if let Some(description) = &value.description {
        let _ = write!(out, "{} ", string_literal(description));
    }
    let _ = write!(out, "{}: {}", value.name, type_ref(&value.ty)?);
    if let Some(default) = &value.default_value {
        let _ = write!(out, " = {default}");
    }
    write_deprecated(
        &mut out,
        value.is_deprecated,
        value.deprecation_reason.as_deref(),
    );
    Ok(out)
}

fn write_union(out: &mut String, ty: &FullType) -> Result<(), GraphqlError> {
    let members = named_list(ty.possible_types.as_deref().unwrap_or_default())?;
    let _ = write!(out, "union {}", ty.name);
    if !members.is_empty() {
        let _ = write!(out, " = {}", members.join(" | "));
    }
    out.push('\n');
    Ok(())
}

fn write_enum(out: &mut String, ty: &FullType) {
    let _ = writeln!(out, "enum {} {{", ty.name);
    for value in ty.enum_values.as_deref().unwrap_or_default() {
        write_enum_value(out, value);
    }
    out.push_str("}\n");
}

fn write_enum_value(out: &mut String, value: &EnumValue) {
    write_description(out, "  ", value.description.as_deref());
    let _ = write!(out, "  {}", value.name);
    write_deprecated(
        out,
        value.is_deprecated,
        value.deprecation_reason.as_deref(),
    );
    out.push('\n');
}

fn write_input(out: &mut String, ty: &FullType) -> Result<(), GraphqlError> {
    let _ = write!(out, "input {}", ty.name);
    if ty.is_one_of == Some(true) {
        out.push_str(" @oneOf");
    }
    out.push_str(" {\n");
    for field in ty.input_fields.as_deref().unwrap_or_default() {
        let _ = writeln!(out, "  {}", input_value(field)?);
    }
    out.push_str("}\n");
    Ok(())
}

fn write_directive(out: &mut String, directive: &DirectiveDef) -> Result<(), GraphqlError> {
    write_description(out, "", directive.description.as_deref());
    let _ = write!(out, "directive @{}", directive.name);
    write_arguments(out, &directive.args)?;
    if directive.is_repeatable {
        out.push_str(" repeatable");
    }
    if directive.locations.is_empty() {
        return Err(invalid(format!(
            "directive '@{}' has no locations",
            directive.name
        )));
    }
    let _ = writeln!(out, " on {}", directive.locations.join(" | "));
    Ok(())
}

fn write_description(out: &mut String, indent: &str, description: Option<&str>) {
    if let Some(text) = description {
        let _ = writeln!(out, "{indent}{}", string_literal(text));
    }
}

fn write_deprecated(out: &mut String, deprecated: bool, reason: Option<&str>) {
    match (deprecated, reason) {
        (false, _) => {}
        (true, Some(reason)) => {
            let _ = write!(out, " @deprecated(reason: {})", string_literal(reason));
        }
        (true, None) => out.push_str(" @deprecated"),
    }
}

/// A GraphQL string literal. JSON string escapes are a subset of GraphQL's.
fn string_literal(text: &str) -> String {
    serde_json::to_string(text).expect("strings serialize")
}

/// The names of `refs`, which must all be named types.
fn named_list(refs: &[TypeRef]) -> Result<Vec<&str>, GraphqlError> {
    refs.iter()
        .map(|r| {
            r.name
                .as_deref()
                .ok_or_else(|| invalid("a type reference has no name"))
        })
        .collect()
}

/// `[Name!]!` and the like.
fn type_ref(ty: &TypeRef) -> Result<String, GraphqlError> {
    type_ref_at(ty, 0)
}

fn type_ref_at(ty: &TypeRef, depth: usize) -> Result<String, GraphqlError> {
    if depth > MAX_TYPE_DEPTH {
        return Err(invalid("a type reference is nested too deeply"));
    }
    let inner = || {
        ty.of_type
            .as_deref()
            .ok_or_else(|| invalid("a list or non-null type has no ofType"))
    };
    match ty.kind {
        TypeKind::NonNull => Ok(format!("{}!", type_ref_at(inner()?, depth + 1)?)),
        TypeKind::List => Ok(format!("[{}]", type_ref_at(inner()?, depth + 1)?)),
        _ => ty
            .name
            .clone()
            .ok_or_else(|| invalid("a type reference has no name")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn schema(value: serde_json::Value) -> IntrospectionSchema {
        serde_json::from_value(value).unwrap()
    }

    fn named(kind: &str, name: &str) -> serde_json::Value {
        json!({"kind": kind, "name": name})
    }

    fn non_null(inner: serde_json::Value) -> serde_json::Value {
        json!({"kind": "NON_NULL", "name": null, "ofType": inner})
    }

    #[test]
    fn every_definition_kind_is_written() {
        let s = schema(json!({
            "queryType": {"name": "Root"},
            "mutationType": {"name": "Mut"},
            "types": [
                {"kind": "OBJECT", "name": "Root", "description": "The \"root\"\nline two",
                 "interfaces": [named("INTERFACE", "Node")],
                 "fields": [
                    {"name": "id", "args": [], "type": non_null(named("SCALAR", "ID"))},
                    {"name": "node", "args": [{"name": "id", "description": "Id", "type": non_null(named("SCALAR", "ID")), "defaultValue": null}],
                     "type": named("INTERFACE", "Node")},
                    {"name": "old", "args": [], "type": json!({"kind": "LIST", "ofType": non_null(named("SCALAR", "String"))}),
                     "isDeprecated": true, "deprecationReason": "Use node"}
                 ]},
                {"kind": "OBJECT", "name": "Mut", "fields": [{"name": "ping", "args": [{"name": "n", "type": named("SCALAR", "Int"), "defaultValue": "3"}], "type": named("SCALAR", "Int")}]},
                {"kind": "INTERFACE", "name": "Node", "fields": [{"name": "id", "args": [], "type": non_null(named("SCALAR", "ID"))}]},
                {"kind": "UNION", "name": "Any", "possibleTypes": [named("OBJECT", "Root"), named("OBJECT", "Mut")]},
                {"kind": "ENUM", "name": "Color", "enumValues": [{"name": "RED"}, {"name": "GREY", "isDeprecated": true, "deprecationReason": null}]},
                {"kind": "INPUT_OBJECT", "name": "Pick", "isOneOf": true, "inputFields": [{"name": "a", "type": named("SCALAR", "Int"), "defaultValue": null}]},
                {"kind": "SCALAR", "name": "Url", "specifiedByURL": "https://url.spec.whatwg.org/"},
                {"kind": "SCALAR", "name": "String"},
                {"kind": "OBJECT", "name": "__Type", "fields": []}
            ],
            "directives": [
                {"name": "auth", "locations": ["FIELD_DEFINITION", "OBJECT"], "args": [{"name": "role", "type": named("SCALAR", "String"), "defaultValue": "\"admin\""}], "isRepeatable": true},
                {"name": "skip", "locations": ["FIELD"], "args": []}
            ]
        }));
        let sdl = to_sdl(&s).unwrap();
        insta::assert_snapshot!(sdl);
        let parsed = apollo_compiler::Schema::parse_and_validate(sdl, "s.graphql");
        assert!(parsed.is_ok(), "{parsed:?}");
    }

    #[test]
    fn malformed_results_are_errors() {
        let no_query = schema(json!({"types": []}));
        assert_eq!(
            to_sdl(&no_query).unwrap_err().to_string(),
            "graphql: invalid introspection result: the schema has no query type"
        );
        let unnamed = schema(json!({
            "queryType": {"name": "Q"},
            "types": [{"kind": "OBJECT", "name": "Q", "fields": [{"name": "a", "args": [], "type": {"kind": "NON_NULL"}}]}]
        }));
        assert!(to_sdl(&unnamed).unwrap_err().to_string().contains("ofType"));
        let mut deep = named("SCALAR", "Int");
        for _ in 0..40 {
            deep = json!({"kind": "LIST", "ofType": deep});
        }
        let deep = schema(json!({
            "queryType": {"name": "Q"},
            "types": [{"kind": "OBJECT", "name": "Q", "fields": [{"name": "a", "args": [], "type": deep}]}]
        }));
        assert!(
            to_sdl(&deep)
                .unwrap_err()
                .to_string()
                .contains("too deeply")
        );
    }
}
