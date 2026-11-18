//! Data-driven runs (cli-runner "Data-driven runs"): which data file a run
//! uses, which of its columns are secret and how iterations are labeled.
//! Reading the file itself is [`crate::data_rows`].

use std::path::{Path, PathBuf};

use super::Plan;
use super::record::DataRowRecord;
use crate::collection::{COLLECTION_FILE, Collection, Entry};
use crate::data_rows::{DataRow, DataRows, SecretColumns};
use crate::paths::contain;
use crate::workspace::Workspace;

/// A collection's default data file (`_collection.yaml` `data`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CollectionData {
    /// The file, checked to be inside the workspace.
    pub path: PathBuf,
    /// The `_collection.yaml` that names it, relative to the workspace
    /// root with `/`, for messages.
    pub collection_file: String,
}

/// The default data file of a run over `scopes` (paths relative to the
/// workspace root): the `data` of the nearest collection at or above each
/// scope. Every scope must agree; none setting `data` gives `None`.
pub fn collection_data(
    ws: &Workspace,
    scopes: &[PathBuf],
) -> Result<Option<CollectionData>, String> {
    let mut found: Option<CollectionData> = None;
    for scope in scopes {
        let Some(data) = nearest_data(ws, scope)? else {
            continue;
        };
        match &found {
            Some(other) if other.path != data.path => {
                return Err(format!(
                    "{} and {} name different data files; pass --data or --no-data",
                    other.collection_file, data.collection_file
                ));
            }
            _ => found = Some(data),
        }
    }
    Ok(found)
}

fn nearest_data(ws: &Workspace, scope: &Path) -> Result<Option<CollectionData>, String> {
    let chain = collection_chain(&ws.tree, scope);
    let Some(collection) = chain.iter().rev().find(|c| c.meta.data.is_some()) else {
        return Ok(None);
    };
    let raw = collection.meta.data.as_deref().unwrap_or_default();
    let file = collection.path.join(COLLECTION_FILE);
    let collection_file = file.to_string_lossy().replace('\\', "/");
    let path = contain(&ws.root, &ws.root.join(&collection.path), raw)
        .map_err(|e| format!("{collection_file}: data: {e}"))?;
    Ok(Some(CollectionData {
        path,
        collection_file,
    }))
}

/// The collections from the root down to the folder `scope` names, or that
/// holds the request file it names.
fn collection_chain<'a>(root: &'a Collection, scope: &Path) -> Vec<&'a Collection> {
    let mut chain = vec![root];
    let mut current = root;
    while let Some(child) = current.entries.iter().find_map(|entry| match entry {
        Entry::Collection(c) if scope.starts_with(&c.path) => Some(c),
        _ => None,
    }) {
        chain.push(child);
        current = child;
    }
    chain
}

/// Every column the collections of `plan`'s requests list in
/// `secret_columns`, plus `extra` (`--secret-columns`).
pub fn secret_columns(plan: &Plan<'_>, extra: &[String]) -> SecretColumns {
    let mut named: Vec<String> = extra.to_vec();
    let listed = plan
        .requests
        .iter()
        .flat_map(|r| r.ancestors.iter())
        .filter_map(|c| c.meta.secret_columns.as_deref())
        .flatten();
    for column in listed {
        if !named.contains(column) {
            named.push(column.clone());
        }
    }
    SecretColumns::new(named)
}

/// The rows a run iterates over, with what makes their values secret and
/// how iterations are labeled.
#[derive(Clone, Debug)]
pub struct DataRun {
    pub rows: DataRows,
    pub secret: SecretColumns,
    /// `--data-label`: the column whose value labels each iteration.
    label: Option<String>,
}

impl DataRun {
    /// A data run over `rows`. The label column must exist and must not be
    /// secret: its values are printed in every report.
    pub fn new(
        rows: DataRows,
        secret: SecretColumns,
        label: Option<String>,
    ) -> Result<Self, String> {
        if let Some(column) = &label {
            if secret.is_secret(column) {
                return Err(format!(
                    "--data-label {column}: a secret column cannot label iterations"
                ));
            }
            if !rows.columns().contains(column) {
                return Err(format!(
                    "--data-label {column}: no such column (columns: {})",
                    rows.columns().join(", ")
                ));
            }
        }
        Ok(Self {
            rows,
            secret,
            label,
        })
    }

    /// How many iterations the run has: one per row.
    pub fn total(&self) -> u32 {
        u32::try_from(self.rows.len()).unwrap_or(u32::MAX)
    }

    /// The row of 1-based iteration `index`.
    pub fn row(&self, index: u32) -> Option<&DataRow> {
        let index = usize::try_from(index).ok()?.checked_sub(1)?;
        self.rows.rows().get(index)
    }

