//! `contracts/allowlist.yaml`: accepted differences, each with a
//! justification and an optional expiry date (contract-testing "Allowlist
//! with justification", "Expired allowlist entries").
//!
//! ```yaml
//! - rule: operation-removed
//!   operation: DELETE /users/{id}
//!   justification: Deprecated since 2026-06, no traffic
//!   expires: 2026-12-31
//! ```
//!
//! The file is read through `rustman_core::format`, so every error names its
//! line: `contracts/allowlist.yaml:3: justification is required`.

use std::fmt;
use std::path::{Path, PathBuf};

use rustman_core::format::{Located, from_yaml};
use rustman_core::{LoadError, Position};
use serde::Deserialize;
use time::{Date, Month};

use crate::catalog::RuleId;
use crate::difference::Difference;
use crate::input::{MAX_INPUT_BYTES, read_text};

/// Where the allowlist is, relative to the workspace (or current folder).
pub const ALLOWLIST_FILE: &str = "contracts/allowlist.yaml";

/// A calendar date, `YYYY-MM-DD`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Day(Date);

impl Day {
    /// Today, in UTC.
    pub fn today() -> Day {
        Day(time::OffsetDateTime::now_utc().date())
    }

    pub fn parse(text: &str) -> Option<Day> {
        let mut parts = text.splitn(3, '-');
        let year: i32 = parts.next()?.parse().ok()?;
        let month: u8 = parts.next()?.parse().ok()?;
        let day = parts.next()?;
        if text.len() != 10 || day.len() != 2 {
            return None;
        }
        let month = Month::try_from(month).ok()?;
        Date::from_calendar_date(year, month, day.parse().ok()?)
            .ok()
            .map(Day)
    }
}

impl fmt::Display for Day {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let d = self.0;
        write!(
            f,
            "{:04}-{:02}-{:02}",
            d.year(),
            u8::from(d.month()),
            d.day()
        )
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawEntry {
    rule: Option<Located<String>>,
    operation: Option<String>,
    pointer: Option<String>,
    justification: Option<String>,
    expires: Option<Located<String>>,
}

/// One accepted difference.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    /// The entry's line in the file.
    pub line: usize,
    pub rule: RuleId,
    /// `METHOD /path`, as the diff reports it.
    pub operation: String,
    /// A JSON pointer into either document, to accept one element only.
    pub pointer: Option<String>,
    pub justification: String,
    pub expires: Option<Day>,
}

impl Entry {
    fn matches(&self, d: &Difference) -> bool {
        self.rule == d.rule
            && same_operation(&self.operation, &d.operation)
            && self
                .pointer
                .as_deref()
                .is_none_or(|p| p == d.old_pointer || p == d.new_pointer)
    }

    fn expired(&self, today: Day) -> bool {
        self.expires.is_some_and(|day| day < today)
    }
}

/// The method compared without case, the path as written.
fn same_operation(a: &str, b: &str) -> bool {
    match (a.trim().split_once(' '), b.split_once(' ')) {
        (Some((ma, pa)), Some((mb, pb))) => ma.eq_ignore_ascii_case(mb) && pa.trim() == pb,
        _ => false,
    }
}

/// The parsed `contracts/allowlist.yaml`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Allowlist {
    /// How the file is named in messages.
    pub file: PathBuf,
    pub entries: Vec<Entry>,
}

impl Allowlist {
    /// Read `path`; an absent file is an empty allowlist. `shown` names the
    /// file in messages. Like Pact files and run reports, the file is read
    /// through [`read_text`] and refused above [`MAX_INPUT_BYTES`].
    pub fn read(path: &Path, shown: &Path) -> Result<Allowlist, Vec<LoadError>> {
        Allowlist::read_limited(path, shown, MAX_INPUT_BYTES)
    }

