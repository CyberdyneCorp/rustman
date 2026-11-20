//! `rustman plugin new <name> --kind <extension point>` (plugins spec,
//! "Plugin SDK and template"; task 6.2): a buildable `no_std` Rust project
//! with a manifest, a sample implementation of the extension point, test
//! cases for `rustman plugin test` and a `cargo build --target
//! wasm32-wasip2` recipe. The sources are in this crate's `templates/`.

use std::path::{Path, PathBuf};

use crate::error::LoadError;
use crate::extension::ExtensionPoint;
use crate::harness::TESTS_FILE;
use crate::manifest::{MANIFEST_FILE, is_kebab_case};

/// Where a new plugin project takes `rustman-plugin-sdk` from.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum Sdk {
    /// The Rustman repository on GitHub, until the SDK is on crates.io.
    #[default]
    Git,
    /// A local copy, such as `crates/rustman-plugin-sdk` of a checkout
    /// (`plugin new --sdk <path>`).
    Path(PathBuf),
}

/// The SDK versions a new project accepts: the plugin API major it was
/// scaffolded for, so a repository SDK of another major fails the build
/// instead of drifting.
pub const SDK_MAJOR: &str = "1";

/// The repository the SDK comes from by default.
pub const SDK_REPOSITORY: &str = "https://github.com/CyberdyneCorp/rustman";

impl Sdk {
    /// The source part of the dependency, as TOML.
    fn source(&self) -> String {
        match self {
            Self::Git => format!("git = \"{SDK_REPOSITORY}\", version = \"{SDK_MAJOR}\""),
            Self::Path(path) => {
                let path = path.to_string_lossy().replace('\\', "/");
                format!("path = \"{path}\"")
            }
        }
    }
}

/// What differs between the templates of the extension points.
struct Sample {
    lib: &'static str,
    tests: &'static str,
    description: &'static str,
    capabilities: &'static [&'static str],
    grants: &'static str,
    dependencies: &'static str,
}

const NO_GRANTS: &str = "it needs none";

fn sample(point: ExtensionPoint) -> Sample {
    let plain = |lib, tests, description| Sample {
        lib,
        tests,
        description,
        capabilities: &[],
        grants: NO_GRANTS,
        dependencies: "",
    };
    match point {
        ExtensionPoint::Auth => Sample {
            lib: include_str!("../templates/auth/lib.rs"),
            tests: include_str!("../templates/auth/rustman-plugin-tests.toml"),
            description: "HMAC-SHA256 request signing",
            capabilities: &["clock", "secrets:read"],
            grants: "`clock` and `secrets:read:hmac_key`",
            dependencies: "hmac = { version = \"0.13\", default-features = false }\nsha2 = { version = \"0.11\", default-features = false }\n",
        },
        ExtensionPoint::Assertion => plain(
            include_str!("../templates/assertion/lib.rs"),
            include_str!("../templates/assertion/rustman-plugin-tests.toml"),
            "a status-2xx assertion function",
        ),
        ExtensionPoint::Generator => plain(
            include_str!("../templates/generator/lib.rs"),
            include_str!("../templates/generator/rustman-plugin-tests.toml"),
            "a slug value generator",
        ),
        ExtensionPoint::Transformer => plain(
            include_str!("../templates/transformer/lib.rs"),
            include_str!("../templates/transformer/rustman-plugin-tests.toml"),
            "a request stamp and an XSSI-stripping response transformer",
        ),
        ExtensionPoint::Importer => Sample {
            capabilities: &["fs:read"],
            grants: "`fs:read:<dir>` for the sources",
            ..plain(
                include_str!("../templates/importer/lib.rs"),
                include_str!("../templates/importer/rustman-plugin-tests.toml"),
                "an importer of METHOD URL lines",
            )
        },
        ExtensionPoint::Kind => plain(
            include_str!("../templates/kind/lib.rs"),
            include_str!("../templates/kind/rustman-plugin-tests.toml"),
            "an echo request kind",
        ),
        ExtensionPoint::Report => plain(
            include_str!("../templates/report/lib.rs"),
            include_str!("../templates/report/rustman-plugin-tests.toml"),
            "a plain-text report renderer",
        ),
    }
}

/// The files of a new `point` plugin named `name`, relative to its folder.
pub fn files(name: &str, point: ExtensionPoint, sdk: &Sdk) -> Vec<(&'static str, String)> {
    let sample = sample(point);
    let capabilities: Vec<String> = sample
        .capabilities
        .iter()
        .map(|c| format!("\"{c}\""))
        .collect();
    let fill = |text: &str| {
        text.replace("@@NAME@@", name)
            .replace("@@CRATE@@", &name.replace('-', "_"))
            .replace("@@POINT@@", point.as_str())
            .replace("@@DESCRIPTION@@", sample.description)
            .replace("@@CAPABILITIES@@", &capabilities.join(", "))
            .replace("@@GRANTS@@", sample.grants)
            .replace("@@DEPENDENCIES@@", sample.dependencies)
            .replace("@@SDK@@", &sdk.source())
    };
    vec![
        (
            "Cargo.toml",
            fill(include_str!("../templates/Cargo.toml.in")),
        ),
        (
            MANIFEST_FILE,
            fill(include_str!("../templates/rustman-plugin.toml.in")),
        ),
        ("src/lib.rs", fill(sample.lib)),
        (TESTS_FILE, fill(sample.tests)),
        ("README.md", fill(include_str!("../templates/README.md.in"))),
        (
            ".gitignore",
            fill(include_str!("../templates/gitignore.in")),
        ),
    ]
}

