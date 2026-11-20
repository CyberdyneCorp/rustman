//! Finding minimization (design D9, stress-testing "Finding
//! minimization"): once load has ended and the target has recovered, the
//! input behind each crash-like finding (a 5xx or timeout caused by a
//! variant) is replayed alone and reduced by delta debugging while the
//! symptom still reproduces: keys and items are removed, nesting is
//! unwrapped and strings are halved (never below one character, since an
//! empty string is a different input). At most [`MAX_REPLAYS`] replays
//! per finding, paced by the caller at 2 req/s.
//!
//! A finding whose input does not reproduce alone is `load-dependent`.
//!
//! The reducer only sees a predicate, so tests drive it without a server.

use std::future::Future;

use serde_json::{Map, Value};

/// The most replays per finding, the first (does it reproduce alone?)
/// included.
pub const MAX_REPLAYS: u32 = 50;

/// What minimizing one input found.
#[derive(Clone, Debug, PartialEq)]
pub struct Reduced {
    /// The smallest input that still reproduced (the original when nothing
    /// smaller did).
    pub value: Value,
    /// Replays sent, the first included.
    pub replays: u32,
    /// The original did not reproduce alone.
    pub load_dependent: bool,
}

/// Reduce `original` while `reproduces` holds, with at most `budget`
/// calls. The first call replays `original` itself: when it does not
/// reproduce, the input is load-dependent and nothing more is sent.
pub async fn reduce<F, Fut>(original: &Value, budget: u32, mut reproduces: F) -> Reduced
where
    F: FnMut(Value) -> Fut,
    Fut: Future<Output = bool>,
{
    let mut reduced = Reduced {
        value: original.clone(),
        replays: 0,
        load_dependent: false,
    };
    if budget == 0 {
        return reduced;
    }
    reduced.replays = 1;
    if !reproduces(original.clone()).await {
        reduced.load_dependent = true;
        return reduced;
    }
    'shrink: while reduced.replays < budget {
        for candidate in smaller(&reduced.value) {
            if reduced.replays >= budget {
                break 'shrink;
            }
            reduced.replays += 1;
            if reproduces(candidate.clone()).await {
                reduced.value = candidate;
                continue 'shrink;
            }
        }
        break;
    }
    reduced
}

/// Every one-step reduction of `value`, coarsest first: at each level,
/// removals, then unwrapping, then the same inside each child.
pub fn smaller(value: &Value) -> Vec<Value> {
    match value {
        Value::Object(map) => object(map),
        Value::Array(items) => array(items),
        Value::String(text) => halve(text).map(Value::String).into_iter().collect(),
        _ => Vec::new(),
    }
}

fn object(map: &Map<String, Value>) -> Vec<Value> {
    let mut out: Vec<Value> = map
        .keys()
        .map(|key| {
            let mut fewer = map.clone();
            fewer.remove(key);
            Value::Object(fewer)
        })
        .collect();
    out.extend(unwrap(map.values()));
    for (key, child) in map {
        for variant in smaller(child) {
            let mut changed = map.clone();
            changed.insert(key.clone(), variant);
            out.push(Value::Object(changed));
        }
    }
    out
}

fn array(items: &[Value]) -> Vec<Value> {
    let mut out: Vec<Value> = (0..items.len())
        .map(|i| {
            let mut fewer = items.to_vec();
            fewer.remove(i);
            Value::Array(fewer)
        })
        .collect();
    out.extend(unwrap(items.iter()));
    for (i, child) in items.iter().enumerate() {
        for variant in smaller(child) {
            let mut changed = items.to_vec();
            changed[i] = variant;
            out.push(Value::Array(changed));
        }
    }
    out
}

/// A container with a single child that is itself a container: the
/// child alone (one level of nesting less).
fn unwrap<'a>(mut children: impl ExactSizeIterator<Item = &'a Value>) -> Option<Value> {
    if children.len() != 1 {
        return None;
    }
    children
        .next()
        .filter(|c| c.is_object() || c.is_array())
        .cloned()
}

/// The first half of `text`, at least one character.
fn halve(text: &str) -> Option<String> {
    let chars = text.chars().count();
    (chars > 1).then(|| {
        text.chars()
            .take(chars.div_ceil(2).min(chars - 1))
            .collect()
    })
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;

    use serde_json::json;

    use super::*;

    /// The stub's bug: `qty` is a string.
    fn qty_is_a_string(value: &Value) -> bool {
        value.get("qty").is_some_and(Value::is_string)
    }

    async fn minimize(original: &Value, budget: u32, bug: fn(&Value) -> bool) -> Reduced {
        reduce(original, budget, |v| async move { bug(&v) }).await
    }

    #[tokio::test]
    async fn a_string_quantity_minimizes_to_that_key_alone() {
        let original = json!({
            "qty": "2",
            "sku": "A1",
            "note": "x".repeat(4000),
            "lines": [{"sku": "B2", "qty": 1}, {"sku": "C3", "qty": 2}],
        });
        let reduced = minimize(&original, MAX_REPLAYS, qty_is_a_string).await;
        assert_eq!(reduced.value, json!({"qty": "2"}));
        assert!(!reduced.load_dependent);
        assert!(reduced.replays <= MAX_REPLAYS);
    }

    #[tokio::test]
    async fn an_input_that_does_not_reproduce_alone_is_load_dependent() {
        let original = json!({"qty": "2"});
        let calls = Cell::new(0);
        let reduced = reduce(&original, MAX_REPLAYS, |_| {
            calls.set(calls.get() + 1);
            async { false }
        })
        .await;
        assert!(reduced.load_dependent);
        assert_eq!(reduced.value, original);
        assert_eq!((reduced.replays, calls.get()), (1, 1));
    }

    #[tokio::test]
    async fn the_budget_bounds_the_replays() {
        let original = Value::Array((0..200).map(Value::from).collect());
        let calls = Cell::new(0);
        let reduced = reduce(&original, 10, |v| {
            calls.set(calls.get() + 1);
            async move { !v.as_array().is_some_and(Vec::is_empty) }
        })
        .await;
        assert_eq!((reduced.replays, calls.get()), (10, 10));
        assert_eq!(reduced.value.as_array().unwrap().len(), 191);
    }

    #[tokio::test]
    async fn nesting_is_unwrapped_and_strings_halved() {
        fn deep_text(value: &Value) -> bool {
            let mut v = value;
            while let Some(inner) = v.get(0).or_else(|| v.get("a")) {
                v = inner;
            }
            v.as_str().is_some_and(|s| s.starts_with('x'))
        }
        let original = json!([[{"a": ["xyzxyzxyz"]}]]);
        let reduced = minimize(&original, MAX_REPLAYS, deep_text).await;
        assert_eq!(reduced.value, json!(["x"]));
    }

    #[test]
    fn every_reduction_is_smaller_in_one_place() {
        let value = json!({"a": 1, "b": "hello", "c": [true, null]});
        let all = smaller(&value);
        assert!(all.contains(&json!({"b": "hello", "c": [true, null]})));
        assert!(all.contains(&json!({"a": 1, "b": "hel", "c": [true, null]})));
        assert!(all.contains(&json!({"a": 1, "b": "hello", "c": [null]})));
        assert!(!all.contains(&value));
        assert_eq!(halve("ab"), Some("a".into()));
        assert_eq!(halve("é"), None);
        assert!(smaller(&json!(42)).is_empty());
    }
}