    fn read_limited(path: &Path, shown: &Path, limit: u64) -> Result<Allowlist, Vec<LoadError>> {
        match read_text(path, limit) {
            Ok(text) => Allowlist::parse(shown, &text),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Allowlist {
                file: shown.to_path_buf(),
                entries: Vec::new(),
            }),
            Err(e) => Err(vec![LoadError::new(shown, None, e.to_string())]),
        }
    }

    /// Parse the text of `file`. Every invalid entry is reported, each with
    /// its line.
    pub fn parse(file: &Path, text: &str) -> Result<Allowlist, Vec<LoadError>> {
        let raw: Option<Vec<Located<RawEntry>>> = if text.trim().is_empty() {
            None
        } else {
            from_yaml(file, text).map_err(|e| vec![e])?
        };
        let mut entries = Vec::new();
        let mut errors = Vec::new();
        for raw in raw.unwrap_or_default() {
            match entry(file, raw.position(), raw.into_inner()) {
                Ok(entry) => entries.push(entry),
                Err(error) => errors.push(error),
            }
        }
        if errors.is_empty() {
            Ok(Allowlist {
                file: file.to_path_buf(),
                entries,
            })
        } else {
            Err(errors)
        }
    }

    /// Which differences are allowed, and the warnings: expired entries
    /// (which allow nothing) and unused ones.
    pub fn apply(&self, differences: &[Difference], today: Day) -> Applied {
        let mut allowed = vec![None; differences.len()];
        let mut warnings = Vec::new();
        for entry in &self.entries {
            let at = format!("{}:{}", self.file.display(), entry.line);
            if let Some(day) = entry.expires.filter(|_| entry.expired(today)) {
                warnings.push(format!("{at}: allowlist entry expired on {day}"));
                continue;
            }
            let mut used = false;
            for (slot, d) in allowed.iter_mut().zip(differences) {
                if entry.matches(d) {
                    used = true;
                    slot.get_or_insert_with(|| entry.justification.clone());
                }
            }
            if !used {
                warnings.push(format!(
                    "{at}: unused allowlist entry ({} {})",
                    entry.rule, entry.operation
                ));
            }
        }
        Applied { allowed, warnings }
    }
}

/// What the allowlist decided for one diff.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Applied {
    /// Per difference, in order: the justification when it is allowed.
    pub allowed: Vec<Option<String>>,
    pub warnings: Vec<String>,
}

