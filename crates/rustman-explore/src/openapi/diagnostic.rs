//! Validation diagnostics: a severity, a JSON pointer and a message.

use std::fmt;

use super::pointer;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Severity {
    /// A structural problem: the import stops.
    Error,
    /// A non-fatal issue: the import continues.
    Warning,
}

/// One problem found in a document, located by a JSON pointer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Diagnostic {
    pub severity: Severity,
    /// The JSON pointer of the offending value; empty for the document root.
    pub pointer: String,
    pub message: String,
}

impl Diagnostic {
    pub fn error(pointer: impl Into<String>, message: impl Into<String>) -> Self {
        Self::new(Severity::Error, pointer, message)
    }

    pub fn warning(pointer: impl Into<String>, message: impl Into<String>) -> Self {
        Self::new(Severity::Warning, pointer, message)
    }

    fn new(severity: Severity, pointer: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            severity,
            pointer: pointer.into(),
            message: message.into(),
        }
    }

    pub fn is_error(&self) -> bool {
        self.severity == Severity::Error
    }
}

/// `<pointer>: <message>`, without the severity (front-ends add their prefix).
impl fmt::Display for Diagnostic {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", pointer::display(&self.pointer), self.message)
    }
}

/// Every diagnostic on its own line, prefixed with `error:` or `warning:`,
/// errors first.
pub fn render(diagnostics: &[Diagnostic]) -> String {
    let errors = diagnostics.iter().filter(|d| d.is_error());
    let warnings = diagnostics.iter().filter(|d| !d.is_error());
    let mut out = String::new();
    for d in errors {
        out.push_str(&format!("error: {d}\n"));
    }
    for d in warnings {
        out.push_str(&format!("warning: {d}\n"));
    }
    out
}
