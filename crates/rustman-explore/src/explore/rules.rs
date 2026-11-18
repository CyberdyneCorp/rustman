//! The static registry of finding rules (design D8). Detectors return rule
//! ids, never free text, so reports, diffs and SARIF stay stable across
//! versions.

pub use rustman_report::exploration::{Severity, SeverityCounts};

/// One finding rule.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Rule {
    /// Stable id, e.g. `auth-not-enforced`.
    pub id: &'static str,
    pub severity: Severity,
    pub description: &'static str,
    pub remediation: &'static str,
}

/// Every rule, in report order.
pub const RULES: [Rule; 15] = [
    Rule {
        id: "auth-not-enforced",
        severity: Severity::Critical,
        description: "An operation that requires credentials answered 2xx without valid credentials.",
        remediation: "Reject requests without valid credentials with 401 or 403.",
    },
    Rule {
        id: "broken-object-level-authorization",
        severity: Severity::Critical,
        description: "A second identity could read a resource that belongs to the first.",
        remediation: "Check that the caller owns the object on every access by id.",
    },
    Rule {
        id: "server-error-on-invalid-input",
        severity: Severity::High,
        description: "Invalid input caused a 5xx response instead of a 4xx.",
        remediation: "Validate input and answer 400 or 422 with a problem description.",
    },
    Rule {
        id: "server-error",
        severity: Severity::High,
        description: "A valid request caused an undocumented 5xx response.",
        remediation: "Fix the server error, or document the response if it is expected.",
    },
    Rule {
        id: "response-schema-violation",
        severity: Severity::Medium,
        description: "A response body does not match the schema documented for its status.",
        remediation: "Make the response match the documented schema, or fix the document.",
    },
    Rule {
        id: "invalid-input-accepted",
        severity: Severity::Medium,
        description: "Input that violates the documented schema was accepted with 2xx.",
        remediation: "Validate input against the documented schema and reject violations with 4xx.",
    },
    Rule {
        id: "undocumented-method-allowed",
        severity: Severity::Medium,
        description: "A method the document does not list for a path answered 2xx.",
        remediation: "Answer 405 for methods the path does not support, or document the method.",
    },
    Rule {
        id: "error-leakage",
        severity: Severity::Medium,
        description: "A response reveals internals: a stack trace, SQL error text, a debug page or a file path.",
        remediation: "Return generic error bodies and log the details on the server.",
    },
    Rule {
        id: "update-not-reflected",
        severity: Severity::Medium,
        description: "A read after a successful update does not show the updated values.",
        remediation: "Persist updates before answering, or answer with an error.",
    },
    Rule {
        id: "resource-survives-delete",
        severity: Severity::Medium,
        description: "A read after a successful delete did not answer 404 or 410.",
        remediation: "Delete the resource, or answer the delete with an error.",
    },
    Rule {
        id: "unexpected-auth-status",
        severity: Severity::Low,
        description: "A request without valid credentials was refused with a status other than 401 or 403.",
        remediation: "Answer 401 for missing or invalid credentials and 403 for insufficient ones.",
    },
    Rule {
        id: "missing-hsts",
        severity: Severity::Low,
        description: "An HTTPS response has no Strict-Transport-Security header.",
        remediation: "Send Strict-Transport-Security on every HTTPS response.",
    },
    Rule {
        id: "missing-content-type-options",
        severity: Severity::Low,
        description: "A response has no X-Content-Type-Options: nosniff header.",
        remediation: "Send X-Content-Type-Options: nosniff on every response.",
    },
    Rule {
        id: "permissive-cors",
        severity: Severity::Low,
        description: "A response allows any origin (*) together with credentials.",
        remediation: "Allow only the origins that need access when credentials are allowed.",
    },
    Rule {
        id: "undocumented-status",
        severity: Severity::Info,
        description: "A response status is not documented for its operation.",
        remediation: "Document every status the operation can answer with.",
    },
];

/// The rule with `id`.
pub fn rule(id: &str) -> Option<&'static Rule> {
    RULES.iter().find(|r| r.id == id)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_are_unique_kebab_case() {
        for (i, r) in RULES.iter().enumerate() {
            assert!(
                r.id.chars().all(|c| c.is_ascii_lowercase() || c == '-'),
                "{}",
                r.id
            );
            assert!(RULES[i + 1..].iter().all(|o| o.id != r.id), "{}", r.id);
        }
        assert_eq!(
            rule("auth-not-enforced").unwrap().severity,
            Severity::Critical
        );
        assert!(rule("nope").is_none());
    }

    #[test]
    fn counts_add_up_by_threshold() {
        let mut counts = SeverityCounts::default();
        counts.add(Severity::Medium);
        counts.add(Severity::High);
        counts.add(Severity::Info);
        assert_eq!(counts.total(), 3);
        assert_eq!(counts.at_or_above(Severity::High), 1);
        assert_eq!(counts.at_or_above(Severity::Medium), 2);
        assert_eq!(
            counts.to_string(),
            "0 critical, 1 high, 1 medium, 0 low, 1 info"
        );
    }

    #[test]
    fn severities_parse_and_order() {
        assert_eq!("medium".parse::<Severity>(), Ok(Severity::Medium));
        assert!("urgent".parse::<Severity>().is_err());
        assert!(Severity::Critical < Severity::Info);
    }
}
