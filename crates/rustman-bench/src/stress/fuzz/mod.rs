//! Fuzz mixing (stress-testing "Fuzz mode and fuzz mixing", "Mutation
//! sources", "Reproducible fuzzing under load", design D4): each request
//! slot is replaced by a mutated variant with probability `--fuzz-ratio`,
//! decided by the seeded pick of the slot, so offered load is unchanged
//! and the same seed fuzzes the same slots with the same variants.
//!
//! A slot is one request of one iteration: iteration `i` of the fuzz phase,
//! request file `f` of a plan of `n` files, is slot `i × n + f`. The pool
//! a fuzzed slot draws from holds the mutations (schema-aware with
//! `--spec`, else schema-less) and the `--malformed` families.
//!
//! - [`gate`] sends a fuzzed slot's variant in place of the request's body;
//! - [`findings`] turns how the target answered fuzzed requests into
//!   findings;
//! - [`impact`] compares valid traffic against the clean baseline phase
//!   (`fuzz-degrades-valid-traffic`).

pub mod findings;
pub(crate) mod gate;
pub mod impact;

use std::sync::Arc;

use rustman_explore::mutate::pick;
use rustman_explore::mutate::schema::SchemaMutator;

use crate::stress::malformed::{Caps, Family, PayloadCache};

/// The share of requests fuzz mode (and `--malformed` without
/// `--fuzz-ratio`) replaces.
pub const DEFAULT_RATIO: f64 = 0.2;

/// Salts of the pick streams of a slot.
const SOURCE_SALT: u64 = 0x5EED_50C3_0000_0001;
const VARIANT_SALT: u64 = 0xBADB_0D1E_0000_0002;

/// Where a fuzzed slot's variant comes from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Source {
    /// A mutation of the saved JSON body (schema-aware with `--spec`).
    Mutation,
    Malformed(Family),
}

/// What a run fuzzes, and how.
pub struct FuzzPlan {
    pub seed: u64,
    /// `0 < ratio <= 1`.
    pub ratio: f64,
    /// Mutations are in the pool (`--fuzz-ratio`, fuzz mode).
    pub mutations: bool,
    /// `--spec`: schema-aware mutations.
    pub schema: Option<Arc<SchemaMutator>>,
    /// `--malformed`, in order.
    pub families: Vec<Family>,
    pub caps: Caps,
    /// `[stress] expected_max_body`.
    pub expected_max_body: Option<u64>,
    /// The run's shared large payloads.
    pub payloads: PayloadCache,
}

impl std::fmt::Debug for FuzzPlan {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FuzzPlan")
            .field("seed", &self.seed)
            .field("ratio", &self.ratio)
            .field("mutations", &self.mutations)
            .field("schema", &self.schema.is_some())
            .field("families", &self.families)
            .finish_non_exhaustive()
    }
}

impl FuzzPlan {
    /// Whether slot `slot` is replaced by a variant.
    pub fn fuzzed(&self, slot: u64) -> bool {
        unit(pick(self.seed, slot)) < self.ratio
    }

    /// The pool a fuzzed slot draws from.
    pub fn sources(&self) -> Vec<Source> {
        let mutations = self.mutations.then_some(Source::Mutation);
        mutations
            .into_iter()
            .chain(self.families.iter().map(|f| Source::Malformed(*f)))
            .collect()
    }

    /// The source of fuzzed slot `slot`.
    pub fn source(&self, slot: u64) -> Option<Source> {
        let sources = self.sources();
        let n = u64::try_from(sources.len()).ok().filter(|n| *n > 0)?;
        let index = usize::try_from(pick(self.seed ^ SOURCE_SALT, slot) % n).ok()?;
        Some(sources[index])
    }

    /// The seeded pick that chooses slot `slot`'s variant within its
    /// source.
    pub fn variant_pick(&self, slot: u64) -> u64 {
        pick(self.seed ^ VARIANT_SALT, slot)
    }

    /// The fuzzed slots among the first `slots`.
    pub fn fuzzed_slots(&self, slots: u64) -> Vec<u64> {
        (0..slots).filter(|s| self.fuzzed(*s)).collect()
    }
}

/// A 64-bit pick as a share in `[0, 1)`.
fn unit(pick: u64) -> f64 {
    (pick >> 11) as f64 / (1u64 << 53) as f64
}

/// A seed for a run without `--seed`.
pub fn random_seed() -> u64 {
    rand::random()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plan(seed: u64, ratio: f64, families: Vec<Family>) -> FuzzPlan {
        FuzzPlan {
            seed,
            ratio,
            mutations: true,
            schema: None,
            families,
            caps: Caps {
                max_body: 1024,
                large_body: 1024,
                bomb: 1024,
            },
            payloads: PayloadCache::default(),
            expected_max_body: None,
        }
    }

    #[test]
    fn the_ratio_holds_within_ten_percent_over_ten_thousand_slots() {
        for seed in [1, 42, 7_777, u64::MAX] {
            for ratio in [0.1, 0.2, 0.5] {
                let fuzzed = plan(seed, ratio, Vec::new()).fuzzed_slots(10_000).len() as f64;
                let expected = 10_000.0 * ratio;
                assert!(
                    (fuzzed - expected).abs() <= expected * 0.1,
                    "seed {seed}, ratio {ratio}: {fuzzed}"
                );
            }
        }
        let all = plan(3, 1.0, Vec::new()).fuzzed_slots(1000).len();
        assert_eq!(all, 1000);
    }

    #[test]
    fn the_same_seed_fuzzes_the_same_slots() {
        let a = plan(42, 0.1, Vec::new()).fuzzed_slots(10_000);
        let b = plan(42, 0.1, Vec::new()).fuzzed_slots(10_000);
        let c = plan(43, 0.1, Vec::new()).fuzzed_slots(10_000);
        assert_eq!(a, b);
        assert_ne!(a, c);
    }

    #[test]
    fn sources_cover_mutations_and_every_family() {
        let families = vec![Family::JsonSyntax, Family::DeepNesting];
        let plan = plan(9, 1.0, families);
        let mut seen: Vec<Source> = (0..200).filter_map(|s| plan.source(s)).collect();
        seen.sort_by_key(|s| format!("{s:?}"));
        seen.dedup();
        assert_eq!(seen.len(), 3, "{seen:?}");
        let only_malformed = FuzzPlan {
            mutations: false,
            ..plan
        };
        assert!((0..50).all(|s| matches!(only_malformed.source(s), Some(Source::Malformed(_)))));
    }
}
