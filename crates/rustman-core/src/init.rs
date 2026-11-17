//! `rustman init`: create a new workspace.

use std::fs;
use std::path::{Path, PathBuf};

use crate::error::ConfigError;
use crate::format::write::request_to_yaml;
use crate::model::{Assertion, HttpRequest, Method, REQUEST_VERSION, Request, StatusExpectation};
use crate::settings::{SETTINGS_FILE, SETTINGS_VERSION};

const GITIGNORE: &str = ".gitignore";
const GITIGNORE_ENTRY: &str = ".rustman/";
const LOCAL_ENVIRONMENT: &str = "environments/local.yaml";
const SAMPLE_REQUEST: &str = "examples/ping.rman.yaml";

/// What `init` did, with paths relative to the new workspace.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct InitReport {
    pub created: Vec<PathBuf>,
    /// True when `.rustman/` was added to an existing `.gitignore`.
    pub gitignore_appended: bool,
}

/// Create a workspace in `dir`.
///
/// `display` is how `dir` is named in errors. With `bare`, the sample
/// environment and request are skipped. Refuses, without touching any file,
/// when `dir` already has a `rustman.toml`.
pub fn init(dir: &Path, display: &str, bare: bool) -> Result<InitReport, ConfigError> {
    if dir.join(SETTINGS_FILE).exists() {
        return Err(ConfigError::AlreadyWorkspace(display.to_owned()));
    }
    fs::create_dir_all(dir).map_err(|e| ConfigError::io(dir, e))?;
    let mut report = InitReport::default();
    let name = workspace_name(dir);
    write_new(dir, SETTINGS_FILE, &settings_toml(&name, bare), &mut report)?;
    if !bare {
        write_new(
            dir,
            LOCAL_ENVIRONMENT,
            "variables:\n  base_url: https://httpbin.org\n",
            &mut report,
        )?;
        write_new(
            dir,
            SAMPLE_REQUEST,
            &request_to_yaml(&sample_request()),
            &mut report,
        )?;
    }
    update_gitignore(dir, &mut report)?;
    Ok(report)
}

fn workspace_name(dir: &Path) -> String {
    let resolved = dunce::canonicalize(dir).unwrap_or_else(|_| dir.to_path_buf());
    resolved
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "workspace".to_owned())
}

fn settings_toml(name: &str, bare: bool) -> String {
    let quoted = toml::Value::String(name.to_owned());
    let mut text = format!("[workspace]\nname = {quoted}\nversion = {SETTINGS_VERSION}\n");
    if !bare {
        text.push_str("\n[defaults]\ndefault_environment = \"local\"\n");
    }
    text
}

fn sample_request() -> Request {
    Request::Http(Box::new(HttpRequest {
        version: REQUEST_VERSION,
        kind: "http".to_owned(),
        name: "Ping".to_owned(),
        description: Some("Sample request created by rustman init".to_owned()),
        tags: None,
        method: Method::parse("GET").ok(),
        url: "{{base_url}}/get".to_owned(),
        query: None,
        headers: None,
        body: None,
        auth: None,
        settings: None,
        variables: None,
        assertions: Some(vec![Assertion::Status(StatusExpectation::Code(200))]),
        captures: None,
    }))
}

/// Write `rel` unless it already exists (never overwrite user files).
fn write_new(
    dir: &Path,
    rel: &str,
    text: &str,
    report: &mut InitReport,
) -> Result<(), ConfigError> {
    let path = dir.join(rel);
    if path.exists() {
        return Ok(());
    }
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| ConfigError::io(parent, e))?;
    }
    fs::write(&path, text).map_err(|e| ConfigError::io(&path, e))?;
    report.created.push(PathBuf::from(rel));
    Ok(())
}

fn update_gitignore(dir: &Path, report: &mut InitReport) -> Result<(), ConfigError> {
    let path = dir.join(GITIGNORE);
    if !path.exists() {
        return write_new(dir, GITIGNORE, &format!("{GITIGNORE_ENTRY}\n"), report);
    }
    let mut text = fs::read_to_string(&path).map_err(|e| ConfigError::io(&path, e))?;
    if ignores_rustman_dir(&text) {
        return Ok(());
    }
    if !text.is_empty() && !text.ends_with('\n') {
        text.push('\n');
    }
    text.push_str(GITIGNORE_ENTRY);
    text.push('\n');
    fs::write(&path, text).map_err(|e| ConfigError::io(&path, e))?;
    report.gitignore_appended = true;
    Ok(())
}

fn ignores_rustman_dir(gitignore: &str) -> bool {
    gitignore
        .lines()
        .map(str::trim)
        .any(|line| matches!(line, ".rustman" | ".rustman/" | "/.rustman" | "/.rustman/"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workspace::Workspace;

    #[test]
    fn fresh_init_creates_a_loadable_workspace() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("shop");
        let report = init(&dir, "shop", false).unwrap();
        assert_eq!(
            report.created,
            [SETTINGS_FILE, LOCAL_ENVIRONMENT, SAMPLE_REQUEST, GITIGNORE].map(PathBuf::from)
        );
        let ws = Workspace::load(&dir).unwrap();
        assert_eq!(ws.settings.workspace.name, "shop");
        assert_eq!(
            ws.settings.defaults.default_environment.as_deref(),
            Some("local")
        );
        assert_eq!(ws.find_by_name("Ping").len(), 1);
        assert_eq!(
            fs::read_to_string(dir.join(GITIGNORE)).unwrap(),
            ".rustman/\n"
        );
    }

    #[test]
    fn bare_init_skips_samples() {
        let tmp = tempfile::tempdir().unwrap();
        let report = init(tmp.path(), ".", true).unwrap();
        assert_eq!(
            report.created,
            [SETTINGS_FILE, GITIGNORE].map(PathBuf::from)
        );
        assert!(Workspace::load(tmp.path()).unwrap().requests().is_empty());
    }

    #[test]
    fn gitignore_variants_are_recognised() {
        assert!(ignores_rustman_dir("target/\n/.rustman/\n"));
        assert!(ignores_rustman_dir(" .rustman \n"));
        assert!(!ignores_rustman_dir(".rustmanx\n# .rustman/\n"));
    }

    #[test]
    fn existing_gitignore_without_trailing_newline_is_appended() {
        let tmp = tempfile::tempdir().unwrap();
        fs::write(tmp.path().join(GITIGNORE), "target/").unwrap();
        let report = init(tmp.path(), ".", true).unwrap();
        assert!(report.gitignore_appended);
        assert_eq!(
            fs::read_to_string(tmp.path().join(GITIGNORE)).unwrap(),
            "target/\n.rustman/\n"
        );
    }

    #[test]
    fn workspace_name_is_toml_escaped() {
        let text = settings_toml("a\"b", true);
        let (settings, _) =
            crate::settings::Settings::parse(Path::new(SETTINGS_FILE), &text).unwrap();
        assert_eq!(settings.workspace.name, "a\"b");
    }
}
