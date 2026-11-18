//! Pre-send validation: the document against the schema (every rule of the
//! specification), deprecation warnings and the variables map. Without a
//! schema only the syntax is checked.

use std::fmt;

use apollo_compiler::ExecutableDocument;
use apollo_compiler::ast;
use apollo_compiler::diagnostic::ToCliReport;
use apollo_compiler::parser::{SourceMap, SourceSpan};
use apollo_compiler::validation::{DiagnosticData, DiagnosticList};
use serde::Serialize;
use serde_json::{Map, Value};

use crate::deprecation::deprecations;
use crate::operation::DOCUMENT_PATH;
use crate::schema::GraphqlSchema;
use crate::variables::check_variables;

/// Whether a diagnostic blocks the send.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    Error,
    Warning,
}

/// A 1-based line and column (in characters).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct Position {
    pub line: usize,
    pub column: usize,
}

/// One validation problem.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Diagnostic {
    pub severity: Severity,
    pub message: String,
    /// Where in the document; `None` for problems of the variables map.
    pub position: Option<Position>,
}

impl fmt::Display for Diagnostic {
    /// `line:column: message`, or the message alone without a position.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.position {
            Some(at) => write!(f, "{}:{}: {}", at.line, at.column, self.message),
            None => f.write_str(&self.message),
        }
    }
}

/// The outcome of [`validate`].
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct Validation {
    /// Errors first, in document order, then warnings.
    pub diagnostics: Vec<Diagnostic>,
    /// Only the syntax was checked: no schema was available.
    pub syntax_only: bool,
}

impl Validation {
    pub fn errors(&self) -> impl Iterator<Item = &Diagnostic> {
        self.diagnostics
            .iter()
            .filter(|d| d.severity == Severity::Error)
    }

    pub fn warnings(&self) -> impl Iterator<Item = &Diagnostic> {
        self.diagnostics
            .iter()
            .filter(|d| d.severity == Severity::Warning)
    }

    /// Whether any error blocks the send.
    pub fn has_errors(&self) -> bool {
        self.errors().next().is_some()
    }
}

/// Validate `document` against `schema`, and `variables` against the
/// variable definitions of the operation `operation_name` selects. With no
/// schema, only the syntax is checked.
pub fn validate(
    document: &str,
    schema: Option<&GraphqlSchema>,
    operation_name: Option<&str>,
    variables: &Map<String, Value>,
) -> Validation {
    let Some(schema) = schema else {
        return Validation {
            diagnostics: syntax(document),
            syntax_only: true,
        };
    };
    let typed = schema.schema();
    let diagnostics = match ExecutableDocument::parse_and_validate(typed, document, DOCUMENT_PATH) {
        Err(invalid) => errors(&invalid.errors, document),
        Ok(valid) => {
            let mut found = variable_errors(typed, &valid, operation_name, variables);
            found.extend(deprecations(typed, &valid));
            found
        }
    };
    Validation {
        diagnostics,
        syntax_only: false,
    }
}

/// Syntax errors of `document`.
fn syntax(document: &str) -> Vec<Diagnostic> {
    match ast::Document::parse(document, DOCUMENT_PATH) {
        Ok(_) => Vec::new(),
        Err(invalid) => errors(&invalid.errors, document),
    }
}

fn variable_errors(
    schema: &apollo_compiler::Schema,
    document: &ExecutableDocument,
    operation_name: Option<&str>,
    variables: &Map<String, Value>,
) -> Vec<Diagnostic> {
    // Operation selection reports its own errors before send.
    let Ok(operation) = document.operations.get(operation_name) else {
        return Vec::new();
    };
    check_variables(schema, &operation.variables, variables)
        .into_iter()
        .map(|message| Diagnostic {
            severity: Severity::Error,
            message,
            position: None,
        })
        .collect()
}

/// Each diagnostic of `list` as an error, with the message graphql-js
/// gives (`Cannot query field "x" on type "T".`) where one exists. A
/// "must have a selection of subfields" error on a field that has one is
/// left out: apollo-compiler drops unknown fields before that check, so it
/// only echoes the unknown field reported next to it.
fn errors(list: &DiagnosticList, document: &str) -> Vec<Diagnostic> {
    list.iter()
        .filter(|d| !echoes_unknown_field(d.error, document))
        .map(|d| {
            let json = d.unstable_to_json_compat();
            let position = json.locations.first().map(|at| Position {
                line: at.line,
                column: at.column,
            });
            Diagnostic {
                severity: Severity::Error,
                message: json.message,
                position,
            }
        })
        .collect()
}

/// A missing-subselection error on a field written with a selection set:
/// the field's text ends with `}` (arguments and directives end with `)`
/// or a name).
fn echoes_unknown_field(error: &DiagnosticData, document: &str) -> bool {
    if error.unstable_error_name() != Some("MissingSubselection") {
        return false;
    }
    ToCliReport::location(error)
        .and_then(|span| document.get(span.offset()..span.end_offset()))
        .is_some_and(|field| field.trim_end().ends_with('}'))
}

/// The start of `span` in `sources`.
pub(crate) fn position(span: SourceSpan, sources: &SourceMap) -> Option<Position> {
    span.line_column(sources).map(|at| Position {
        line: at.line,
        column: at.column,
    })
}
