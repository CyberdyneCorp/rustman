//! Validating and writing what an importer plugin returned (plugins spec,
//! "Importer extension point"). The plugin writes nothing: the host checks
//! every request file and environment against the workspace schema first,
//! and writes nothing at all when one of them is invalid.

use std::path::{Path, PathBuf};

use crate::environment::{self, ENVIRONMENTS_DIR};
use crate::error::ConfigError;
use crate::model::{KindRegistry, REQUEST_FILE_SUFFIX, Request};
use crate::plugins::{Extensions, ImportedFiles};
use crate::secrets::STATE_DIR;

/// What to do with a file that already exists.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum OnConflict {
    #[default]
    Skip,
    Overwrite,
    /// Write next to it, as `<name>-2.rman.yaml` and so on.
    Rename,
}

/// A checked file, ready to write.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Planned {
    /// Relative to the workspace root, with `/`.
    pub path: String,
    pub content: String,
}

/// What happened to a planned file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Written {
    Created(String),
    Replaced(String),
    /// Written under another name, as the file existed.
    Renamed {
        from: String,
        to: String,
    },
    /// Left as it was, as the file existed.
    Skipped(String),
}

/// Check every file of `files`: requests go under `into` (relative to the
/// workspace root, with `/`; empty for the root) and must load as a
/// built-in kind or a kind a plugin of `extensions` registers;
/// environments go under `environments/`. The first invalid file fails the
/// whole import, naming the plugin and the file.
pub fn plan(
    into: &str,
    files: &ImportedFiles,
    extensions: &dyn Extensions,
) -> Result<Vec<Planned>, String> {
    let fail = |message: String| format!("plugin {}: {message}", files.plugin);
    let mut planned = Vec::new();
    for (path, content) in &files.requests {
        let path = relative(path, REQUEST_FILE_SUFFIX).map_err(fail)?;
        check_request(&path, content, extensions).map_err(fail)?;
        planned.push(Planned {
            path: join(into, &path),
            content: content.clone(),
        });
    }
    for (path, content) in &files.environments {
        let path = relative(path, ".yaml").map_err(fail)?;
        check_environment(&path, content).map_err(fail)?;
        planned.push(Planned {
            path: join(ENVIRONMENTS_DIR, &path),
            content: content.clone(),
        });
    }
    Ok(planned)
}

/// `path` as plain relative parts ending in `suffix`: no absolute path, no
/// `..`, no empty part, nothing in `.rustman/`.
fn relative(path: &str, suffix: &str) -> Result<String, String> {
    let parts: Vec<&str> = path.split('/').collect();
    let bad = |part: &&str| {
        part.is_empty() || *part == "." || *part == ".." || part.contains(['\\', ':'])
    };
    if parts.iter().any(bad) || parts[0].eq_ignore_ascii_case(STATE_DIR) {
        return Err(format!("invalid file path '{path}'"));
    }
    if !path.ends_with(suffix) || path.len() == suffix.len() {
        return Err(format!("file path '{path}' must end with {suffix}"));
    }
    Ok(parts.join("/"))
}

fn join(dir: &str, path: &str) -> String {
    if dir.is_empty() {
        path.to_owned()
    } else {
        format!("{dir}/{path}")
    }
}

fn check_request(path: &str, content: &str, extensions: &dyn Extensions) -> Result<(), String> {
    let file = Path::new(path);
    let registry = KindRegistry::default().with_opaque_fallback();
    let request = registry.load(file, content).map_err(|e| e.to_string())?;
    let Request::Opaque(opaque) = request else {
        return Ok(());
    };
    let kinds: Vec<String> = extensions.kinds().into_iter().map(|k| k.name).collect();
    if !kinds.contains(&opaque.kind) {
        let mut expected: Vec<&str> = crate::model::BUILTIN_KINDS.to_vec();
        expected.extend(kinds.iter().map(String::as_str));
        return Err(format!(
            "{path}: unknown request kind '{}' (expected one of: {})",
            opaque.kind,
            expected.join(", ")
        ));
    }
    let fields = opaque.kind_fields(file).map_err(|e| e.to_string())?;
    extensions
        .validate_kind(&opaque.kind, &fields)
        .map_err(|e| format!("{path}: {e}"))
}

fn check_environment(path: &str, content: &str) -> Result<(), String> {
    let name = path.strip_suffix(".yaml").unwrap_or(path);
    if path.contains('/') || !environment::is_valid_name(name) {
        return Err(format!("invalid environment file '{path}'"));
    }
    environment::parse(Path::new(path), name, content)
        .map(drop)
        .map_err(|e| e.to_string())
}

/// Write `planned` under the workspace `root`.
pub fn write(
    root: &Path,
    planned: &[Planned],
    on_conflict: OnConflict,
) -> Result<Vec<Written>, ConfigError> {
    let mut written = Vec::new();
    for file in planned {
        let target = root.join(&file.path);
        let outcome = match (target.exists(), on_conflict) {
            (false, _) => Written::Created(file.path.clone()),
            (true, OnConflict::Skip) => {
                written.push(Written::Skipped(file.path.clone()));
                continue;
            }
            (true, OnConflict::Overwrite) => Written::Replaced(file.path.clone()),
            (true, OnConflict::Rename) => Written::Renamed {
                from: file.path.clone(),
                to: free_name(root, &file.path),
            },
        };
        let path = match &outcome {
            Written::Renamed { to, .. } => root.join(to),
            _ => target,
        };
        write_file(&path, &file.content)?;
        written.push(outcome);
    }
    Ok(written)
}

