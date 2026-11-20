//! Deprecation warnings: fields and enum values a document uses that the
//! schema marks `@deprecated`. They never block a send.

use apollo_compiler::ast::{DirectiveList, Type, Value};
use apollo_compiler::executable::{Field, Selection, SelectionSet};
use apollo_compiler::parser::SourceMap;
use apollo_compiler::schema::ExtendedType;
use apollo_compiler::{ExecutableDocument, Node, Schema};

use crate::validate::{Diagnostic, Severity, position};

/// The reason the specification gives a `@deprecated` without one.
pub const DEFAULT_REASON: &str = "No longer supported";

/// The `reason` of a `@deprecated` directive in `directives`, if any.
pub fn deprecation_reason(directives: &DirectiveList) -> Option<String> {
    let directive = directives.get("deprecated")?;
    let reason = directive
        .specified_argument_by_name("reason")
        .and_then(|v| v.as_str())
        .unwrap_or(DEFAULT_REASON);
    Some(reason.to_owned())
}

/// A warning for each deprecated field or enum value `document` uses.
pub(crate) fn deprecations(schema: &Schema, document: &ExecutableDocument) -> Vec<Diagnostic> {
    let mut walker = Walker {
        schema,
        sources: &document.sources,
        found: Vec::new(),
    };
    for operation in document.operations.iter() {
        for variable in &operation.variables {
            if let Some(default) = &variable.default_value {
                walker.value(&variable.ty, default);
            }
        }
        walker.selection_set(&operation.selection_set);
    }
    for fragment in document.fragments.values() {
        walker.selection_set(&fragment.selection_set);
    }
    walker.found
}

struct Walker<'a> {
    schema: &'a Schema,
    sources: &'a SourceMap,
    found: Vec<Diagnostic>,
}

impl Walker<'_> {
    fn selection_set(&mut self, set: &SelectionSet) {
        for selection in &set.selections {
            match selection {
                Selection::Field(field) => self.field(&set.ty, field),
                Selection::InlineFragment(inline) => self.selection_set(&inline.selection_set),
                Selection::FragmentSpread(_) => {}
            }
        }
    }

    fn field(&mut self, parent: &str, field: &Node<Field>) {
        if let Some(reason) = deprecation_reason(&field.definition.directives) {
            let subject = format!("{parent}.{}", field.name);
            self.warn(field.location(), &subject, &reason);
        }
        for argument in &field.arguments {
            if let Some(definition) = field.definition.argument_by_name(&argument.name) {
                self.value(&definition.ty, &argument.value);
            }
        }
        self.selection_set(&field.selection_set);
    }

    /// Enum values inside `value`, an input of type `ty`.
    fn value(&mut self, ty: &Type, value: &Node<Value>) {
        let named = ty.inner_named_type();
        match (value.as_ref(), self.schema.types.get(named)) {
            (Value::List(items), _) => {
                for item in items {
                    self.value(ty.item_type(), item);
                }
            }
            (Value::Enum(name), Some(ExtendedType::Enum(enum_type))) => {
                let reason = enum_type
                    .values
                    .get(name)
                    .and_then(|v| deprecation_reason(&v.directives));
                if let Some(reason) = reason {
                    self.warn(value.location(), &format!("{named}.{name}"), &reason);
                }
            }
            (Value::Object(fields), Some(ExtendedType::InputObject(input))) => {
                for (name, field_value) in fields {
                    if let Some(definition) = input.fields.get(name) {
                        self.value(&definition.ty, field_value);
                    }
                }
            }
            _ => {}
        }
    }

    fn warn(
        &mut self,
        location: Option<apollo_compiler::parser::SourceSpan>,
        subject: &str,
        reason: &str,
    ) {
        self.found.push(Diagnostic {
            severity: Severity::Warning,
            message: format!("{subject} is deprecated: {reason}"),
            position: location.and_then(|l| position(l, self.sources)),
        });
    }
}
