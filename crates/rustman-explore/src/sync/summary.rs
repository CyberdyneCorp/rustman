//! The change summary every sync prints, or emits as JSON with `--json`.

use std::fmt;

use serde::Serialize;

/// An operation added, removed or unchanged in shape.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OperationFile {
    /// The `operationId`, or `METHOD /path` when it has none.
    pub operation_id: String,
    /// `/`-separated, relative to the collection folder.
    pub file: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ChangedOperation {
    pub operation_id: String,
    pub file: String,
    /// The fields that took the new document's value.
    pub fields: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Conflict {
    pub operation_id: String,
    pub file: String,
    pub field: String,
    /// Always `kept local`: the user's value wins.
    pub resolution: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EnvironmentChange {
    pub name: String,
    /// `created` or `updated`.
    pub change: String,
}

/// What a sync did (or, with `--dry-run`, would do).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SyncSummary {
    pub added: Vec<OperationFile>,
    /// Each moved to `_removed/`; `file` is where it went.
    pub removed: Vec<OperationFile>,
    pub changed: Vec<ChangedOperation>,
    pub conflicts: Vec<Conflict>,
    pub unchanged: usize,
    pub environments: Vec<EnvironmentChange>,
    pub dry_run: bool,
}

impl fmt::Display for SyncSummary {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for op in &self.added {
            writeln!(f, "added: {} ({})", op.operation_id, op.file)?;
        }
        for op in &self.changed {
            let fields = op.fields.join(", ");
            writeln!(f, "changed: {} ({fields})", op.operation_id)?;
        }
        for c in &self.conflicts {
            writeln!(
                f,
                "conflict: {} {}: {}",
                c.operation_id, c.field, c.resolution
            )?;
        }
        for op in &self.removed {
            writeln!(f, "removed: {} (moved to {})", op.operation_id, op.file)?;
        }
        for env in &self.environments {
            writeln!(f, "environment {}: {}", env.name, env.change)?;
        }
        let plural = |n: usize, word: &str| {
            let s = if n == 1 { "" } else { "s" };
            format!("{n} {word}{s}")
        };
        write!(
            f,
            "{} added, {} changed, {} removed, {}, {} unchanged",
            self.added.len(),
            self.changed.len(),
            self.removed.len(),
            plural(self.conflicts.len(), "conflict"),
            self.unchanged,
        )?;
        if self.dry_run {
            write!(f, " (dry run: nothing was written)")?;
        }
        writeln!(f)
    }
}
