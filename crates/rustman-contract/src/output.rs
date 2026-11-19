//! The human summaries printed by `rustman contract diff`,
//! `rustman contract pact verify` and `rustman contract verify`.

use crate::catalog::Classification;
use crate::conformance::{self, Conformance};
use crate::pact::verify::{Outcome, Verification};
use crate::verdict::Verdict;

/// One block per difference, then the counts per classification:
///
/// ```text
/// breaking      operation-removed  DELETE /users/{id}: operation removed
///   old: /paths/~1users~1{id}/delete
///   new: /paths/~1users~1{id}
///
/// 1 breaking, 0 non-breaking, 0 unknown (rule catalog version 2)
/// ```
///
/// An allowlisted difference reads `breaking (allowed)`, with its
/// justification.
pub fn human(verdict: &Verdict) -> String {
    let diff = &verdict.diff;
    let label = |c: Classification, allowed: bool| {
        if allowed {
            format!("{} (allowed)", c.as_str())
        } else {
            c.as_str().to_owned()
        }
    };
    let class_width = verdict
        .items()
        .map(|(d, a)| label(d.classification, a.is_some()).len())
        .max()
        .unwrap_or(0)
        .max(Classification::NonBreaking.as_str().len());
    let rule_width = diff
        .differences
        .iter()
        .map(|d| d.rule.id().len())
        .max()
        .unwrap_or(0);
    let mut out = String::new();
    for (d, allowed) in verdict.items() {
        out.push_str(&format!(
            "{:class_width$}  {:rule_width$}  {}: {}\n  old: {}\n  new: {}\n",
            label(d.classification, allowed.is_some()),
            d.rule.id(),
            d.operation,
            d.message,
            d.old_pointer,
            d.new_pointer
        ));
        if let Some(why) = allowed {
            out.push_str(&format!("  allowed: {why}\n"));
        }
    }
    if diff.differences.is_empty() {
        out.push_str("no differences\n");
    }
    let mut summary = diff.summary();
    let allowed = verdict.allowed_count();
    if allowed > 0 {
        summary.push_str(&format!(", {allowed} allowed"));
    }
    out.push_str(&format!(
        "\n{summary} (rule catalog version {})\n",
        diff.catalog_version
    ));
    out
}

/// One line per interaction, its mismatches below it, then the counts:
///
/// ```text
/// passed   get user (web-users.json)
/// failed   get user 7 (web-users.json)
///   $.id: expected number 42, got string "42"
/// blocked  create user (web-users.json): blocked: mutations not allowed
///
/// 1 passed, 1 failed, 1 blocked
/// ```
pub fn pact_human(verification: &Verification) -> String {
    let mut out = String::new();
    for result in &verification.results {
        let status = result.outcome.status();
        let head = format!("{status:<8} {} ({})", result.description, result.file);
        match &result.outcome {
            Outcome::Passed => out.push_str(&format!("{head}\n")),
            Outcome::Blocked(reason) => out.push_str(&format!("{head}: {reason}\n")),
            Outcome::Failed(mismatches) => {
                out.push_str(&format!("{head}\n"));
                for mismatch in mismatches {
                    out.push_str(&format!("  {mismatch}\n"));
                }
            }
        }
    }
    if verification.results.is_empty() {
        out.push_str("no interactions\n");
    }
    out.push_str(&format!("\n{}\n", verification.summary()));
    out
}

/// The violations and undocumented exchanges, the operations observed,
/// then the coverage and counts:
///
/// ```text
/// violation     GET /users/{id}: exchange 01J... (GET http://h/users/7)
///   $.email: required property missing
/// undocumented  GET /internal/debug: exchange 01J...
///
/// observed operations:
///   GET /users/{id}  2 exchanges, 1 with violations
///
/// 1/12 operations observed; 1 violation, 1 undocumented, 1 conforms
/// ```
pub fn conformance_human(result: &Conformance) -> String {
    use conformance::Verdict as V;
    let mut out = String::new();
    for checked in result.checked.iter().filter(|c| c.verdict != V::Conforms) {
        let exchange = &checked.exchange;
        out.push_str(&format!(
            "{:<13} {}: exchange {} ({} {})\n",
            checked.verdict.as_str(),
            checked.subject,
            exchange.id,
            exchange.method,
            exchange.url
        ));
        for message in &checked.messages {
            out.push_str(&format!("  {message}\n"));
        }
    }
    let observed: Vec<_> = result
        .coverage
        .operations
        .iter()
        .filter(|o| o.exchanges > 0)
        .collect();
    if !observed.is_empty() {
        out.push_str("\nobserved operations:\n");
        for o in observed {
            out.push_str(&format!(
                "  {}  {} exchanges, {} with violations\n",
                o.operation, o.exchanges, o.violations
            ));
        }
    }
    out.push_str(&format!(
        "\n{}; {} violation, {} undocumented, {} conforms\n",
        result.coverage.line(),
        result.count(V::Violation),
        result.count(V::Undocumented),
        result.count(V::Conforms)
    ));
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::allowlist::Applied;
    use crate::catalog::RuleId;
    use crate::difference::{Diff, Difference};
    use crate::verdict::Strictness;

    fn removal() -> Diff {
        let rule = RuleId::OperationRemoved;
        Diff::new(
            "v1.yaml",
            "v2.yaml",
            vec![Difference {
                rule,
                classification: rule.classification(),
                operation: "DELETE /users/{id}".into(),
                message: "operation removed".into(),
                old_pointer: "/paths/~1users~1{id}/delete".into(),
                new_pointer: "/paths".into(),
            }],
        )
    }

    fn verdict(diff: Diff, allowed: Vec<Option<String>>) -> Verdict {
        let applied = Applied {
            allowed,
            warnings: Vec::new(),
        };
        Verdict::new(diff, applied, Strictness::default())
    }

    #[test]
    fn differences_then_counts() {
        assert_eq!(
            human(&verdict(removal(), Vec::new())),
            "breaking      operation-removed  DELETE /users/{id}: operation removed\n  \
             old: /paths/~1users~1{id}/delete\n  new: /paths\n\n\
             1 breaking, 0 non-breaking, 0 unknown (rule catalog version 2)\n"
        );
        let empty = verdict(Diff::new("a", "b", Vec::new()), Vec::new());
        assert!(human(&empty).starts_with("no differences\n"));
    }

    #[test]
    fn allowed_differences_say_so() {
        let text = human(&verdict(removal(), vec![Some("no traffic".into())]));
        assert!(
            text.starts_with(
                "breaking (allowed)  operation-removed  DELETE /users/{id}: operation removed\n"
            ),
            "{text}"
        );
        assert!(text.contains("\n  allowed: no traffic\n"), "{text}");
        assert!(text.ends_with(
            "1 breaking, 0 non-breaking, 0 unknown, 1 allowed (rule catalog version 2)\n"
        ));
    }
}
