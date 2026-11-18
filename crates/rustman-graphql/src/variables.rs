//! Checking a request's variables map against the variable definitions of
//! its operation. Messages name the variable path and the expected type
//! but never the value, which may be a secret.

use apollo_compiler::ast::{Type, VariableDefinition};
use apollo_compiler::schema::{ExtendedType, InputObjectType};
use apollo_compiler::{Node, Schema};
use serde_json::{Map, Value};

/// A message for each variable of `values` that does not fit
/// `definitions`: missing required ones, nulls for non-null types and
/// values of the wrong shape.
pub(crate) fn check_variables(
    schema: &Schema,
    definitions: &[Node<VariableDefinition>],
    values: &Map<String, Value>,
) -> Vec<String> {
    let mut checker = Checker {
        schema,
        problems: Vec::new(),
    };
    for definition in definitions {
        let path = format!("${}", definition.name);
        match values.get(definition.name.as_str()) {
            Some(value) => checker.value(&path, &definition.ty, value),
            None if definition.ty.is_non_null() && definition.default_value.is_none() => {
                checker.required(&path, &definition.ty);
            }
            None => {}
        }
    }
    checker.problems
}

struct Checker<'a> {
    schema: &'a Schema,
    problems: Vec<String>,
}

impl Checker<'_> {
    fn required(&mut self, path: &str, ty: &Type) {
        self.problems
            .push(format!("variable {path} of type {ty} is required"));
    }

    fn value(&mut self, path: &str, ty: &Type, value: &Value) {
        if value.is_null() {
            if ty.is_non_null() {
                self.problems
                    .push(format!("variable {path} of type {ty} must not be null"));
            }
            return;
        }
        if !ty.is_list() {
            return self.named(path, ty.inner_named_type(), value);
        }
        match value {
            Value::Array(items) => {
                for (index, item) in items.iter().enumerate() {
                    self.value(&format!("{path}[{index}]"), ty.item_type(), item);
                }
            }
            // A single value is coerced to a list of one.
            other => self.value(path, ty.item_type(), other),
        }
    }

    fn named(&mut self, path: &str, name: &str, value: &Value) {
        let fits = match self.schema.types.get(name) {
            Some(ExtendedType::Scalar(_)) => scalar_fits(name, value),
            Some(ExtendedType::Enum(enum_type)) => value
                .as_str()
                .is_some_and(|v| enum_type.values.contains_key(v)),
            Some(ExtendedType::InputObject(input)) => {
                return self.input_object(path, input, value);
            }
            _ => true,
        };
        if !fits {
            let got = json_kind(value);
            self.problems
                .push(format!("variable {path}: expected {name}, got {got}"));
        }
    }

    fn input_object(&mut self, path: &str, input: &InputObjectType, value: &Value) {
        let Value::Object(fields) = value else {
            let got = json_kind(value);
            self.problems.push(format!(
                "variable {path}: expected {}, got {got}",
                input.name
            ));
            return;
        };
        for key in fields.keys() {
            if !input.fields.contains_key(key.as_str()) {
                self.problems.push(format!(
                    "variable {path}.{key}: field is not defined on {}",
                    input.name
                ));
            }
        }
        for (name, definition) in &input.fields {
            let field_path = format!("{path}.{name}");
            match fields.get(name.as_str()) {
                Some(field) => self.value(&field_path, &definition.ty, field),
                None if definition.is_required() => self.required(&field_path, &definition.ty),
                None => {}
            }
        }
    }
}

/// Whether `value` is a valid input for the scalar `name`. Custom scalars
/// accept anything: only the server knows their rules.
fn scalar_fits(name: &str, value: &Value) -> bool {
    match name {
        "Int" => is_int(value),
        "Float" => value.is_number(),
        "String" => value.is_string(),
        "Boolean" => value.is_boolean(),
        "ID" => value.is_string() || value.is_i64() || value.is_u64(),
        _ => true,
    }
}

/// A 32-bit integer, written as an integer or as an integral float (`2.0`),
/// which graphql-js also accepts as an `Int`.
fn is_int(value: &Value) -> bool {
    if let Some(n) = value.as_i64() {
        return i32::try_from(n).is_ok();
    }
    value
        .as_f64()
        .is_some_and(|f| f.fract() == 0.0 && f >= f64::from(i32::MIN) && f <= f64::from(i32::MAX))
}

