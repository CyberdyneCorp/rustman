//! Renderers of the canonical [`Report`]: JSON, JUnit XML, SARIF 2.1.0,
//! Markdown and self-contained HTML. They only ever see the report, whose
//! every text was redacted when it was built, so no rendering can recover a
//! secret.

mod chart;
mod human;
pub(crate) mod junit;
mod sarif;
pub(crate) mod view;

use std::fmt;
use std::path::{Path, PathBuf};
use std::str::FromStr;

pub use sarif::level as sarif_level;
pub use view::{line_of, pointer};

use crate::exploration::Severity;
use crate::report::Report;

/// A `report render` / `--report` format.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RenderFormat {
    Json,
    Junit,
    Markdown,
    Html,
    Sarif,
}

impl RenderFormat {
    pub const ALL: [Self; 5] = [
        Self::Json,
        Self::Junit,
        Self::Markdown,
        Self::Html,
        Self::Sarif,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Json => "json",
            Self::Junit => "junit",
            Self::Markdown => "markdown",
            Self::Html => "html",
            Self::Sarif => "sarif",
        }
    }
}

impl fmt::Display for RenderFormat {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for RenderFormat {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::ALL
            .into_iter()
            .find(|f| f.as_str() == s)
            .ok_or_else(|| {
                format!(
                    "unknown report format '{s}' (expected json, junit, markdown, html or sarif)"
                )
            })
    }
}

/// What a rendering needs besides the report.
#[derive(Clone, Debug, Default)]
pub struct RenderOptions {
    /// Findings at or above this fail JUnit test cases (default: the
    /// exploration's own `--fail-on`).
    pub fail_on: Option<Severity>,
    /// SARIF locations are relative to this folder when the document is
    /// under it (the current directory, usually the repository).
    pub base: Option<PathBuf>,
    /// The OpenAPI document's text, so SARIF regions name the operation's
    /// line.
    pub document: Option<String>,
}

impl RenderOptions {
    /// The SARIF artifact URI of `spec`: a URL as is, a file relative to
    /// [`Self::base`] when under it, always with `/`.
    pub fn document_uri(&self, spec: &str) -> String {
        if spec.contains("://") {
            return spec.to_owned();
        }
        let path = Path::new(spec);
        let relative = self
            .base
            .as_deref()
            .and_then(|base| path.strip_prefix(base).ok())
            .unwrap_or(path);
        relative.to_string_lossy().replace('\\', "/")
    }
}

/// `report` in `format`, ending in a newline.
pub fn render(report: &Report, format: RenderFormat, options: &RenderOptions) -> String {
    match (format, report) {
        (RenderFormat::Json, _) => report.to_json(),
        (RenderFormat::Junit, Report::Run(r)) => junit::run_junit(r),
        (RenderFormat::Junit, Report::Exploration(r)) => {
            junit::exploration_junit(r, options.fail_on.unwrap_or(r.settings.fail_on))
        }
        (RenderFormat::Sarif, Report::Run(r)) => sarif::run_sarif(r),
        (RenderFormat::Sarif, Report::Exploration(r)) => sarif::exploration_sarif(r, options),
        (RenderFormat::Junit, Report::Contract(r)) => junit::contract_junit(r),
        (RenderFormat::Sarif, Report::Contract(r)) => sarif::contract_sarif(r, options),
        (RenderFormat::Junit, Report::Bench(r)) => junit::bench_junit(r),
        (RenderFormat::Sarif, Report::Bench(r)) => sarif::bench_sarif(r),
        (RenderFormat::Markdown, _) => human::markdown(report, options),
        (RenderFormat::Html, _) => human::html(report, options),
    }
}

/// One `--report <format>[=<path>]` value: a format, written to `path` or
/// to stdout.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReportTarget {
    pub format: RenderFormat,
    pub path: Option<PathBuf>,
}

impl FromStr for ReportTarget {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        let (name, path) = match value.split_once('=') {
            Some((_, "")) => return Err(format!("'{value}' has no path after '='")),
            Some((name, path)) => (name, Some(PathBuf::from(path))),
            None => (value, None),
        };
        Ok(Self {
            format: name.parse()?,
            path,
        })
    }
}

impl ReportTarget {
    /// Render `report` and write it to the target's file (creating its
    /// folder, replacing the file atomically); a target without a path
    /// returns the text for stdout instead.
    pub fn write(
        &self,
        report: &Report,
        options: &RenderOptions,
    ) -> std::io::Result<Option<String>> {
        let text = render(report, self.format, options);
        let Some(path) = &self.path else {
            return Ok(Some(text));
        };
        crate::reporter::write_file(path, &text)?;
        Ok(None)
    }
}

/// At most one of `targets` may go to stdout; `Err` names them otherwise.
pub fn one_stdout(targets: &[ReportTarget]) -> Result<(), String> {
    let to_stdout: Vec<&str> = targets
        .iter()
        .filter(|t| t.path.is_none())
        .map(|t| t.format.as_str())
        .collect();
    if to_stdout.len() <= 1 {
        return Ok(());
    }
    Err(format!(
        "only one report can go to stdout, got {}; give the others a path, as in html=out/report.html",
        to_stdout.join(", ")
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn targets_parse_with_and_without_a_path() {
        let t: ReportTarget = "html=out/r.html".parse().unwrap();
        assert_eq!(t.format, RenderFormat::Html);
        assert_eq!(t.path.as_deref(), Some(Path::new("out/r.html")));
        let t: ReportTarget = "sarif".parse().unwrap();
        assert_eq!((t.format, t.path), (RenderFormat::Sarif, None));
        let t: ReportTarget = r"junit=C:\out\junit.xml".parse().unwrap();
        assert_eq!(t.path.as_deref(), Some(Path::new(r"C:\out\junit.xml")));
        assert!("xml".parse::<ReportTarget>().unwrap_err().contains("'xml'"));
        assert!("json=".parse::<ReportTarget>().is_err());
    }

    #[test]
    fn only_one_target_may_use_stdout() {
        let targets: Vec<ReportTarget> = ["json", "html=r.html", "sarif"]
            .iter()
            .map(|t| t.parse().unwrap())
            .collect();
        assert!(one_stdout(&targets[..2]).is_ok());
        assert!(one_stdout(&targets).unwrap_err().contains("json, sarif"));
    }

    #[test]
    fn document_uris_are_relative_with_slashes() {
        let base = std::env::temp_dir();
        let options = RenderOptions {
            base: Some(base.clone()),
            ..RenderOptions::default()
        };
        let spec = base.join("specs").join("api.yaml");
        assert_eq!(
            options.document_uri(&spec.to_string_lossy()),
            "specs/api.yaml"
        );
        assert_eq!(options.document_uri("api.yaml"), "api.yaml");
        assert_eq!(
            options.document_uri("https://h/openapi.json"),
            "https://h/openapi.json"
        );
    }
}
