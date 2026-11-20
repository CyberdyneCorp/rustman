//! The host test harness (plugins spec, "Host test harness"; task 6.3):
//! runs a plugin component in the real sandbox, with exactly the grants
//! each case lists, against fake clock, randomness, environment, network
//! and secrets, so the same cases give byte-identical results on every run
//! and platform. `rustman plugin test <path>` prints the [`Report`].
//!
//! Everything shown (output lines, plugin logs, problems) is redacted of
//! the fake secrets, like plugin output in a real session.

mod calls;
pub mod cases;
pub mod fakes;

use std::path::{Path, PathBuf};
use std::sync::Arc;

use rustman_core::{Redactor, Secret};

use crate::declare::{Declaration, Source};
use crate::error::LoadError;
use crate::manifest::Manifest;
use crate::pin::sha256_hex;
use crate::plugin::Plugin;
use crate::sandbox::{HostContext, LogLevel};
use crate::source::{Package, read_path};
use crate::workspace::check;

pub use calls::Line;
pub use cases::{Fakes, TESTS_FILE, TestFile};
pub use fakes::FakeServices;

/// The outcome of one case.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CaseReport {
    pub name: String,
    /// The call's output lines, redacted.
    pub output: Vec<Line>,
    /// What the plugin logged and the host calls refused, redacted.
    pub logs: Vec<String>,
    /// Why the case failed; empty when it passed.
    pub problems: Vec<String>,
}

impl CaseReport {
    pub fn passed(&self) -> bool {
        self.problems.is_empty()
    }
}

/// The outcome of every case of a plugin.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Report {
    pub plugin: String,
    pub version: String,
    pub cases: Vec<CaseReport>,
}

impl Report {
    pub fn passed(&self) -> bool {
        self.cases.iter().all(CaseReport::passed)
    }
}

/// The plugin at `path` (a folder holding `rustman-plugin.toml`, or a
/// `.wasm` file) and its test cases (`cases`, default
/// `rustman-plugin-tests.toml` next to the manifest).
struct Subject {
    dir: PathBuf,
    package: Package,
    manifest: Manifest,
    tests: TestFile,
}

/// Run the test cases of the plugin at `path`. `Err` is a plugin or test
/// file that cannot be read; a failing case is in the report.
pub fn run(path: &Path, cases: Option<&Path>) -> Result<Report, LoadError> {
    let subject = subject(path, cases)?;
    let reports = subject
        .tests
        .cases
        .iter()
        .map(|case| run_case(&subject, case))
        .collect();
    Ok(Report {
        plugin: subject.manifest.name.clone(),
        version: subject.manifest.version.to_string(),
        cases: reports,
    })
}

fn subject(path: &Path, cases: Option<&Path>) -> Result<Subject, LoadError> {
    let context = "under test";
    let (dir, source) = if path.is_dir() {
        (path.to_path_buf(), ".".to_owned())
    } else {
        let dir = path.parent().unwrap_or(Path::new(".")).to_path_buf();
        let file = path.file_name().unwrap_or_default();
        (dir, file.to_string_lossy().into_owned())
    };
    let package = read_path(&dir, context, &source)?;
    let manifest = Manifest::parse(context, &package.manifest)?;
    manifest.check_api()?;
    let file = cases.map_or_else(|| dir.join(TESTS_FILE), Path::to_path_buf);
    let name = &manifest.name;
    let text = std::fs::read_to_string(&file)
        .map_err(|e| LoadError::invalid(name, format!("{}: {e}", file.display())))?;
    let tests: TestFile = toml::from_str(&text).map_err(|e| {
        let message = format!("{}: {}", file.display(), e.message().trim_end());
        LoadError::invalid(name, message)
    })?;
    if tests.cases.is_empty() {
        let message = format!("{}: no [[case]]", file.display());
        return Err(LoadError::invalid(name, message));
    }
    Ok(Subject {
        dir,
        package,
        manifest,
        tests,
    })
}

