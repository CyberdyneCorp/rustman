//! The three-way merge at field granularity (design D3): the request
//! generated from the snapshot is the base, the request file is local, the
//! request generated from the new document is remote. A field takes the
//! remote value only when the user has not changed it; keyed lists
//! (query, headers, tags) merge row by row; fields the import never
//! generates (settings, variables, assertions, captures) are never touched.

use rustman_core::model::{HttpRequest, NameValue};

/// The resolution recorded for a field changed on both sides.
pub const KEPT_LOCAL: &str = "kept local";

/// How one field merged.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Outcome {
    /// Nothing to do: the remote value is already there, or did not change.
    Same,
    /// The remote value was taken.
    Updated,
    /// Both sides changed it; the local value was kept.
    KeptLocal,
}

impl Outcome {
    /// The combined outcome of a field made of several merged parts.
    fn and(self, other: Self) -> Self {
        match (self, other) {
            (Self::KeptLocal, _) | (_, Self::KeptLocal) => Self::KeptLocal,
            (Self::Updated, _) | (_, Self::Updated) => Self::Updated,
            _ => Self::Same,
        }
    }
}

/// Merge one value: take `remote` only when `local` still equals `base`.
pub fn merge_value<T: Clone + PartialEq>(base: Option<&T>, local: &T, remote: &T) -> (T, Outcome) {
    if local == remote || base == Some(remote) {
        return (local.clone(), Outcome::Same);
    }
    if base == Some(local) {
        return (remote.clone(), Outcome::Updated);
    }
    (local.clone(), Outcome::KeptLocal)
}

/// Merge keyed rows. Rows only the user has (not in `base`) always stay;
/// rows the user removed stay removed; rows removed upstream go unless the
/// user changed them; new upstream rows are appended.
pub fn merge_rows<T, K>(base: &[T], local: &[T], remote: &[T], key: K) -> (Vec<T>, Outcome)
where
    T: Clone + PartialEq,
    K: Fn(&T) -> String,
{
    let find = |rows: &'_ [T], k: &str| rows.iter().find(|r| key(r) == k).cloned();
    let mut out = Vec::with_capacity(local.len());
    let mut outcome = Outcome::Same;
    for row in local {
        let k = key(row);
        let (kept, row_outcome) =
            merge_row(find(base, &k).as_ref(), row, find(remote, &k).as_ref());
        outcome = outcome.and(row_outcome);
        out.extend(kept);
    }
    for row in remote {
        let k = key(row);
        if local.iter().any(|l| key(l) == k) {
            continue;
        }
        match find(base, &k) {
            None => {
                out.push(row.clone());
                outcome = outcome.and(Outcome::Updated);
            }
            Some(b) if b != *row => outcome = outcome.and(Outcome::KeptLocal),
            Some(_) => {}
        }
    }
    (out, outcome)
}

/// One local row against its base and remote counterparts.
fn merge_row<T: Clone + PartialEq>(
    base: Option<&T>,
    local: &T,
    remote: Option<&T>,
) -> (Option<T>, Outcome) {
    match (base, remote) {
        (None, None) => (Some(local.clone()), Outcome::Same),
        (Some(b), None) if b == local => (None, Outcome::Updated),
        (Some(_), None) => (Some(local.clone()), Outcome::KeptLocal),
        (base, Some(r)) => {
            let (value, outcome) = merge_value(base, local, r);
            (Some(value), outcome)
        }
    }
}

/// [`merge_rows`] over optional lists; an empty result keeps the local
/// shape (`None` stays `None`).
fn merge_optional_rows<T, K>(
    base: Option<&Vec<T>>,
    local: &Option<Vec<T>>,
    remote: &Option<Vec<T>>,
    key: K,
) -> (Option<Vec<T>>, Outcome)
where
    T: Clone + PartialEq,
    K: Fn(&T) -> String,
{
    let empty = Vec::new();
    let (rows, outcome) = merge_rows(
        base.unwrap_or(&empty),
        local.as_ref().unwrap_or(&empty),
        remote.as_ref().unwrap_or(&empty),
        key,
    );
    let rows = if rows.is_empty() && local.is_none() {
        None
    } else {
        Some(rows)
    };
    (rows, outcome)
}

