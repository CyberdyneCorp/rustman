//! The import report (task 1.6): a human summary and the
//! `rustman.import-report/v1` JSON. Secrets appear by name only.

use serde::Serialize;

use crate::ir::{Location, Note, NoteKind};

pub const SCHEMA: &str = "rustman.import-report/v1";

#[derive(Clone, Debug, Serialize)]
pub struct Report {
    pub schema: &'static str,
    pub source: SourceInfo,
    /// The collection folder, relative to the workspace root (none for an
    /// environment-only import).
    pub destination: Option<String>,
    pub dry_run: bool,
    pub summary: Summary,
    pub files: Vec<FileEntry>,
    pub conflicts: Vec<Conflict>,
    pub secrets: Vec<SecretEntry>,
    pub notes: Vec<Note>,
    /// Script statements found, converted and left as comments.
    pub migration: Migration,
}

/// The script migration totals and, per request, the statements left
/// unconverted (with extracted secret values redacted).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct Migration {
    pub found: usize,
    pub mapped: usize,
    pub unconverted: usize,
    /// `mapped * 100 / found`, rounded down; 100 when nothing was found.
    pub percent: usize,
    pub requests: Vec<RequestMigration>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct RequestMigration {
    /// The request's item path in the source, such as `Users / List`.
    pub request: String,
    /// The request file, relative to the workspace root.
    pub path: String,
    pub found: usize,
    pub mapped: usize,
    pub unconverted: usize,
    /// The unconverted statements, as written.
    pub statements: Vec<String>,
}

impl Migration {
    /// Totals over `requests`.
    pub fn new(requests: Vec<RequestMigration>) -> Self {
        let found = requests.iter().map(|r| r.found).sum();
        let mapped = requests.iter().map(|r| r.mapped).sum();
        Self {
            found,
            mapped,
            unconverted: found - mapped,
            percent: (mapped * 100).checked_div(found).unwrap_or(100),
            requests,
        }
    }

    /// `scripts: 31/40 statements converted (77%)`.
    pub fn summary_line(&self) -> String {
        format!(
            "scripts: {}/{} statements converted ({}%)\n",
            self.mapped, self.found, self.percent
        )
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct SourceInfo {
    pub path: String,
    pub format: String,
    pub name: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct Summary {
    /// Request files written (created, replaced or renamed).
    pub requests: usize,
    /// `_collection.yaml` files written.
    pub collections: usize,
    /// Environment files created or updated.
    pub environments: usize,
    /// Files kept because they already existed (including unchanged ones).
    pub skipped: usize,
    pub dropped: usize,
    pub secrets_moved: usize,
    pub unsupported: usize,
    pub unmapped: usize,
    /// Scripts preserved as comments, not executed.
    pub scripts: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FileKind {
    Request,
    Collection,
    Environment,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Action {
    Create,
    Overwrite,
    /// An existing environment file that gains variables or secrets.
    Update,
    Skip,
    /// The file exists with exactly this content: nothing to write and no
    /// conflict, whatever the policy.
    Unchanged,
}

impl Action {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Create => "create",
            Self::Overwrite => "overwrite",
            Self::Update => "update",
            Self::Skip => "skip",
            Self::Unchanged => "unchanged",
        }
    }

    pub fn writes(self) -> bool {
        !matches!(self, Self::Skip | Self::Unchanged)
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct FileEntry {
    pub path: String,
    pub kind: FileKind,
    pub action: Action,
}

/// An existing file or value met by the import, and what was done.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Conflict {
    pub path: String,
    /// `skipped (exists)`, `overwritten` or `renamed to <path>`.
    pub resolution: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct SecretEntry {
    pub name: String,
    pub environment: String,
    pub location: Location,
}

impl Report {
    /// Fill in the summary from the files, secrets and notes.
    pub fn count(&mut self) {
        let written = |kind| {
            self.files
                .iter()
                .filter(|f| f.kind == kind && f.action.writes())
                .count()
        };
        let notes = |kind| self.notes.iter().filter(|n| n.kind == kind).count();
        self.summary = Summary {
            requests: written(FileKind::Request),
            collections: written(FileKind::Collection),
            environments: written(FileKind::Environment),
            skipped: self.files.iter().filter(|f| !f.action.writes()).count(),
            dropped: notes(NoteKind::Dropped),
            secrets_moved: self.secrets.len(),
            unsupported: notes(NoteKind::Unsupported),
            unmapped: notes(NoteKind::Unmapped),
            scripts: notes(NoteKind::Script),
        };
    }

    /// Whether some feature was unsupported, dropped or unmapped.
    pub fn has_losses(&self) -> bool {
        self.notes.iter().any(|n| n.kind.is_loss())
    }

    pub fn to_json(&self) -> String {
        let mut text = serde_json::to_string_pretty(self).expect("the report serializes");
        text.push('\n');
        text
    }

    /// The human summary.
    pub fn summary_text(&self) -> String {
        let s = &self.summary;
        let into = self
            .destination
            .as_deref()
            .map_or_else(String::new, |d| format!(" into {d}/"));
        let mut out = format!(
            "{} {} ({}){into}\n",
            if self.dry_run {
                "would import"
            } else {
                "imported"
            },
            self.source.name,
            self.source.format,
        );
        out.push_str(&format!(
            "written: {} requests, {} collections, {} environments; skipped: {}; dropped: {}; \
             secrets moved: {}; unsupported: {}; unmapped: {}\n",
            s.requests,
            s.collections,
            s.environments,
            s.skipped,
            s.dropped,
            s.secrets_moved,
            s.unsupported,
            s.unmapped,
        ));
        let unchanged = self
            .files
            .iter()
            .filter(|f| f.action == Action::Unchanged)
            .count();
        if unchanged > 0 {
            out.push_str(&format!(
                "unchanged: {unchanged} files already up to date\n"
            ));
        }
        if s.scripts > 0 {
            out.push_str(&format!(
                "{} scripts preserved as comments, not executed\n",
                s.scripts
            ));
        }
        if self.migration.found > 0 {
            out.push_str(&self.migration.summary_line());
        }
        if self.dry_run {
            out.push_str("dry run: nothing was written\n");
            for file in &self.files {
                out.push_str(&format!("  {} {}\n", file.action.as_str(), file.path));
            }
        }
        for secret in &self.secrets {
            out.push_str(&format!(
                "secret: {} -> environment '{}' ({})\n",
                secret.name,
                secret.environment,
                secret.location.display()
            ));
        }
        for conflict in &self.conflicts {
            out.push_str(&format!(
                "conflict: {} {}\n",
                conflict.path, conflict.resolution
            ));
        }
        for note in &self.notes {
            out.push_str(&note_line(note));
        }
        out
    }
}

fn note_line(note: &Note) -> String {
    let at = note.location.display();
    match note.kind {
        NoteKind::Script => format!("{} ({at})\n", note.message),
        kind => format!("{}: {} ({at})\n", kind.as_str(), note.message),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request(found: usize, mapped: usize) -> RequestMigration {
        RequestMigration {
            request: "r".into(),
            path: "r.rman.yaml".into(),
            found,
            mapped,
            unconverted: found - mapped,
            statements: Vec::new(),
        }
    }

    #[test]
    fn migration_percentages_round_down() {
        let m = Migration::new(vec![request(30, 25), request(10, 6)]);
        assert_eq!(
            (m.found, m.mapped, m.unconverted, m.percent),
            (40, 31, 9, 77)
        );
        assert_eq!(
            m.summary_line(),
            "scripts: 31/40 statements converted (77%)\n"
        );
        assert_eq!(Migration::new(Vec::new()).percent, 100);
    }
}
