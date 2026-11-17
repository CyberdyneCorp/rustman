//! `--reporter <format>[:<path>]`: which reports a run writes, and where.

use std::path::{Path, PathBuf};
use std::str::FromStr;

use rustman_core::history::new_ulid;
use rustman_core::run::RunRecord;

/// A report format.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Format {
    Pretty,
    Json,
    Junit,
    Tap,
}

impl Format {
    /// The report of `run` in this format, without color.
    pub fn render(self, run: &RunRecord) -> String {
        match self {
            Self::Pretty => crate::pretty(run),
            Self::Json => crate::json(run),
            Self::Junit => crate::junit(run),
            Self::Tap => crate::tap(run),
        }
    }

    /// The report for a terminal: `pretty` gets ANSI colors (for a writer
    /// that strips them when color is off); other formats are unchanged.
    pub fn render_styled(self, run: &RunRecord) -> String {
        match self {
            Self::Pretty => crate::pretty_styled(run),
            _ => self.render(run),
        }
    }
}

impl std::fmt::Display for Format {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Pretty => "pretty",
            Self::Json => "json",
            Self::Junit => "junit",
            Self::Tap => "tap",
        })
    }
}

/// One `--reporter` value: a format, written to `path` or to stdout.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Reporter {
    pub format: Format,
    pub path: Option<PathBuf>,
}

/// A `--reporter` value that names no known format.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error(
    "unknown reporter '{0}' (expected pretty, json, junit or tap, optionally as <name>:<path>)"
)]
pub struct ReporterError(pub String);

impl FromStr for Reporter {
    type Err = ReporterError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        let (name, path) = match value.split_once(':') {
            Some((name, path)) if !path.is_empty() => (name, Some(PathBuf::from(path))),
            Some(_) => return Err(ReporterError(value.to_owned())),
            None => (value, None),
        };
        let format = match name {
            "pretty" => Format::Pretty,
            "json" => Format::Json,
            "junit" => Format::Junit,
            "tap" => Format::Tap,
            _ => return Err(ReporterError(value.to_owned())),
        };
        Ok(Self { format, path })
    }
}

impl Reporter {
    /// Write the report of `run` to the reporter's file, creating its folder.
    /// Reporters without a path return the text for stdout instead, styled
    /// (see [`Format::render_styled`]).
    pub fn write(&self, run: &RunRecord) -> std::io::Result<Option<String>> {
        let Some(path) = &self.path else {
            return Ok(Some(self.format.render_styled(run)));
        };
        let text = self.format.render(run);
        if let Some(folder) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
            std::fs::create_dir_all(folder)?;
        }
        write_report(path, &text)?;
        Ok(None)
    }
}

/// Write a report to `path`. A device or pipe (`/dev/stdout`) is written in
/// place; a symlink is followed, so the file it points to gets the report;
/// a regular file is replaced atomically.
fn write_report(path: &Path, text: &str) -> std::io::Result<()> {
    match std::fs::metadata(path) {
        Ok(meta) if !meta.is_file() => std::fs::write(path, text),
        Ok(_) => write_atomic(&std::fs::canonicalize(path)?, text),
        // A dangling symlink: write through it, creating its target.
        Err(_) if path.is_symlink() => std::fs::write(path, text),
        Err(_) => write_atomic(path, text),
    }
}

/// Replace `path` with `text` through a temporary file, so an interrupted
/// write never leaves half a report. The temporary name is unique to this
/// write (process id and a ULID), so parallel runs writing the same report
/// never share one.
fn write_atomic(path: &Path, text: &str) -> std::io::Result<()> {
    let name = path.file_name().map(|n| n.to_string_lossy().into_owned());
    let temp = path.with_file_name(format!(
        ".{}.{}-{}.tmp",
        name.unwrap_or_default(),
        std::process::id(),
        new_ulid()
    ));
    std::fs::write(&temp, text)?;
    let renamed = rename_retrying(&temp, path);
    if renamed.is_err() {
        let _ = std::fs::remove_file(&temp);
    }
    renamed
}

/// Rename `from` to `to`. On Windows, a target another process has open
/// (a viewer, an antivirus scan) refuses the rename for a moment: retry
/// briefly before giving up.
fn rename_retrying(from: &Path, to: &Path) -> std::io::Result<()> {
    const ATTEMPTS: u32 = if cfg!(windows) { 20 } else { 1 };
    let mut attempt = 1;
    loop {
        match std::fs::rename(from, to) {
            Err(e) if attempt < ATTEMPTS && in_use(&e) => {
                attempt += 1;
                std::thread::sleep(std::time::Duration::from_millis(50));
            }
            result => return result,
        }
    }
}

/// Whether `e` means the file is in use by another process.
fn in_use(e: &std::io::Error) -> bool {
    const ERROR_SHARING_VIOLATION: i32 = 32;
    e.kind() == std::io::ErrorKind::PermissionDenied
        || e.raw_os_error() == Some(ERROR_SHARING_VIOLATION)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn atomic_writes_use_unique_temporary_names_and_leave_none() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("r.json");
        std::thread::scope(|scope| {
            for i in 0..8 {
                let path = &path;
                scope.spawn(move || write_atomic(path, &format!("report {i}")).unwrap());
            }
        });
        assert!(
            std::fs::read_to_string(&path)
                .unwrap()
                .starts_with("report ")
        );
        let names: Vec<String> = std::fs::read_dir(dir.path())
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(names, ["r.json"]);
        assert!(in_use(&std::io::Error::from(
            std::io::ErrorKind::PermissionDenied
        )));
        assert!(!in_use(&std::io::Error::from(std::io::ErrorKind::NotFound)));
    }

    #[test]
    fn reporters_parse_with_and_without_a_path() {
        assert_eq!(
            "json".parse::<Reporter>().unwrap(),
            Reporter {
                format: Format::Json,
                path: None
            }
        );
        let reporter: Reporter = "json:out/report.json".parse().unwrap();
        assert_eq!(reporter.path.as_deref(), Some(Path::new("out/report.json")));
        assert_eq!("pretty".parse::<Reporter>().unwrap().format, Format::Pretty);
        assert_eq!("junit".parse::<Reporter>().unwrap().format, Format::Junit);
        assert_eq!("tap".parse::<Reporter>().unwrap().format, Format::Tap);
        // A Windows drive letter stays in the path.
        let reporter: Reporter = r"junit:C:\out\junit.xml".parse().unwrap();
        assert_eq!(
            reporter.path.as_deref(),
            Some(Path::new(r"C:\out\junit.xml"))
        );
    }

    #[test]
    fn unknown_reporters_are_rejected() {
        let err = "xml".parse::<Reporter>().unwrap_err();
        assert_eq!(
            err.to_string(),
            "unknown reporter 'xml' (expected pretty, json, junit or tap, optionally as <name>:<path>)"
        );
        assert!("json:".parse::<Reporter>().is_err());
    }
}
