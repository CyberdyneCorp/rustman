//! The `snapshot` assertion (snapshot-testing spec): `- snapshot` or
//! `- snapshot: {name, compare, ignore, headers, replace, sort, presets}`.
//!
//! Regular expressions and JSONPath expressions are checked while the file
//! loads, and the error points at the offending value. Names must be unique
//! within one `assertions` list ([`unique_snapshot_names`]).

use serde::de::{self, Deserializer};
use serde::{Deserialize, Serialize};

use super::assertion::Assertion;
use super::common::Validate;
use crate::format::{Located, error_at};

/// The name of a snapshot assertion without `name`.
pub const DEFAULT_SNAPSHOT_NAME: &str = "default";

/// The headers stored when `headers.include` is absent.
pub const DEFAULT_HEADER_INCLUDE: [&str; 1] = ["content-type"];

/// A `snapshot` assertion. Every field is optional; [`SnapshotAssertion::default`]
/// is the shorthand `- snapshot`.
#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SnapshotAssertion {
    pub name: Option<Located<String>>,
    pub compare: Option<Vec<ComparedPart>>,
    /// JSONPath expressions whose values become `«ignored»`.
    pub ignore: Option<Vec<Located<String>>>,
    pub headers: Option<SnapshotHeaders>,
    pub replace: Option<Vec<Replacement>>,
    pub sort: Option<Vec<SortRule>>,
    /// `None` means the defaults ([`DEFAULT_PRESETS`]); `[]` disables them.
    pub presets: Option<Vec<Preset>>,
}

/// A part of the exchange a snapshot compares.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ComparedPart {
    Status,
    Headers,
    Body,
}

impl ComparedPart {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Status => "status",
            Self::Headers => "headers",
            Self::Body => "body",
        }
    }
}

/// The parts compared when `compare` is absent.
pub const DEFAULT_COMPARE: [ComparedPart; 2] = [ComparedPart::Status, ComparedPart::Body];

/// `headers: {include, redact}`.
#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SnapshotHeaders {
    /// The headers stored when headers are compared (default `content-type`).
    pub include: Option<Vec<String>>,
    /// Included headers whose value is stored as `«redacted»`.
    pub redact: Option<Vec<String>>,
}

/// `{pattern, with}`: a regex replacement.
#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Replacement {
    pub pattern: Located<String>,
    pub with: String,
}

/// `{path, by}`: sort the array at `path` by the value at `by` (a key path
/// relative to each item, such as `id` or `owner.id`), or by the whole item.
#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SortRule {
    pub path: Located<String>,
    pub by: Option<String>,
}

/// A built-in normalization preset.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Preset {
    Timestamps,
    Dates,
    Uuids,
    Etags,
    EpochMs,
    Jwt,
}

impl Preset {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Timestamps => "timestamps",
            Self::Dates => "dates",
            Self::Uuids => "uuids",
            Self::Etags => "etags",
            Self::EpochMs => "epoch_ms",
            Self::Jwt => "jwt",
        }
    }
}

/// The presets applied when `presets` is absent.
pub const DEFAULT_PRESETS: [Preset; 2] = [Preset::Timestamps, Preset::Uuids];

impl SnapshotAssertion {
    /// The snapshot's name (`default` when absent).
    pub fn name(&self) -> &str {
        self.name
            .as_deref()
            .map_or(DEFAULT_SNAPSHOT_NAME, String::as_str)
    }

    /// The compared parts, in the fixed order status, headers, body.
    pub fn compared(&self) -> Vec<ComparedPart> {
        let given = self.compare.as_deref().unwrap_or(&DEFAULT_COMPARE);
        [
            ComparedPart::Status,
            ComparedPart::Headers,
            ComparedPart::Body,
        ]
        .into_iter()
        .filter(|part| given.contains(part))
        .collect()
    }

    pub fn compares(&self, part: ComparedPart) -> bool {
        self.compared().contains(&part)
    }

    /// The stored header names, lower-cased.
    pub fn included_headers(&self) -> Vec<String> {
        let include = self.headers.as_ref().and_then(|h| h.include.as_deref());
        match include {
            Some(names) => names.iter().map(|n| n.to_ascii_lowercase()).collect(),
            None => DEFAULT_HEADER_INCLUDE.map(str::to_owned).to_vec(),
        }
    }

