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
    /// taint checks only (never for output), kept up to date as values are
    /// added: nothing to rebuild.
    pieces: HashSet<Piece>,
}

impl Layer {
    /// One layer holding every value of `layers`, from their forms as
    /// they are (nothing is encoded again).
    pub(super) fn compacted(layers: &[Arc<Layer>]) -> Self {
        let mut layer = Self::default();
        for other in layers {
            for (name, value) in &other.entries {
                if layer.values.insert(value.clone()) {
                    layer.entries.push((name.clone(), value.clone()));
                }
            }
            for name in &other.names {
                if !layer.names.contains(name) {
                    layer.names.push(name.clone());
                }
            }
            layer
                .by_name
                .extend(other.by_name.iter().map(|(n, p)| (n.clone(), p.clone())));
            for (needle, placeholder) in other.needles.iter().zip(&other.placeholders) {
                if layer.forms.insert(needle.clone()) {
                    layer.needles.push(needle.clone());
                    layer.placeholders.push(placeholder.clone());
                }
            }
            layer.pieces.extend(&other.pieces);
        }
        layer
    }

    pub(super) fn is_empty(&self) -> bool {
        self.needles.is_empty()
    }

    /// Whether the matcher was built: adding a value would throw it away.
    pub(super) fn is_built(&self) -> bool {
        self.matcher.get().is_some()
    }

    /// The bytes of every form held, to compare layer sizes.
    pub(super) fn size(&self) -> usize {
        self.needles.iter().map(Vec::len).sum::<usize>().max(1)
    }

    /// Every value of this layer, with its name, in the order added.
    pub(super) fn entries(&self) -> &[(String, String)] {
        &self.entries
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
        self.pieces.extend(pieces(value));
        self.matcher = OnceLock::new();
    }

    pub(super) fn matcher(&self) -> Option<&AhoCorasick> {
        self.matcher.get_or_init(|| build(&self.needles)).as_ref()
    }

    /// Whether `text` holds any [`MIN_SECRET_PIECE`]-character piece of a
    /// value of this layer.
    pub(super) fn has_piece_in(&self, text: &str) -> bool {
        !self.pieces.is_empty() && pieces(text).any(|p| self.pieces.contains(&p))
    }

    pub(super) fn placeholder(&self, pattern: usize) -> &Arc<str> {
        &self.placeholders[pattern]
    }

    pub(super) fn longest(&self) -> usize {
        self.needles.iter().map(Vec::len).max().unwrap_or(0)
    }
}

type Piece = [char; MIN_SECRET_PIECE];

/// Every [`MIN_SECRET_PIECE`]-character window of `text`.
fn pieces(text: &str) -> impl Iterator<Item = Piece> + '_ {
    let mut window: Vec<char> = Vec::with_capacity(MIN_SECRET_PIECE);
    text.chars().filter_map(move |c| {
        if window.len() == MIN_SECRET_PIECE {
            window.remove(0);
        }
        window.push(c);
        window.as_slice().try_into().ok()
    })
}

/// A matcher over `patterns`: a contiguous NFA, whose memory stays
/// proportional to the pattern bytes (a DFA over long secrets in many
/// encoded forms takes hundreds of KiB each, and long to build).
fn build<P: AsRef<[u8]>>(patterns: &[P]) -> Option<AhoCorasick> {
    if patterns.is_empty() {
        return None;
    }
    AhoCorasickBuilder::new()
        .kind(Some(AhoCorasickKind::ContiguousNFA))
        .build(patterns)
        .ok()
}
