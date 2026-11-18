//! The `__schema` object of an introspection result. Fields that older
//! servers do not return default to empty or false.

use serde::Deserialize;

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IntrospectionSchema {
    #[serde(default)]
    pub description: Option<String>,
    pub query_type: Option<NamedRef>,
    #[serde(default)]
    pub mutation_type: Option<NamedRef>,
    #[serde(default)]
    pub subscription_type: Option<NamedRef>,
    pub types: Vec<FullType>,
    #[serde(default)]
    pub directives: Vec<DirectiveDef>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct NamedRef {
    pub name: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum TypeKind {
    Scalar,
    Object,
    Interface,
    Union,
    Enum,
    InputObject,
    List,
    NonNull,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FullType {
    pub kind: TypeKind,
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default, rename = "specifiedByURL")]
    pub specified_by_url: Option<String>,
    #[serde(default)]
    pub fields: Option<Vec<Field>>,
    #[serde(default)]
    pub input_fields: Option<Vec<InputValue>>,
    #[serde(default)]
    pub interfaces: Option<Vec<TypeRef>>,
    #[serde(default)]
    pub enum_values: Option<Vec<EnumValue>>,
    #[serde(default)]
    pub possible_types: Option<Vec<TypeRef>>,
    #[serde(default)]
    pub is_one_of: Option<bool>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Field {
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub args: Vec<InputValue>,
    #[serde(rename = "type")]
    pub ty: TypeRef,
    #[serde(default)]
    pub is_deprecated: bool,
    #[serde(default)]
    pub deprecation_reason: Option<String>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InputValue {
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(rename = "type")]
    pub ty: TypeRef,
    /// A GraphQL literal, as the server prints it.
    #[serde(default)]
    pub default_value: Option<String>,
    #[serde(default)]
    pub is_deprecated: bool,
    #[serde(default)]
    pub deprecation_reason: Option<String>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EnumValue {
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub is_deprecated: bool,
    #[serde(default)]
    pub deprecation_reason: Option<String>,
}

/// A type reference: a named type, or a list or non-null wrapper.
#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TypeRef {
    pub kind: TypeKind,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub of_type: Option<Box<TypeRef>>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DirectiveDef {
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub locations: Vec<String>,
    #[serde(default)]
    pub args: Vec<InputValue>,
    #[serde(default)]
    pub is_repeatable: bool,
}