/// A merged request and what happened to each field.
#[derive(Clone, Debug, PartialEq)]
pub struct MergedRequest {
    pub request: HttpRequest,
    /// Fields that took the remote value.
    pub updated: Vec<&'static str>,
    /// Fields changed on both sides, where the local value was kept.
    pub kept_local: Vec<&'static str>,
}

impl MergedRequest {
    fn record(&mut self, field: &'static str, outcome: Outcome) {
        match outcome {
            Outcome::Same => {}
            Outcome::Updated => self.updated.push(field),
            Outcome::KeptLocal => self.kept_local.push(field),
        }
    }
}

fn header_key(row: &NameValue) -> String {
    row.name.to_ascii_lowercase()
}

fn query_key(row: &NameValue) -> String {
    row.name.clone()
}

/// Merge a request file (`local`) with the requests generated for the same
/// operation from the snapshot (`base`, absent for an operation the
/// snapshot did not have) and from the new document (`remote`).
pub fn merge_request(
    base: Option<&HttpRequest>,
    local: &HttpRequest,
    remote: &HttpRequest,
) -> MergedRequest {
    let mut merged = MergedRequest {
        request: local.clone(),
        updated: Vec::new(),
        kept_local: Vec::new(),
    };
    let out = &mut merged.request;
    let mut outcomes = Vec::new();
    macro_rules! value {
        ($field:ident) => {{
            let (value, outcome) =
                merge_value(base.map(|b| &b.$field), &local.$field, &remote.$field);
            out.$field = value;
            outcomes.push((stringify!($field), outcome));
        }};
    }
    macro_rules! rows {
        ($field:ident, $key:expr) => {{
            let base_rows = base.and_then(|b| b.$field.as_ref());
            let (value, outcome) =
                merge_optional_rows(base_rows, &local.$field, &remote.$field, $key);
            out.$field = value;
            outcomes.push((stringify!($field), outcome));
        }};
    }
    value!(name);
    value!(description);
    rows!(tags, |t: &String| t.clone());
    value!(method);
    value!(url);
    rows!(query, query_key);
    rows!(headers, header_key);
    value!(body);
    value!(auth);
    for (field, outcome) in outcomes {
        merged.record(field, outcome);
    }
    merged
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn values_follow_the_three_way_rules() {
        assert_eq!(merge_value(Some(&1), &1, &2), (2, Outcome::Updated));
        assert_eq!(merge_value(Some(&1), &3, &1), (3, Outcome::Same));
        assert_eq!(merge_value(Some(&1), &3, &3), (3, Outcome::Same));
        assert_eq!(merge_value(Some(&1), &3, &2), (3, Outcome::KeptLocal));
        assert_eq!(merge_value(None, &3, &2), (3, Outcome::KeptLocal));
    }

    fn rows(pairs: &[(&str, &str)]) -> Vec<(String, String)> {
        pairs
            .iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
            .collect()
    }

    #[test]
    fn rows_merge_by_key() {
        let key = |r: &(String, String)| r.0.clone();
        let base = rows(&[("a", "1"), ("b", "1"), ("gone", "1"), ("deleted", "1")]);
        let local = rows(&[("mine", "x"), ("a", "1"), ("b", "edited"), ("gone", "1")]);
        let remote = rows(&[("a", "2"), ("b", "2"), ("deleted", "1"), ("new", "1")]);
        let (merged, outcome) = merge_rows(&base, &local, &remote, key);
        assert_eq!(
            merged,
            rows(&[("mine", "x"), ("a", "2"), ("b", "edited"), ("new", "1")])
        );
        assert_eq!(outcome, Outcome::KeptLocal);
    }
}