    /// The data row of iteration `index` in reports, labeled by the label
    /// column's value (line breaks become spaces, to keep report lines
    /// whole), else `row <index>`, also when that value is blank.
    pub fn data_row(&self, index: u32) -> DataRowRecord {
        let value = self.label.as_deref().and_then(|column| {
            self.row(index)
                .and_then(|row| row.get(column))
                .map(|value| value.replace(['\r', '\n'], " "))
                .filter(|value| !value.trim().is_empty())
        });
        let label = value.unwrap_or_else(|| format!("row {index}"));
        DataRowRecord::new(index, self.total(), label)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::run::plan;
    use std::fs;

    fn workspace(files: &[(&str, &str)]) -> (tempfile::TempDir, Workspace) {
        let dir = tempfile::tempdir().unwrap();
        let mut all = vec![("rustman.toml", "[workspace]\nname = \"t\"\nversion = 1\n")];
        all.extend_from_slice(files);
        for (path, text) in all {
            let path = dir.path().join(path);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, text).unwrap();
        }
        let ws = Workspace::load(dir.path()).unwrap();
        (dir, ws)
    }

    const REQUEST: &str = "version: 1\nkind: http\nname: r\nurl: http://h/\n";

    fn rows(text: &str) -> DataRows {
        DataRows::from_csv(text.as_bytes()).unwrap()
    }

    #[test]
    fn the_nearest_collection_data_applies() {
        let (dir, ws) = workspace(&[
            ("api/tenants/_collection.yaml", "data: tenants.csv\n"),
            ("api/tenants/a.rman.yaml", REQUEST),
            ("api/other/a.rman.yaml", REQUEST),
        ]);
        let found = collection_data(&ws, &["api/tenants".into()])
            .unwrap()
            .unwrap();
        assert_eq!(found.path, dir.path().join("api/tenants/tenants.csv"));
        assert_eq!(found.collection_file, "api/tenants/_collection.yaml");
        let file = collection_data(&ws, &["api/tenants/a.rman.yaml".into()]).unwrap();
        assert_eq!(file, Some(found));
        assert_eq!(collection_data(&ws, &["api/other".into()]).unwrap(), None);
        assert_eq!(collection_data(&ws, &["".into()]).unwrap(), None);
    }

    #[test]
    fn collection_data_must_stay_in_the_workspace() {
        let (_dir, ws) = workspace(&[
            ("api/_collection.yaml", "data: ../../outside.csv\n"),
            ("api/a.rman.yaml", REQUEST),
        ]);
        assert_eq!(
            collection_data(&ws, &["api".into()]).unwrap_err(),
            "api/_collection.yaml: data: file path escapes workspace: ../../outside.csv"
        );
    }

    #[test]
    fn scopes_with_different_data_files_are_ambiguous() {
        let (_dir, ws) = workspace(&[
            ("a/_collection.yaml", "data: a.csv\n"),
            ("a/r.rman.yaml", REQUEST),
            ("b/_collection.yaml", "data: b.csv\n"),
            ("b/r.rman.yaml", REQUEST),
        ]);
        assert_eq!(
            collection_data(&ws, &["a".into(), "b".into()]).unwrap_err(),
            "a/_collection.yaml and b/_collection.yaml name different data files; pass --data or --no-data"
        );
    }

    #[test]
    fn secret_columns_come_from_the_flag_and_every_collection_of_the_plan() {
        let (_dir, ws) = workspace(&[
            ("_collection.yaml", "secret_columns: [pin]\n"),
            ("api/_collection.yaml", "secret_columns: [ssn]\n"),
            ("api/a.rman.yaml", REQUEST),
        ]);
        let plan = plan(&ws, &[], &[]);
        let secret = secret_columns(&plan, &["dob".to_owned()]);
        for column in ["pin", "ssn", "dob", "password"] {
            assert!(secret.is_secret(column), "{column}");
        }
        assert!(!secret.is_secret("email"));
    }

    #[test]
    fn iterations_are_labeled_by_column_or_row_number() {
        let data = rows("email,password\nana@example.com,x\nbo@example.com,y\n");
        let run = DataRun::new(data.clone(), SecretColumns::default(), None).unwrap();
        assert_eq!(run.data_row(2), DataRowRecord::new(2, 2, "row 2".into()));
        let run = DataRun::new(data.clone(), SecretColumns::default(), Some("email".into()));
        assert_eq!(run.unwrap().data_row(1).label, "ana@example.com");
        let secret = DataRun::new(
            data.clone(),
            SecretColumns::default(),
            Some("password".into()),
        );
        assert_eq!(
            secret.unwrap_err(),
            "--data-label password: a secret column cannot label iterations"
        );
        let missing = DataRun::new(data, SecretColumns::default(), Some("id".into()));
        assert_eq!(
            missing.unwrap_err(),
            "--data-label id: no such column (columns: email, password)"
        );
    }

    /// Regression: an empty label cell gave an empty label.
    #[test]
    fn blank_label_cells_fall_back_to_the_row_number() {
        let data = rows("email,role\nana@example.com,admin\n,user\n \t,user\n");
        let run = DataRun::new(data, SecretColumns::default(), Some("email".into())).unwrap();
        assert_eq!(run.data_row(1).label, "ana@example.com");
        assert_eq!(run.data_row(2).label, "row 2");
        assert_eq!(run.data_row(3).label, "row 3");
    }
}