/// `<stem>-<n><suffix>` for the first `n` from 2 that names no file.
fn free_name(root: &Path, path: &str) -> String {
    let suffix = if path.ends_with(REQUEST_FILE_SUFFIX) {
        REQUEST_FILE_SUFFIX
    } else {
        ".yaml"
    };
    let stem = &path[..path.len() - suffix.len()];
    (2..)
        .map(|n| format!("{stem}-{n}{suffix}"))
        .find(|candidate| !root.join(candidate).exists())
        .expect("some name is free")
}

fn write_file(path: &PathBuf, content: &str) -> Result<(), ConfigError> {
    let io = |source| ConfigError::Io {
        path: path.clone(),
        source,
    };
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(io)?;
    }
    std::fs::write(path, content).map_err(io)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plugins::{PluginExchange, PluginVerdict, SignOutput, SignRequest};

    struct NoKinds;

    impl Extensions for NoKinds {
        fn generate(&self, _: &str, _: &str, _: &[String]) -> Result<String, String> {
            unreachable!()
        }
        fn sign(&self, _: &str, _: &SignRequest) -> Result<SignOutput, String> {
            unreachable!()
        }
        fn check(&self, _: &str, _: &PluginExchange, _: &str) -> PluginVerdict {
            unreachable!()
        }
    }

    const REQUEST: &str = "version: 1\nkind: http\nname: one\nurl: https://api.example.com/\n";

    fn files(requests: &[(&str, &str)], environments: &[(&str, &str)]) -> ImportedFiles {
        let own = |files: &[(&str, &str)]| {
            files
                .iter()
                .map(|(p, c)| ((*p).to_owned(), (*c).to_owned()))
                .collect()
        };
        ImportedFiles {
            plugin: "soapui".into(),
            requests: own(requests),
            environments: own(environments),
        }
    }

    #[test]
    fn valid_files_are_planned_under_their_folders() {
        let env = "variables:\n  base_url: https://api.example.com\n";
        let planned = plan(
            "imported",
            &files(&[("orders/one.rman.yaml", REQUEST)], &[("dev.yaml", env)]),
            &NoKinds,
        )
        .unwrap();
        let paths: Vec<&str> = planned.iter().map(|p| p.path.as_str()).collect();
        assert_eq!(
            paths,
            ["imported/orders/one.rman.yaml", "environments/dev.yaml"]
        );
    }

    /// "Invalid output": an unregistered kind fails the whole import,
    /// naming the plugin and the request.
    #[test]
    fn an_unregistered_kind_fails_the_import() {
        let bad = "version: 1\nkind: soap\nname: two\n";
        let error = plan(
            "",
            &files(&[("one.rman.yaml", REQUEST), ("two.rman.yaml", bad)], &[]),
            &NoKinds,
        )
        .unwrap_err();
        assert_eq!(
            error,
            "plugin soapui: two.rman.yaml: unknown request kind 'soap' (expected one of: http, graphql, jsonrpc, grpc, mcp, ws, sse)"
        );
    }

    #[test]
    fn paths_stay_in_their_folder() {
        for path in [
            "../x.rman.yaml",
            "/etc/x.rman.yaml",
            "a//b.rman.yaml",
            ".rustman/x.rman.yaml",
            "a\\b.rman.yaml",
            "C:x.rman.yaml",
            "x.yaml",
            ".rman.yaml",
        ] {
            let error = plan("", &files(&[(path, REQUEST)], &[]), &NoKinds).unwrap_err();
            assert!(error.starts_with("plugin soapui: "), "{path}: {error}");
        }
        let error = plan("", &files(&[], &[("a/dev.yaml", "{}")]), &NoKinds).unwrap_err();
        assert_eq!(
            error,
            "plugin soapui: invalid environment file 'a/dev.yaml'"
        );
    }

    #[test]
    fn conflicts_skip_overwrite_or_rename() {
        let dir = tempfile::tempdir().unwrap();
        let planned = [Planned {
            path: "a/one.rman.yaml".into(),
            content: "new".into(),
        }];
        assert_eq!(
            write(dir.path(), &planned, OnConflict::Skip).unwrap(),
            [Written::Created("a/one.rman.yaml".into())]
        );
        assert_eq!(
            write(dir.path(), &planned, OnConflict::Skip).unwrap(),
            [Written::Skipped("a/one.rman.yaml".into())]
        );
        assert_eq!(
            write(dir.path(), &planned, OnConflict::Rename).unwrap(),
            [Written::Renamed {
                from: "a/one.rman.yaml".into(),
                to: "a/one-2.rman.yaml".into()
            }]
        );
        assert_eq!(
            write(dir.path(), &planned, OnConflict::Overwrite).unwrap(),
            [Written::Replaced("a/one.rman.yaml".into())]
        );
        assert!(dir.path().join("a/one-2.rman.yaml").exists());
    }
}