    /// Whether the stored value of header `name` is `«redacted»`.
    pub fn redacts_header(&self, name: &str) -> bool {
        let redact = self.headers.as_ref().and_then(|h| h.redact.as_deref());
        redact.is_some_and(|names| names.iter().any(|n| n.eq_ignore_ascii_case(name)))
    }

    /// The presets in effect.
    pub fn effective_presets(&self) -> &[Preset] {
        self.presets.as_deref().unwrap_or(&DEFAULT_PRESETS)
    }
}

impl Validate for SnapshotAssertion {
    fn validate(&self) -> Result<(), String> {
        if let Some(name) = &self.name {
            check_name(name)?;
        }
        for path in self.ignore.iter().flatten() {
            check_jsonpath(path)?;
        }
        for rule in self.sort.iter().flatten() {
            check_jsonpath(&rule.path)?;
        }
        for rule in self.replace.iter().flatten() {
            compile_regex(&rule.pattern).map_err(|e| error_at(rule.pattern.position(), e))?;
        }
        Ok(())
    }
}

/// Names become part of a file name: letters, digits, `-` and `_`.
fn check_name(name: &Located<String>) -> Result<(), String> {
    let valid = !name.is_empty()
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_'));
    if valid {
        return Ok(());
    }
    let message = format!(
        "invalid snapshot name '{}' (use letters, digits, '-' and '_')",
        name.get()
    );
    Err(error_at(name.position(), message))
}

fn check_jsonpath(path: &Located<String>) -> Result<(), String> {
    serde_json_path::JsonPath::parse(path)
        .map(|_| ())
        .map_err(|e| {
            error_at(
                path.position(),
                format!("invalid JSONPath '{}': {e}", path.get()),
            )
        })
}

/// Compile a `replace` pattern; the error reads `invalid regex '<p>': <reason>`.
pub fn compile_regex(pattern: &str) -> Result<regex::Regex, String> {
    regex::Regex::new(pattern)
        .map_err(|e| format!("invalid regex '{pattern}': {}", regex_reason(&e)))
}

/// The last line of a regex error (`unclosed group`), without its `error: `.
fn regex_reason(error: &regex::Error) -> String {
    let text = error.to_string();
    let last = text.lines().last().unwrap_or_default();
    last.strip_prefix("error: ").unwrap_or(last).to_owned()
}