/// Write a new `point` plugin named `name`, built against `sdk`, into
/// `dir`, which must not exist or be empty. Returns the files written.
pub fn scaffold(
    dir: &Path,
    name: &str,
    point: ExtensionPoint,
    sdk: &Sdk,
) -> Result<Vec<PathBuf>, LoadError> {
    if !is_kebab_case(name) {
        return Err(LoadError::invalid(name, "the name is not kebab-case"));
    }
    let io = |path: &Path, e: std::io::Error| {
        LoadError::invalid(name, format!("{}: {e}", path.display()))
    };
    let occupied = std::fs::read_dir(dir).is_ok_and(|mut entries| entries.next().is_some());
    if occupied || dir.is_file() {
        let message = format!("{} already exists and is not empty", dir.display());
        return Err(LoadError::invalid(name, message));
    }
    let mut written = Vec::new();
    for (relative, content) in files(name, point, sdk) {
        let path = dir.join(relative);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| io(parent, e))?;
        }
        std::fs::write(&path, content).map_err(|e| io(&path, e))?;
        written.push(path);
    }
    Ok(written)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::harness::TestFile;
    use crate::manifest::Manifest;

    #[test]
    fn every_template_has_a_valid_manifest_and_test_cases() {
        for point in ExtensionPoint::ALL {
            let files = files("my-plugin", point, &Sdk::Git);
            let text = |name: &str| &files.iter().find(|(n, _)| *n == name).unwrap().1;
            let manifest = Manifest::parse("t", text(MANIFEST_FILE)).unwrap();
            assert_eq!(manifest.name, "my-plugin");
            assert_eq!(manifest.extension_points, [point]);
            assert_eq!(
                manifest.component.as_deref(),
                Some("target/wasm32-wasip2/release/my_plugin.wasm")
            );
            let tests: TestFile = toml::from_str(text(TESTS_FILE)).unwrap();
            assert!(!tests.cases.is_empty(), "{point}");
            for case in &tests.cases {
                case.call().unwrap();
            }
            let cargo: toml::Table = toml::from_str(text("Cargo.toml")).unwrap();
            assert_eq!(cargo["package"]["name"].as_str(), Some("my-plugin"));
            assert!(text("Cargo.toml").contains(&format!("features = [\"{point}\", \"runtime\"]")));
            let sdk = &cargo["dependencies"]["rustman-plugin-sdk"];
            assert_eq!(sdk["git"].as_str(), Some(SDK_REPOSITORY), "{point}");
            assert_eq!(sdk["version"].as_str(), Some(SDK_MAJOR), "{point}");
            for (name, content) in &files {
                assert!(!content.contains("@@"), "{point} {name}");
            }
        }
    }

    /// The request files the importer template's cases expect are valid
    /// request files, `{{variables}}` in the URL included (the cases pin
    /// the template's output; `scripts/check_plugin_templates.py` runs
    /// them against the built template).
    #[test]
    fn importer_template_writes_valid_request_files() {
        let files = files("my-importer", ExtensionPoint::Importer, &Sdk::Git);
        let text = &files.iter().find(|(n, _)| *n == TESTS_FILE).unwrap().1;
        let tests: TestFile = toml::from_str(text).unwrap();
        let registry = rustman_core::model::KindRegistry::default();
        let mut checked = 0;
        for case in &tests.cases {
            for (key, content) in &case.expect {
                let Some(file) = key.strip_prefix("request.") else {
                    continue;
                };
                let loaded = registry.load(Path::new(file), content);
                assert!(loaded.is_ok(), "{}: {loaded:?}", case.name);
                checked += 1;
            }
        }
        assert!(
            tests
                .cases
                .iter()
                .any(|c| c.expect.values().any(|v| v.contains("{{base_url}}")))
        );
        assert_eq!(checked, 2);
    }

    /// A scaffolded project builds before the SDK is published: from the
    /// repository by default, or from a local copy.
    #[test]
    fn the_sdk_comes_from_the_repository_or_a_local_copy() {
        let local = Sdk::Path(PathBuf::from("/src/rustman/crates/rustman-plugin-sdk"));
        let files = files("my-auth", ExtensionPoint::Auth, &local);
        let cargo: toml::Table = toml::from_str(&files[0].1).unwrap();
        let sdk = &cargo["dependencies"]["rustman-plugin-sdk"];
        assert_eq!(
            sdk["path"].as_str(),
            Some("/src/rustman/crates/rustman-plugin-sdk")
        );
        assert!(sdk.get("version").is_none());
    }

    #[test]
    fn scaffolding_refuses_bad_names_and_used_folders() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("my-auth");
        let written = scaffold(&target, "my-auth", ExtensionPoint::Auth, &Sdk::Git).unwrap();
        assert_eq!(written.len(), 6);
        assert!(target.join("src/lib.rs").is_file());
        let err = scaffold(&target, "my-auth", ExtensionPoint::Auth, &Sdk::Git).unwrap_err();
        assert!(
            err.to_string().ends_with("already exists and is not empty"),
            "{err}"
        );
        let err = scaffold(
            &dir.path().join("x"),
            "My_Auth",
            ExtensionPoint::Auth,
            &Sdk::Git,
        )
        .unwrap_err();
        assert_eq!(
            err.to_string(),
            "plugin My_Auth: the name is not kebab-case"
        );
    }
}