fn entry(file: &Path, position: Option<Position>, raw: RawEntry) -> Result<Entry, LoadError> {
    let line = position.map_or(0, |p| p.line);
    let error = |at: Option<Position>, message: String| LoadError::new(file, at, message);
    let rule = raw
        .rule
        .ok_or_else(|| error(position, "rule is required".into()))?;
    let rule_id = RuleId::from_id(&rule).ok_or_else(|| {
        let message = format!("unknown rule '{}' (see rustman contract rules)", *rule);
        error(rule.position().or(position), message)
    })?;
    let operation = raw
        .operation
        .filter(|o| o.trim().contains(' '))
        .ok_or_else(|| error(position, "operation is required, as in 'GET /users'".into()))?;
    let justification = raw
        .justification
        .filter(|j| !j.trim().is_empty())
        .ok_or_else(|| error(position, "justification is required".into()))?;
    let expires = match raw.expires {
        None => None,
        Some(text) => Some(Day::parse(&text).ok_or_else(|| {
            let message = format!("expires '{}' is not a date (YYYY-MM-DD)", *text);
            error(text.position().or(position), message)
        })?),
    };
    Ok(Entry {
        line,
        rule: rule_id,
        operation: operation.trim().to_owned(),
        pointer: raw.pointer,
        justification,
        expires,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(text: &str) -> Result<Allowlist, Vec<LoadError>> {
        Allowlist::parse(Path::new(ALLOWLIST_FILE), text)
    }

    fn removal() -> Difference {
        let rule = RuleId::OperationRemoved;
        Difference {
            rule,
            classification: rule.classification(),
            operation: "DELETE /users/{id}".into(),
            message: "operation removed".into(),
            old_pointer: "/paths/~1users~1{id}/delete".into(),
            new_pointer: "/paths".into(),
        }
    }

    fn day(text: &str) -> Day {
        Day::parse(text).unwrap()
    }

    #[test]
    fn days_parse_and_print() {
        assert_eq!(day("2026-01-01").to_string(), "2026-01-01");
        assert!(day("2026-01-01") < day("2026-11-17"));
        for bad in ["2026-13-01", "2026-02-30", "26-1-1", "2026-01-1", "x"] {
            assert!(Day::parse(bad).is_none(), "{bad}");
        }
    }

    #[test]
    fn an_entry_allows_its_difference() {
        let list = parse(
            "- rule: operation-removed\n  operation: \"delete /users/{id}\"\n  \
             justification: Deprecated since 2026-06, no traffic\n",
        )
        .unwrap();
        let applied = list.apply(&[removal()], day("2026-11-17"));
        assert_eq!(
            applied.allowed,
            vec![Some("Deprecated since 2026-06, no traffic".into())]
        );
        assert!(applied.warnings.is_empty());
    }

    #[test]
    fn a_pointer_narrows_the_entry() {
        let text = "- rule: operation-removed\n  operation: DELETE /users/{id}\n  \
                    pointer: /paths/~1other\n  justification: x\n";
        let applied = parse(text).unwrap().apply(&[removal()], day("2026-11-17"));
        assert_eq!(applied.allowed, vec![None]);
        assert_eq!(
            applied.warnings,
            vec![
                "contracts/allowlist.yaml:1: unused allowlist entry (operation-removed DELETE /users/{id})"
            ]
        );
    }

    #[test]
    fn a_missing_justification_names_its_line() {
        let text = "- rule: operation-removed\n  operation: DELETE /a\n  justification: ok\n\
                    - rule: operation-removed\n  operation: DELETE /b\n";
        let errors = parse(text).unwrap_err();
        let shown: Vec<String> = errors.iter().map(ToString::to_string).collect();
        assert_eq!(
            shown,
            vec!["contracts/allowlist.yaml:4: justification is required"]
        );
        let blank =
            parse("- rule: operation-removed\n  operation: DELETE /a\n  justification: ' '\n");
        assert_eq!(
            blank.unwrap_err()[0].to_string(),
            "contracts/allowlist.yaml:1: justification is required"
        );
    }

    #[test]
    fn invalid_entries_are_reported_at_their_value() {
        let errors = parse(
            "- rule: no-such-rule\n  operation: GET /a\n  justification: x\n\
             - rule: operation-removed\n  operation: GET /a\n  justification: x\n  expires: soon\n\
             - rule: operation-removed\n  justification: x\n\
             - {operation: GET /a, justification: x}\n",
        )
        .unwrap_err();
        let shown: Vec<String> = errors.iter().map(ToString::to_string).collect();
        assert_eq!(
            shown,
            vec![
                "contracts/allowlist.yaml:1: unknown rule 'no-such-rule' (see rustman contract rules)",
                "contracts/allowlist.yaml:7: expires 'soon' is not a date (YYYY-MM-DD)",
                "contracts/allowlist.yaml:8: operation is required, as in 'GET /users'",
                "contracts/allowlist.yaml:10: rule is required",
            ]
        );
        let unknown = parse("- rule: operation-removed\n  because: x\n").unwrap_err();
        assert!(
            unknown[0]
                .to_string()
                .starts_with("contracts/allowlist.yaml:2: unknown field")
        );
    }

    #[test]
    fn an_expired_entry_allows_nothing_and_warns() {
        let text = "- rule: operation-removed\n  operation: DELETE /users/{id}\n  \
                    justification: x\n  expires: 2026-01-01\n";
        let list = parse(text).unwrap();
        let applied = list.apply(&[removal()], day("2026-11-17"));
        assert_eq!(applied.allowed, vec![None]);
        assert_eq!(
            applied.warnings,
            vec!["contracts/allowlist.yaml:1: allowlist entry expired on 2026-01-01"]
        );
        let applied = list.apply(&[removal()], day("2026-01-01"));
        assert!(applied.allowed[0].is_some(), "valid through its expiry day");
    }

    #[test]
    fn an_empty_or_absent_file_allows_nothing() {
        assert!(parse("").unwrap().entries.is_empty());
        assert!(parse("[]\n").unwrap().entries.is_empty());
        let dir = std::env::temp_dir().join("rustman-no-such-allowlist");
        let list = Allowlist::read(&dir.join("allowlist.yaml"), Path::new(ALLOWLIST_FILE)).unwrap();
        assert!(list.entries.is_empty());
    }

    /// Regression: the allowlist was read with an unbounded
    /// `read_to_string`, unlike every other input of the contract commands.
    #[test]
    fn an_allowlist_over_the_size_limit_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("allowlist.yaml");
        let text = "- rule: operation-removed\n  operation: DELETE /a\n  justification: x\n";
        std::fs::write(&path, text).unwrap();
        let shown = Path::new(ALLOWLIST_FILE);
        let limit = text.len() as u64;
        assert_eq!(
            Allowlist::read_limited(&path, shown, limit)
                .unwrap()
                .entries
                .len(),
            1
        );
        let errors = Allowlist::read_limited(&path, shown, limit - 1).unwrap_err();
        assert_eq!(
            errors[0].to_string(),
            format!(
                "contracts/allowlist.yaml: larger than the {}-byte limit",
                limit - 1
            )
        );
    }
}
