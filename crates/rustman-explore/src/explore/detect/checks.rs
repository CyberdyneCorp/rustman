//! The expectation of each probe step ([`Check`]).

use serde_json::Value;

use super::conformance::conformance;
use super::{Detected, Observed, Scope};
use crate::explore::plan::Check;

pub fn check(check: &Check, scope: &Scope<'_>, response: &Observed<'_>) -> Vec<Detected> {
    let status = response.status;
    let found = match check {
        Check::Conformance => {
            return scope
                .operation
                .map(|op| conformance(op, scope.components, response))
                .unwrap_or_default();
        }
        Check::RejectsInput if status >= 500 => Some(Detected::new(
            "server-error-on-invalid-input",
            format!("invalid input got status {status} instead of 4xx"),
        )),
        Check::RejectsInput if response.is_success() => Some(Detected::new(
            "invalid-input-accepted",
            format!("invalid input was accepted with status {status}"),
        )),
        Check::RequiresAuth if response.is_success() => Some(Detected::new(
            "auth-not-enforced",
            format!("a request without valid credentials got status {status}"),
        )),
        Check::RequiresAuth if status != 401 && status != 403 => Some(Detected::new(
            "unexpected-auth-status",
            format!(
                "a request without valid credentials got status {status} instead of 401 or 403"
            ),
        )),
        Check::MethodNotAllowed if response.is_success() => Some(Detected::new(
            "undocumented-method-allowed",
            format!("an undocumented method got status {status} instead of 405 or 404"),
        )),
        Check::ReflectsUpdate { expected } if response.is_success() => {
            not_reflected(expected, response)
        }
        Check::Gone if status != 404 && status != 410 => Some(Detected::new(
            "resource-survives-delete",
            format!("a read after delete got status {status} instead of 404 or 410"),
        )),
        Check::DeniedToOther
            if response.is_success() && same_body(scope.previous, response.body) =>
        {
            Some(Detected::new(
                "broken-object-level-authorization",
                format!("identity B read identity A's resource with status {status}"),
            ))
        }
        _ => None,
    };
    found.into_iter().collect()
}

/// `update-not-reflected` when a field the update set reads back with
/// another value. Fields the read leaves out are not judged.
fn not_reflected(expected: &Value, response: &Observed<'_>) -> Option<Detected> {
    let body = response.json()?;
    let differing: Vec<String> = expected
        .as_object()?
        .iter()
        .filter_map(|(name, want)| {
            let got = body.get(name)?;
            (got != want).then(|| format!("'{name}' is {got} after the update set {want}"))
        })
        .collect();
    (!differing.is_empty()).then(|| Detected::new("update-not-reflected", differing.join("; ")))
}

/// Whether two bodies are the same: equal JSON, or equal bytes.
fn same_body(previous: Option<&[u8]>, body: &[u8]) -> bool {
    let Some(previous) = previous else {
        return false;
    };
    let json = |b: &[u8]| serde_json::from_slice::<Value>(b).ok();
    match (json(previous), json(body)) {
        (Some(a), Some(b)) => a == b,
        _ => previous == body,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use http::HeaderMap;
    use serde_json::json;

    fn rules(
        check: &Check,
        status: u16,
        body: &[u8],
        previous: Option<&[u8]>,
    ) -> Vec<&'static str> {
        let headers = HeaderMap::new();
        let components = json!({});
        let scope = Scope {
            operation: None,
            components: &components,
            scans: &[],
            previous,
        };
        let observed = Observed {
            status,
            headers: &headers,
            body,
            https: false,
        };
        super::check(check, &scope, &observed)
            .into_iter()
            .map(|d| d.rule)
            .collect()
    }

    #[test]
    fn each_expectation() {
        let none: Vec<&str> = Vec::new();
        assert_eq!(
            rules(&Check::RejectsInput, 503, b"", None),
            ["server-error-on-invalid-input"]
        );
        assert_eq!(
            rules(&Check::RejectsInput, 204, b"", None),
            ["invalid-input-accepted"]
        );
        assert_eq!(rules(&Check::RejectsInput, 422, b"", None), none);
        assert_eq!(
            rules(&Check::RequiresAuth, 200, b"", None),
            ["auth-not-enforced"]
        );
        assert_eq!(
            rules(&Check::RequiresAuth, 404, b"", None),
            ["unexpected-auth-status"]
        );
        assert_eq!(rules(&Check::RequiresAuth, 403, b"", None), none);
        assert_eq!(
            rules(&Check::MethodNotAllowed, 200, b"", None),
            ["undocumented-method-allowed"]
        );
        assert_eq!(rules(&Check::MethodNotAllowed, 405, b"", None), none);
        assert_eq!(
            rules(&Check::Gone, 200, b"", None),
            ["resource-survives-delete"]
        );
        assert_eq!(rules(&Check::Gone, 410, b"", None), none);
    }

    #[test]
    fn update_reflection_compares_only_fields_read_back() {
        let check = Check::ReflectsUpdate {
            expected: json!({"name": "updated", "age": 3}),
        };
        assert_eq!(
            rules(&check, 200, br#"{"name": "old"}"#, None),
            ["update-not-reflected"]
        );
        let none: Vec<&str> = Vec::new();
        assert_eq!(rules(&check, 200, br#"{"name": "updated"}"#, None), none);
        assert_eq!(rules(&check, 404, br#"{"name": "old"}"#, None), none);
    }

    #[test]
    fn bola_needs_the_same_resource() {
        let a = br#"{"id": "o-1", "item": "x"}"#;
        assert_eq!(
            rules(
                &Check::DeniedToOther,
                200,
                br#"{"item":"x","id":"o-1"}"#,
                Some(a)
            ),
            ["broken-object-level-authorization"]
        );
        let none: Vec<&str> = Vec::new();
        assert_eq!(
            rules(&Check::DeniedToOther, 200, br#"{"id": "o-2"}"#, Some(a)),
            none
        );
        assert_eq!(rules(&Check::DeniedToOther, 404, a, Some(a)), none);
    }
}