/// Read an `assertions` list, refusing two snapshot assertions with the same
/// name. The error points at the second one:
/// `<path>:<line>: duplicate snapshot name 'default'`.
pub(crate) fn unique_snapshot_names<'de, D: Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<Vec<Assertion>>, D::Error> {
    let Some(items) = Option::<Vec<Located<Assertion>>>::deserialize(deserializer)? else {
        return Ok(None);
    };
    let mut seen: Vec<&str> = Vec::new();
    for item in &items {
        let Assertion::Snapshot(snapshot) = item.get() else {
            continue;
        };
        let name = snapshot.name();
        if seen.contains(&name) {
            let message = format!("duplicate snapshot name '{name}'");
            return Err(de::Error::custom(error_at(item.position(), message)));
        }
        seen.push(name);
    }
    Ok(Some(items.into_iter().map(Located::into_inner).collect()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Checked;
    use std::path::Path;

    #[derive(Debug, Deserialize)]
    #[serde(deny_unknown_fields)]
    struct File {
        #[serde(default, deserialize_with = "unique_snapshot_names")]
        assertions: Option<Vec<Assertion>>,
    }

    fn load(text: &str) -> Result<Vec<Assertion>, crate::error::LoadError> {
        crate::format::from_yaml::<File>(Path::new("users/get.rman.yaml"), text)
            .map(|f| f.assertions.unwrap_or_default())
    }

    fn snapshot(a: &Assertion) -> &SnapshotAssertion {
        match a {
            Assertion::Snapshot(s) => s,
            other => panic!("not a snapshot: {other:?}"),
        }
    }

    #[test]
    fn shorthand_is_the_default_snapshot() {
        let all = load("assertions: [snapshot]\n").unwrap();
        assert_eq!(all.len(), 1);
        let s = snapshot(&all[0]);
        assert_eq!(s, &SnapshotAssertion::default());
        assert_eq!(s.name(), "default");
        assert_eq!(s.compared(), DEFAULT_COMPARE);
        assert_eq!(s.effective_presets(), DEFAULT_PRESETS);
        assert_eq!(s.included_headers(), ["content-type"]);

        let all = load("assertions:\n  - status: 200\n  - snapshot\n").unwrap();
        assert!(matches!(all[1], Assertion::Snapshot(_)));
    }

    #[test]
    fn full_form_loads_every_field() {
        let text = r#"assertions:
  - snapshot:
      name: listing
      compare: [body, headers, status]
      ignore: ["$.id", "$.items[*].created_at"]
      headers: {include: [Content-Type, X-Version], redact: [x-version]}
      replace: [{pattern: "ORD-\\d+", with: "ORD-«n»"}]
      sort: [{path: $.users, by: id}, {path: $.tags}]
      presets: [timestamps, dates, uuids, etags, epoch_ms, jwt]
"#;
        let all = load(text).unwrap();
        let s = snapshot(&all[0]);
        assert_eq!(s.name(), "listing");
        assert_eq!(
            s.compared(),
            [
                ComparedPart::Status,
                ComparedPart::Headers,
                ComparedPart::Body
            ]
        );
        assert_eq!(s.included_headers(), ["content-type", "x-version"]);
        assert!(s.redacts_header("X-Version") && !s.redacts_header("content-type"));
        assert_eq!(s.replace.as_ref().unwrap()[0].with, "ORD-«n»");
        assert_eq!(s.sort.as_ref().unwrap()[1].by, None);
        assert_eq!(s.effective_presets().len(), 6);
        let none = load("assertions:\n  - snapshot: {presets: []}\n").unwrap();
        assert!(snapshot(&none[0]).effective_presets().is_empty());
    }

    #[test]
    fn duplicate_names_fail_at_the_second_assertion() {
        let err = load("assertions:\n  - snapshot\n  - status: 200\n  - snapshot\n").unwrap_err();
        assert_eq!(
            err.to_string(),
            "users/get.rman.yaml:4: duplicate snapshot name 'default'"
        );
        let err = load("assertions:\n  - snapshot: {name: a}\n  - snapshot:\n      name: a\n")
            .unwrap_err();
        assert_eq!(
            err.to_string(),
            "users/get.rman.yaml:3: duplicate snapshot name 'a'"
        );
        assert_eq!(
            load("assertions:\n  - snapshot\n  - snapshot: {name: other}\n")
                .unwrap()
                .len(),
            2
        );
    }

    #[test]
    fn invalid_regex_names_the_pattern_and_reason() {
        let err = load(
            "assertions:\n  - snapshot:\n      replace:\n        - {pattern: \"(\", with: x}\n",
        )
        .unwrap_err();
        assert_eq!(
            err.to_string(),
            "users/get.rman.yaml:4: invalid regex '(': unclosed group"
        );
    }

    #[test]
    fn invalid_paths_and_names_are_refused() {
        let err = load("assertions:\n  - snapshot:\n      ignore: ['$.ok', 'id']\n").unwrap_err();
        assert!(err.message.starts_with("invalid JSONPath 'id'"), "{err}");
        assert_eq!(err.line(), Some(3));
        let err = load("assertions:\n  - snapshot: {name: 'a/b'}\n").unwrap_err();
        assert!(
            err.message.starts_with("invalid snapshot name 'a/b'"),
            "{err}"
        );
        let err = load("assertions:\n  - snapshot: {compare: [cookies]}\n").unwrap_err();
        assert!(
            err.message.starts_with("unknown variant `cookies`"),
            "{err}"
        );
        let err = load("assertions:\n  - snapshott\n").unwrap_err();
        assert!(err.message.contains("snapshott"), "{err}");
    }

    #[test]
    fn written_back_in_the_form_it_was_read() {
        let text = "\
- snapshot
- snapshot:
    name: full
    compare:
      - status
      - headers
    ignore:
      - $.id
    headers:
      include:
        - content-type
      redact:
        - x-key
    replace:
      - pattern: ORD-\\d+
        with: ORD-«n»
    sort:
      - path: $.users
        by: id
    presets: []
";
        let all = load(&format!("assertions:\n{}", indent(text))).unwrap();
        assert_eq!(crate::format::write::assertions_to_yaml(&all), text);
    }

    fn indent(text: &str) -> String {
        text.lines().map(|l| format!("  {l}\n")).collect()
    }

    #[test]
    fn checked_in_code() {
        let bad = SnapshotAssertion {
            replace: Some(vec![Replacement {
                pattern: Located::new("[".into()),
                with: String::new(),
            }]),
            ..SnapshotAssertion::default()
        };
        assert!(
            Checked::new(bad)
                .unwrap_err()
                .starts_with("invalid regex '['")
        );
    }
}
