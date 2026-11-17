//! Which requests a run executes, in collection order, and the units of
//! sequential work the scheduler runs concurrently (design D10).

use std::path::{Path, PathBuf};

use super::filter::Filter;
use crate::workspace::{RequestRef, Workspace};

/// The requests of one run.
#[derive(Clone, Debug)]
pub struct Plan<'a> {
    /// Every selected request, in collection order. Reports follow this order.
    pub requests: Vec<RequestRef<'a>>,
    /// Indices into `requests` per folder, in order. A folder whose
    /// `_collection.yaml` sets `parallel: true` gives one unit per request.
    folders: Vec<Vec<usize>>,
}

impl Plan<'_> {
    pub fn len(&self) -> usize {
        self.requests.len()
    }

    pub fn is_empty(&self) -> bool {
        self.requests.is_empty()
    }

    /// The units of sequential work for `parallel` concurrent units. With
    /// one, the whole run is a single unit in collection order, so captures
    /// chain across folders too.
    pub fn units(&self, parallel: usize) -> Vec<Vec<usize>> {
        if parallel <= 1 {
            vec![(0..self.requests.len()).collect()]
        } else {
            self.folders.clone()
        }
    }
}

/// The requests under `scopes` (paths relative to the workspace root, a
/// request file or a folder; empty for the whole workspace) that match any
/// of `filters` (all requests without filters), in collection order.
pub fn plan<'a>(ws: &'a Workspace, scopes: &[PathBuf], filters: &[Filter]) -> Plan<'a> {
    let in_scope = |path: &Path| scopes.is_empty() || scopes.iter().any(|s| path.starts_with(s));
    let requests: Vec<RequestRef<'a>> = ws
        .requests()
        .into_iter()
        .filter(|r| in_scope(&r.entry.path))
        .filter(|r| filters.is_empty() || filters.iter().any(|f| f.matches(r)))
        .collect();
    let folders = folder_units(&requests);
    Plan { requests, folders }
}

fn folder_units(requests: &[RequestRef<'_>]) -> Vec<Vec<usize>> {
    let mut units: Vec<Vec<usize>> = Vec::new();
    let mut by_folder: Vec<(PathBuf, usize)> = Vec::new();
    for (index, request) in requests.iter().enumerate() {
        let parallel = request
            .ancestors
            .last()
            .is_some_and(|c| c.meta.parallel == Some(true));
        let folder = request.entry.path.parent().unwrap_or(Path::new(""));
        let existing = by_folder.iter().find(|(f, _)| f == folder);
        match existing {
            Some((_, unit)) if !parallel => units[*unit].push(index),
            _ => {
                by_folder.push((folder.to_path_buf(), units.len()));
                units.push(vec![index]);
            }
        }
    }
    units
}

#[cfg(test)]
mod tests {
    use super::*;
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

    fn request(name: &str, tags: &str) -> String {
        format!("version: 1\nkind: http\nname: {name}\ntags: [{tags}]\nurl: http://h/{name}\n")
    }

    fn ids(plan: &Plan<'_>) -> Vec<String> {
        plan.requests.iter().map(|r| r.entry.id()).collect()
    }

    #[test]
    fn scopes_and_filters_keep_collection_order() {
        let (a1, a2, b1) = (
            request("a1", "smoke"),
            request("a2", "slow"),
            request("b1", "smoke"),
        );
        let (_dir, ws) = workspace(&[
            ("a/1.rman.yaml", &a1),
            ("a/2.rman.yaml", &a2),
            ("b/1.rman.yaml", &b1),
        ]);
        assert_eq!(ids(&plan(&ws, &[], &[])), ["a/1", "a/2", "b/1"]);
        assert_eq!(ids(&plan(&ws, &["b".into()], &[])), ["b/1"]);
        assert_eq!(ids(&plan(&ws, &["a/2.rman.yaml".into()], &[])), ["a/2"]);
        let smoke = [Filter::parse("tag:smoke")];
        assert_eq!(ids(&plan(&ws, &[], &smoke)), ["a/1", "b/1"]);
        let glob = [Filter::parse("a/*")];
        assert_eq!(ids(&plan(&ws, &[], &glob)), ["a/1", "a/2"]);
    }

    #[test]
    fn folders_are_units_unless_parallel() {
        let r = request("r", "");
        let (_dir, ws) = workspace(&[
            ("a/1.rman.yaml", &r),
            ("a/2.rman.yaml", &r),
            ("b/_collection.yaml", "parallel: true\n"),
            ("b/1.rman.yaml", &r),
            ("b/2.rman.yaml", &r),
            ("c.rman.yaml", &r),
        ]);
        let plan = plan(&ws, &[], &[]);
        assert_eq!(ids(&plan), ["a/1", "a/2", "b/1", "b/2", "c"]);
        assert_eq!(plan.units(2), vec![vec![0, 1], vec![2], vec![3], vec![4]]);
        assert_eq!(plan.units(1), vec![vec![0, 1, 2, 3, 4]]);
    }
}
