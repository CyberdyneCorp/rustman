//! Turning secret occurrences into the disjoint runs a redacted output
//! replaces. Occurrences arrive in order of where they end (as an
//! overlapping Aho-Corasick search reports them, merged across layers and
//! with secret spans), and are folded into runs as they come, so memory
//! stays bounded by the number of runs, never by the number of
//! occurrences (a periodic body can hold many per byte).

use std::ops::Range;
use std::sync::Arc;

/// One occurrence: where it is, and the placeholder that replaces it.
pub(super) struct Hit {
    pub range: Range<usize>,
    pub placeholder: Arc<str>,
}

/// Overlapping occurrences, replaced as a whole, by the placeholders of the
/// secrets that make the run up, left to right.
pub(super) struct Run {
    pub range: Range<usize>,
    pub placeholders: Vec<Arc<str>>,
}

impl Run {
    pub(super) fn replacement(&self) -> String {
        self.placeholders.concat()
    }
}

/// Disjoint runs, sorted, built from hits sorted by end.
#[derive(Default)]
pub(super) struct Runs {
    runs: Vec<Run>,
}

impl Runs {
    /// Add `hit`, which ends at or after every hit added before. A hit
    /// inside the last run adds nothing; one covering runs replaces them;
    /// one overlapping the start of a run extends it with its placeholder.
    pub(super) fn add(&mut self, hit: Hit) {
        let Hit { range, placeholder } = hit;
        let inside = self
            .runs
            .last()
            .is_some_and(|last| range.start >= last.range.start && range.end <= last.range.end);
        if inside || range.is_empty() {
            return;
        }
        let mut run = Run {
            range,
            placeholders: vec![placeholder],
        };
        while let Some(last) = self.runs.pop_if(|last| last.range.end > run.range.start) {
            if last.range.start < run.range.start {
                let mut placeholders = last.placeholders;
                for p in run.placeholders {
                    if !placeholders.contains(&p) {
                        placeholders.push(p);
                    }
                }
                run = Run {
                    range: last.range.start..run.range.end,
                    placeholders,
                };
            }
        }
        self.runs.push(run);
    }

    pub(super) fn into_vec(self) -> Vec<Run> {
        self.runs
    }
}

/// Merge iterators of hits, each sorted by end, into one sorted by end.
pub(super) fn by_end(mut sources: Vec<Box<dyn Iterator<Item = Hit> + '_>>) -> Runs {
    let mut heads: Vec<Option<Hit>> = sources.iter_mut().map(Iterator::next).collect();
    let mut runs = Runs::default();
    loop {
        let next = heads
            .iter()
            .enumerate()
            .filter_map(|(i, h)| h.as_ref().map(|h| (h.range.end, i)))
            .min();
        let Some((_, i)) = next else {
            return runs;
        };
        if let Some(hit) = heads[i].take() {
            runs.add(hit);
        }
        heads[i] = sources[i].next();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hit(range: Range<usize>, name: &str) -> Hit {
        Hit {
            range,
            placeholder: Arc::from(name),
        }
    }

    fn shown(runs: Runs) -> Vec<(Range<usize>, String)> {
        let runs = runs.into_vec();
        runs.iter()
            .map(|r| (r.range.clone(), r.replacement()))
            .collect()
    }

    #[test]
    fn hits_fold_into_disjoint_runs() {
        let mut runs = Runs::default();
        for h in [
            hit(0..3, "s"),
            hit(0..6, "L"),
            hit(5..8, "x"),
            hit(6..8, "y"),
        ] {
            runs.add(h);
        }
        runs.add(hit(10..12, "z"));
        assert_eq!(
            shown(runs),
            [(0..8, "Lx".to_owned()), (10..12, "z".to_owned())]
        );
    }

    #[test]
    fn sources_merge_by_end() {
        let a: Vec<Hit> = vec![hit(0..2, "a"), hit(4..9, "a")];
        let b: Vec<Hit> = vec![hit(1..5, "b")];
        let runs = by_end(vec![Box::new(a.into_iter()), Box::new(b.into_iter())]);
        assert_eq!(shown(runs), [(0..9, "ab".to_owned())]);
    }
}