fn run_case(subject: &Subject, case: &cases::RawCase) -> CaseReport {
    let fakes = subject.tests.fakes.merged(&case.fakes);
    let redactor = fake_redactor(&fakes);
    let mut report = CaseReport {
        name: case.name.clone(),
        output: Vec::new(),
        logs: Vec::new(),
        problems: Vec::new(),
    };
    match call(subject, case, &fakes) {
        Ok((outcome, logs)) => {
            report.problems = problems(case, &outcome);
            report.output = outcome.unwrap_or_default();
            report.logs = logs;
        }
        Err(problem) => report.problems.push(problem),
    }
    redact(&mut report, &redactor);
    report
}

/// The call's outcome and the plugin's logs.
type Called = (Result<Vec<Line>, String>, Vec<String>);

fn call(subject: &Subject, case: &cases::RawCase, fakes: &Fakes) -> Result<Called, String> {
    let call = case.call()?;
    let name = &subject.manifest.name;
    let services = FakeServices::new(fakes)?;
    let context = HostContext::new(&subject.dir)
        .with_secrets(secrets(fakes))
        .with_services(Arc::new(services));
    let declaration = Declaration {
        name: name.clone(),
        source: Source::Path(".".to_owned()),
        sha256: sha256_hex(&subject.package.wasm),
        grants: case.grants.clone(),
        limits: case.limits.validate(name).map_err(|e| e.to_string())?,
    };
    let verified = check(declaration, subject.package.clone()).map_err(|e| e.to_string())?;
    let plugin = Plugin::new(&verified, Arc::new(context)).map_err(|e| e.to_string())?;
    let outcome = calls::make(&plugin, &call);
    let mut logs: Vec<String> = plugin.take_logs().into_iter().map(log_line).collect();
    logs.extend(plugin.take_denials());
    Ok((outcome, logs))
}

/// Why `outcome` does not meet the case's expectations.
fn problems(case: &cases::RawCase, outcome: &Result<Vec<Line>, String>) -> Vec<String> {
    match (outcome, &case.expect_error) {
        (Ok(_), Some(expected)) => {
            vec![format!(
                "expected an error containing {expected:?}, but the call succeeded"
            )]
        }
        (Ok(lines), None) => case
            .expect
            .iter()
            .filter_map(|(key, want)| mismatch(lines, key, want))
            .collect(),
        (Err(error), Some(expected)) if error.contains(expected.as_str()) => Vec::new(),
        (Err(error), Some(expected)) => {
            vec![format!(
                "expected an error containing {expected:?}, got: {error}"
            )]
        }
        (Err(error), None) => vec![error.clone()],
    }
}

fn mismatch(lines: &[Line], key: &str, want: &str) -> Option<String> {
    match lines.iter().find(|(name, _)| name == key) {
        Some((_, got)) if got == want => None,
        Some((_, got)) => Some(format!("expected {key} = {want:?}, got {got:?}")),
        None => Some(format!("expected {key} = {want:?}, but there is no {key}")),
    }
}

fn secrets(fakes: &Fakes) -> Vec<Secret<String>> {
    fakes
        .secrets
        .iter()
        .map(|(name, value)| Secret::new(name.clone(), value.clone()))
        .collect()
}

fn fake_redactor(fakes: &Fakes) -> Redactor {
    let mut redactor = Redactor::new();
    for secret in secrets(fakes) {
        redactor.add(&secret);
    }
    redactor
}

fn redact(report: &mut CaseReport, redactor: &Redactor) {
    let text = |value: &mut String| *value = redactor.text(value).into_owned();
    report.output.iter_mut().for_each(|(_, value)| text(value));
    report.logs.iter_mut().for_each(text);
    report.problems.iter_mut().for_each(text);
}

fn log_line(log: crate::sandbox::PluginLog) -> String {
    let level = match log.level {
        LogLevel::Debug => "debug",
        LogLevel::Info => "info",
        LogLevel::Warn => "warn",
        LogLevel::Error => "error",
    };
    format!("log {level}: {}", log.message)
}
