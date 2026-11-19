//! What a diff means for the command (contract-testing "Diff exit codes and
//! strictness"): which differences the allowlist accepts, and which fail.

use crate::allowlist::Applied;
use crate::catalog::Classification;
use crate::difference::{Diff, Difference};

/// Which differences fail besides unallowlisted breaking ones.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Strictness {
    /// `--fail-on-unknown`.
    pub unknown: bool,
    /// `--fail-on non-breaking`: any difference fails (frozen APIs).
    pub any: bool,
}

/// A diff with the allowlist applied and the strictness chosen.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Verdict {
    pub diff: Diff,
    /// Per difference: the justification when the allowlist accepts it.
    pub allowed: Vec<Option<String>>,
    /// Expired and unused allowlist entries.
    pub warnings: Vec<String>,
    pub strictness: Strictness,
}

impl Verdict {
    pub fn new(diff: Diff, applied: Applied, strictness: Strictness) -> Verdict {
        let mut allowed = applied.allowed;
        allowed.resize(diff.differences.len(), None);
        Verdict {
            diff,
            allowed,
            warnings: applied.warnings,
            strictness,
        }
    }

    /// The differences with their justification, when allowed.
    pub fn items(&self) -> impl Iterator<Item = (&Difference, Option<&str>)> {
        self.diff
            .differences
            .iter()
            .zip(&self.allowed)
            .map(|(d, a)| (d, a.as_deref()))
    }

    /// Whether `d`, allowed or not, fails the command.
    pub fn fails(&self, d: &Difference, allowed: bool) -> bool {
        if allowed {
            return false;
        }
        match d.classification {
            Classification::Breaking => true,
            Classification::Unknown => self.strictness.unknown || self.strictness.any,
            Classification::NonBreaking => self.strictness.any,
        }
    }

    /// How many differences fail the command.
    pub fn failing(&self) -> usize {
        self.items()
            .filter(|(d, a)| self.fails(d, a.is_some()))
            .count()
    }

    /// How many differences the allowlist accepted.
    pub fn allowed_count(&self) -> usize {
        self.allowed.iter().filter(|a| a.is_some()).count()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::catalog::RuleId;

    fn difference(rule: RuleId) -> Difference {
        Difference {
            rule,
            classification: rule.classification(),
            operation: "GET /a".into(),
            message: "m".into(),
            old_pointer: "/paths".into(),
            new_pointer: "/paths".into(),
        }
    }

    fn verdict(rules: &[RuleId], allowed: Vec<Option<String>>, strictness: Strictness) -> Verdict {
        let differences = rules.iter().map(|r| difference(*r)).collect();
        let applied = Applied {
            allowed,
            warnings: Vec::new(),
        };
        Verdict::new(Diff::new("a", "b", differences), applied, strictness)
    }

    #[test]
    fn breaking_fails_unless_allowed() {
        let rules = [RuleId::OperationRemoved, RuleId::OperationRemoved];
        let v = verdict(&rules, vec![Some("ok".into())], Strictness::default());
        assert_eq!((v.failing(), v.allowed_count()), (1, 1));
    }

    #[test]
    fn unknown_fails_only_when_asked() {
        let rules = [RuleId::CompositionChanged];
        assert_eq!(verdict(&rules, vec![], Strictness::default()).failing(), 0);
        let strict = Strictness {
            unknown: true,
            any: false,
        };
        assert_eq!(verdict(&rules, vec![], strict).failing(), 1);
    }

    #[test]
    fn fail_on_non_breaking_fails_any_difference() {
        let rules = [RuleId::OperationAdded, RuleId::CompositionChanged];
        assert_eq!(verdict(&rules, vec![], Strictness::default()).failing(), 0);
        let frozen = Strictness {
            unknown: false,
            any: true,
        };
        assert_eq!(verdict(&rules, vec![], frozen).failing(), 2);
    }
}
