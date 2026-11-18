//! A set of secret values and their matchers. A [`super::Redactor`] is a
//! short list of layers shared through `Arc`, so cloning one (a request's
//! redactor starts from the run's) copies no values, and each layer builds
//! its matchers once, on first use, for the values it holds.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, OnceLock};

use aho_corasick::{AhoCorasick, AhoCorasickBuilder, AhoCorasickKind};

use super::MIN_SECRET_PIECE;
use super::forms::encoded_forms;
use crate::secret::placeholder;

#[derive(Default)]
pub(super) struct Layer {
    /// Every value added, with its name, in order (to rebuild a layer).
    entries: Vec<(String, String)>,
    values: HashSet<String>,
    /// Every searchable form of every value, without duplicates.
    needles: Vec<Vec<u8>>,
    forms: HashSet<Vec<u8>>,
    /// The placeholder of each needle, by index.
    placeholders: Vec<Arc<str>>,
    by_name: HashMap<String, Arc<str>>,
    /// The names, in the order they were added.
    names: Vec<String>,
    /// Built on first use, dropped when a value is added.
    matcher: OnceLock<Option<AhoCorasick>>,
    /// Every [`MIN_SECRET_PIECE`]-character window of every raw value, for
    /// taint checks only (never for output); built on first use.
    pieces: OnceLock<Option<AhoCorasick>>,
}

impl Layer {
    /// One layer holding every value of `layers`.
    pub(super) fn compacted(layers: &[Arc<Layer>]) -> Self {
        let mut layer = Self::default();
        for (name, value) in layers.iter().flat_map(|l| &l.entries) {
            layer.add(name, value);
        }
        layer
    }

    pub(super) fn has_value(&self, value: &str) -> bool {
        self.values.contains(value)
    }

    pub(super) fn names(&self) -> &[String] {
        &self.names
    }

    /// Add `value` under `name` unless this layer has it already.
    pub(super) fn add(&mut self, name: &str, value: &str) {
        if !self.values.insert(value.to_owned()) {
            return;
        }
        self.entries.push((name.to_owned(), value.to_owned()));
        if !self.by_name.contains_key(name) {
            self.names.push(name.to_owned());
        }
        let placeholder = self
            .by_name
            .entry(name.to_owned())
            .or_insert_with(|| Arc::from(placeholder(name)))
            .clone();
        for form in encoded_forms(value) {
            let form = form.into_bytes();
            if self.forms.insert(form.clone()) {
                self.needles.push(form);
                self.placeholders.push(placeholder.clone());
            }
        }
        self.matcher = OnceLock::new();
        self.pieces = OnceLock::new();
    }

    pub(super) fn matcher(&self) -> Option<&AhoCorasick> {
        self.matcher.get_or_init(|| build(&self.needles)).as_ref()
    }

    pub(super) fn piece_matcher(&self) -> Option<&AhoCorasick> {
        self.pieces
            .get_or_init(|| {
                let mut seen = HashSet::new();
                let mut pieces = Vec::new();
                for (_, value) in &self.entries {
                    let chars: Vec<char> = value.chars().collect();
                    for window in chars.windows(MIN_SECRET_PIECE) {
                        let piece: String = window.iter().collect();
                        if seen.insert(piece.clone()) {
                            pieces.push(piece);
                        }
                    }
                }
                build(&pieces)
            })
            .as_ref()
    }

    pub(super) fn placeholder(&self, pattern: usize) -> &Arc<str> {
        &self.placeholders[pattern]
    }

    pub(super) fn longest(&self) -> usize {
        self.needles.iter().map(Vec::len).max().unwrap_or(0)
    }
}

/// Pattern bytes above which a matcher is a contiguous NFA: a DFA over
/// that many long patterns takes long to build and much memory.
const LARGE_PATTERNS: usize = 64 * 1024;

fn build<P: AsRef<[u8]>>(patterns: &[P]) -> Option<AhoCorasick> {
    if patterns.is_empty() {
        return None;
    }
    let total: usize = patterns.iter().map(|p| p.as_ref().len()).sum();
    let kind = (total > LARGE_PATTERNS).then_some(AhoCorasickKind::ContiguousNFA);
    AhoCorasickBuilder::new().kind(kind).build(patterns).ok()
}
