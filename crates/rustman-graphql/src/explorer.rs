//! Schema explorer data for the TUI explorer and the agent interface: our
//! own serializable types, so front-ends never touch `apollo-compiler`.

use apollo_compiler::Node;
use apollo_compiler::ast::{FieldDefinition, InputValueDefinition, OperationType};
use apollo_compiler::collections::IndexMap;
use apollo_compiler::schema::{Component, DirectiveDefinition, ExtendedType};
use apollo_compiler::{Name, Schema};
use serde::Serialize;

use crate::deprecation::deprecation_reason;
use crate::schema::GraphqlSchema;

/// The kind of a named type.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum TypeKind {
    Scalar,
    Object,
    Interface,
    Union,
    Enum,
    InputObject,
}

impl TypeKind {
    fn of(ty: &ExtendedType) -> Self {
        match ty {
            ExtendedType::Scalar(_) => Self::Scalar,
            ExtendedType::Object(_) => Self::Object,
            ExtendedType::Interface(_) => Self::Interface,
            ExtendedType::Union(_) => Self::Union,
            ExtendedType::Enum(_) => Self::Enum,
            ExtendedType::InputObject(_) => Self::InputObject,
        }
    }
}

/// The root types and every type and directive the schema defines.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct SchemaOverview {
    pub query: Option<String>,
    pub mutation: Option<String>,
    pub subscription: Option<String>,
    /// Named types, built-in scalars and introspection types excluded.
    pub types: Vec<TypeSummary>,
    pub directives: Vec<DirectiveInfo>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct TypeSummary {
    pub name: String,
    pub kind: TypeKind,
    pub description: Option<String>,
}

/// Everything about one named type.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct TypeDetail {
    pub name: String,
    pub kind: TypeKind,
    pub description: Option<String>,
    /// Object and interface fields.
    pub fields: Vec<FieldInfo>,
    pub input_fields: Vec<InputValueInfo>,
    pub enum_values: Vec<EnumValueInfo>,
    /// Interfaces this type implements.
    pub interfaces: Vec<String>,
    /// Union members, or the types implementing an interface.
    pub possible_types: Vec<String>,
    /// The `@specifiedBy` URL of a custom scalar.
    pub specified_by: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct FieldInfo {
    pub name: String,
    pub description: Option<String>,
    /// The type reference, such as `[User!]!`.
    pub ty: String,
    pub arguments: Vec<InputValueInfo>,
    /// The deprecation reason, when deprecated.
    pub deprecated: Option<String>,
}

impl FieldInfo {
    /// `name(arg: Type = default, ...): Type`.
    pub fn signature(&self) -> String {
        if self.arguments.is_empty() {
            return format!("{}: {}", self.name, self.ty);
        }
        let args: Vec<String> = self.arguments.iter().map(|a| a.signature()).collect();
        format!("{}({}): {}", self.name, args.join(", "), self.ty)
    }
}

/// An argument or input field.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct InputValueInfo {
    pub name: String,
    pub description: Option<String>,
    pub ty: String,
    /// The default value, as GraphQL source.
    pub default: Option<String>,
    pub deprecated: Option<String>,
}