fn json_kind(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "boolean",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "list",
        Value::Object(_) => "object",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use apollo_compiler::ExecutableDocument;
    use serde_json::json;

    const SDL: &str = "
        type Query { user(id: ID!): String, search(input: Search!, roles: [Role!]): String }
        enum Role { ADMIN USER }
        input Search { text: String!, limit: Int = 10, page: Page }
        input Page { size: Int! }
        scalar Upload
        type Mutation { upload(file: Upload!, n: Float, ok: Boolean): String }
    ";

    fn problems(document: &str, values: Value) -> Vec<String> {
        let schema = Schema::parse_and_validate(SDL, "s.graphql").unwrap();
        let doc = ExecutableDocument::parse_and_validate(&schema, document, "d.graphql").unwrap();
        let operation = doc.operations.iter().next().unwrap();
        let Value::Object(values) = values else {
            panic!("an object")
        };
        check_variables(&schema, &operation.variables, &values)
    }

    #[test]
    fn a_missing_required_variable_is_named() {
        let p = problems("query($id: ID!) { user(id: $id) }", json!({}));
        assert_eq!(p, ["variable $id of type ID! is required"]);
    }

    #[test]
    fn defaults_and_nullable_variables_may_be_missing() {
        let doc = "query($id: ID! = 1, $r: [Role!]) { user(id: $id) search(input: {text: \"\"}, roles: $r) }";
        assert!(problems(doc, json!({})).is_empty());
    }

    #[test]
    fn values_of_the_wrong_shape_are_reported_by_path() {
        let doc = "query($s: Search!, $r: [Role!]) { search(input: $s, roles: $r) }";
        let p = problems(
            doc,
            json!({"s": {"text": 3, "page": {}, "extra": 1}, "r": ["ADMIN", "NOPE", null]}),
        );
        assert_eq!(
            p,
            [
                "variable $s.extra: field is not defined on Search",
                "variable $s.text: expected String, got number",
                "variable $s.page.size of type Int! is required",
                "variable $r[1]: expected Role, got string",
                "variable $r[2] of type Role! must not be null",
            ]
        );
    }

    #[test]
    fn valid_values_pass() {
        let doc = "query($s: Search!, $r: [Role!], $id: ID!) { search(input: $s, roles: $r) user(id: $id) }";
        let values =
            json!({"s": {"text": "a", "limit": null, "page": {"size": 2}}, "r": "USER", "id": 7});
        assert!(problems(doc, values).is_empty());
    }

    #[test]
    fn scalars_are_checked_and_custom_scalars_accept_anything() {
        let doc =
            "mutation($f: Upload!, $n: Float, $ok: Boolean) { upload(file: $f, n: $n, ok: $ok) }";
        assert!(problems(doc, json!({"f": {"$file": "a.png"}, "n": 1, "ok": true})).is_empty());
        let p = problems(doc, json!({"f": null, "n": "1", "ok": 1}));
        assert_eq!(
            p,
            [
                "variable $f of type Upload! must not be null",
                "variable $n: expected Float, got string",
                "variable $ok: expected Boolean, got number",
            ]
        );
    }

    #[test]
    fn int_values_must_fit_32_bits_and_never_echo_the_value() {
        let schema = "query($id: ID!, $s: Search!) { user(id: $id) search(input: $s) }";
        let p = problems(
            schema,
            json!({"id": true, "s": {"text": "secret-value", "limit": 3_000_000_000_i64}}),
        );
        assert_eq!(
            p,
            [
                "variable $id: expected ID, got boolean",
                "variable $s.limit: expected Int, got number",
            ]
        );
        assert!(!p.join(" ").contains("secret-value"));
    }

    #[test]
    fn integral_floats_are_ints_and_fractions_are_not() {
        let doc = "query($s: Search!) { search(input: $s) }";
        let page = |size: Value| json!({"s": {"text": "t", "limit": 2.0, "page": {"size": size}}});
        assert!(problems(doc, page(json!(2.0))).is_empty());
        assert!(problems(doc, page(json!(-2_147_483_648.0))).is_empty());
        for bad in [json!(2.5), json!(2_147_483_648.0)] {
            assert_eq!(
                problems(doc, page(bad)),
                ["variable $s.page.size: expected Int, got number"]
            );
        }
    }
}