impl InputValueInfo {
    /// `name: Type` or `name: Type = default`.
    pub fn signature(&self) -> String {
        match &self.default {
            Some(default) => format!("{}: {} = {default}", self.name, self.ty),
            None => format!("{}: {}", self.name, self.ty),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct EnumValueInfo {
    pub name: String,
    pub description: Option<String>,
    pub deprecated: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct DirectiveInfo {
    pub name: String,
    pub description: Option<String>,
    pub arguments: Vec<InputValueInfo>,
    pub locations: Vec<String>,
    pub repeatable: bool,
}

impl GraphqlSchema {
    /// The root types, the named types and the directives.
    pub fn overview(&self) -> SchemaOverview {
        let schema: &Schema = self.schema();
        let root = |op| schema.root_operation(op).map(|n| n.to_string());
        SchemaOverview {
            query: root(OperationType::Query),
            mutation: root(OperationType::Mutation),
            subscription: root(OperationType::Subscription),
            types: schema
                .types
                .values()
                .filter(|t| !t.is_built_in())
                .map(|t| TypeSummary {
                    name: t.name().to_string(),
                    kind: TypeKind::of(t),
                    description: text(t.description()),
                })
                .collect(),
            directives: schema
                .directive_definitions
                .values()
                .map(|d| directive(d))
                .collect(),
        }
    }

    /// Everything about the type `name`, built-in types included.
    pub fn type_detail(&self, name: &str) -> Option<TypeDetail> {
        let schema: &Schema = self.schema();
        let ty = schema.types.get(name)?;
        let mut detail = TypeDetail {
            name: ty.name().to_string(),
            kind: TypeKind::of(ty),
            description: text(ty.description()),
            fields: field_definitions(ty).map(fields).unwrap_or_default(),
            input_fields: Vec::new(),
            enum_values: Vec::new(),
            interfaces: Vec::new(),
            possible_types: Vec::new(),
            specified_by: None,
        };
        fill_kind_specific(schema, ty, &mut detail);
        Some(detail)
    }

    /// The field `field` of the type `type_name`, meta-fields included.
    pub fn field(&self, type_name: &str, field: &str) -> Option<FieldInfo> {
        let definition = self.schema().type_field(type_name, field).ok()?;
        Some(field_info(definition))
    }
}

fn fill_kind_specific(schema: &Schema, ty: &ExtendedType, detail: &mut TypeDetail) {
    match ty {
        ExtendedType::Object(object) => detail.interfaces = names(&object.implements_interfaces),
        ExtendedType::Interface(interface) => {
            detail.interfaces = names(&interface.implements_interfaces);
            detail.possible_types = implementers(schema, &interface.name);
        }
        ExtendedType::Union(union) => detail.possible_types = names(&union.members),
        ExtendedType::Enum(enum_type) => {
            detail.enum_values = enum_type
                .values
                .values()
                .map(|v| EnumValueInfo {
                    name: v.value.to_string(),
                    description: text(v.description.as_ref()),
                    deprecated: deprecation_reason(&v.directives),
                })
                .collect();
        }
        ExtendedType::InputObject(input) => {
            detail.input_fields = input.fields.values().map(|f| input_value(f)).collect();
        }
        ExtendedType::Scalar(scalar) => {
            detail.specified_by = scalar
                .directives
                .get("specifiedBy")
                .and_then(|d| d.specified_argument_by_name("url"))
                .and_then(|v| v.as_str())
                .map(str::to_owned);
        }
    }
}

/// The explicit fields of an object or interface type.
pub(crate) fn field_definitions(
    ty: &ExtendedType,
) -> Option<&IndexMap<Name, Component<FieldDefinition>>> {
    match ty {
        ExtendedType::Object(object) => Some(&object.fields),
        ExtendedType::Interface(interface) => Some(&interface.fields),
        _ => None,
    }
}

fn fields(map: &IndexMap<Name, Component<FieldDefinition>>) -> Vec<FieldInfo> {
    map.values().map(|f| field_info(f)).collect()
}

pub(crate) fn field_info(field: &FieldDefinition) -> FieldInfo {
    FieldInfo {
        name: field.name.to_string(),
        description: text(field.description.as_ref()),
        ty: field.ty.to_string(),
        arguments: field.arguments.iter().map(input_value).collect(),
        deprecated: deprecation_reason(&field.directives),
    }
}

pub(crate) fn input_value(value: &Node<InputValueDefinition>) -> InputValueInfo {
    InputValueInfo {
        name: value.name.to_string(),
        description: text(value.description.as_ref()),
        ty: value.ty.to_string(),
        default: value.default_value.as_ref().map(|v| v.to_string()),
        deprecated: deprecation_reason(&value.directives),
    }
}

pub(crate) fn directive(definition: &DirectiveDefinition) -> DirectiveInfo {
    DirectiveInfo {
        name: definition.name.to_string(),
        description: text(definition.description.as_ref()),
        arguments: definition.arguments.iter().map(input_value).collect(),
        locations: definition
            .locations
            .iter()
            .map(|l| l.name().to_owned())
            .collect(),
        repeatable: definition.repeatable,
    }
}

fn implementers(schema: &Schema, interface: &str) -> Vec<String> {
    let map = schema.implementers_map();
    let Some(found) = map.get(interface) else {
        return Vec::new();
    };
    found
        .objects
        .iter()
        .chain(found.interfaces.iter())
        .map(|n| n.to_string())
        .collect()
}

fn names<'a, T: std::fmt::Display + 'a>(set: impl IntoIterator<Item = &'a T>) -> Vec<String> {
    set.into_iter().map(|n| n.to_string()).collect()
}

fn text(description: Option<&Node<str>>) -> Option<String> {
    description.map(|d| d.to_string())
}
