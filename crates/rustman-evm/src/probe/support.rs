//! Reading node answers: method support (chain-exploration "Method support
//! matrix"), archive state ("Archive node detection") and rate limits.

use std::fmt;

use super::meter::{Answer, ProbeFailure};
use crate::rpc::{METHOD_NOT_FOUND, RpcFailure};

/// How a provider answered a method.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Support {
    Supported,
    /// `-32601` or an equivalent "no such method" error.
    Unsupported,
    /// An auth or plan error.
    Restricted,
    Error,
    /// Not sent: the budget ran out.
    SkippedBudget,
}

impl Support {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Supported => "supported",
            Self::Unsupported => "unsupported",
            Self::Restricted => "restricted",
            Self::Error => "error",
            Self::SkippedBudget => "skipped (budget)",
        }
    }
}

impl fmt::Display for Support {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Words of an auth or plan refusal.
const RESTRICTED_WORDS: [&str; 9] = [
    "unauthorized",
    "unauthorised",
    "forbidden",
    "plan",
    "upgrade",
    "permission",
    "whitelisted",
    "allowlisted",
    "paid",
];

/// Phrases of an auth or plan refusal.
const RESTRICTED_PHRASES: [&str; 5] = [
    "api key",
    "apikey",
    "not allowed",
    "access denied",
    "does not have access",
];

/// Phrases of a missing method.
const UNSUPPORTED_PHRASES: [&str; 6] = [
    "method not found",
    "does not exist",
    "not available",
    "not supported",
    "unsupported",
    "unknown method",
];

/// The class of one answer, with the error shown in reports.
pub fn classify(answer: &Answer) -> (Support, Option<String>) {
    let failure = match answer {
        Ok(_) => return (Support::Supported, None),
        Err(ProbeFailure::Budget) => return (Support::SkippedBudget, None),
        Err(failure @ ProbeFailure::Rpc(rpc)) => (failure.detail(), rpc),
    };
    let (detail, rpc) = failure;
    (support_of(rpc), Some(detail))
}

fn support_of(failure: &RpcFailure) -> Support {
    match failure {
        RpcFailure::Error { code, message, .. } => {
            let text = message.to_ascii_lowercase();
            if restricted(&text) {
                Support::Restricted
            } else if *code == METHOD_NOT_FOUND || has_any(&text, &UNSUPPORTED_PHRASES) {
                Support::Unsupported
            } else {
                Support::Error
            }
        }
        RpcFailure::Transport(message) if http_status(message).is_some_and(is_auth_status) => {
            Support::Restricted
        }
        RpcFailure::Transport(_) | RpcFailure::Refused(_) => Support::Error,
    }
}

fn restricted(text: &str) -> bool {
    let words = text.split(|c: char| !c.is_ascii_alphanumeric());
    has_any(text, &RESTRICTED_PHRASES) || words.into_iter().any(|w| RESTRICTED_WORDS.contains(&w))
}

fn has_any(text: &str, phrases: &[&str]) -> bool {
    phrases.iter().any(|p| text.contains(p))
}

fn is_auth_status(status: u16) -> bool {
    (401..=403).contains(&status)
}

/// The status of a transport failure shown as `HTTP <status>`.
pub fn http_status(message: &str) -> Option<u16> {
    let rest = message.strip_prefix("HTTP ")?;
    rest.get(..3)?.parse().ok()
}

/// Whether `failure` is a rate limit: HTTP 429, JSON-RPC `-32005`, or an
/// error that says so.
pub fn is_rate_limited(failure: &RpcFailure) -> bool {
    match failure {
        RpcFailure::Transport(message) => http_status(message) == Some(429),
        RpcFailure::Error { code, message, .. } => {
            let text = message.to_ascii_lowercase();
            *code == rustman_jsonrpc::LIMIT_EXCEEDED
                || text.contains("rate limit")
                || text.contains("too many requests")
        }
        RpcFailure::Refused(_) => false,
    }
}

/// What the archive check found.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Archive {
    /// Historical state answered.
    Archive,
    /// Historical state is pruned.
    Full,
    Unknown,
    SkippedBudget,
}

impl Archive {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Archive => "archive",
            Self::Full => "full",
            Self::Unknown => "unknown",
            Self::SkippedBudget => "skipped (budget)",
        }
    }
}

/// Phrases of pruned historical state.
const PRUNED_PHRASES: [&str; 6] = [
    "missing trie node",
    "pruned",
    "state not available",
    "state is not available",
    "historical state",
    "state unavailable",
];

/// The archive class of an `eth_getBalance` answer at an old block.
pub fn archive_of(answer: &Answer) -> Archive {
    match answer {
        Ok(_) => Archive::Archive,
        Err(ProbeFailure::Budget) => Archive::SkippedBudget,
        Err(ProbeFailure::Rpc(RpcFailure::Error { message, .. })) => {
            let text = message.to_ascii_lowercase();
            match has_any(&text, &PRUNED_PHRASES) {
                true => Archive::Full,
                false => Archive::Unknown,
            }
        }
        Err(ProbeFailure::Rpc(_)) => Archive::Unknown,
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn error(code: i64, message: &str) -> Answer {
        Err(ProbeFailure::Rpc(RpcFailure::Error {
            code,
            message: message.into(),
            data: None,
        }))
    }

    fn transport(message: &str) -> Answer {
        Err(ProbeFailure::Rpc(RpcFailure::Transport(message.into())))
    }

    #[test]
    fn answers_are_classified() {
        let cases = [
            (Ok(json!("0x1")), Support::Supported),
            (
                error(
                    -32601,
                    "the method debug_traceCall does not exist/is not available",
                ),
                Support::Unsupported,
            ),
            (error(-32000, "method not found"), Support::Unsupported),
            (
                error(-32600, "Unsupported method: trace_call. Upgrade your plan"),
                Support::Restricted,
            ),
            (error(-32002, "invalid API key"), Support::Restricted),
            (transport("HTTP 403"), Support::Restricted),
            (transport("HTTP 429"), Support::Error),
            (error(-32000, "execution reverted"), Support::Error),
            (Err(ProbeFailure::Budget), Support::SkippedBudget),
        ];
        for (answer, support) in cases {
            assert_eq!(classify(&answer).0, support, "{answer:?}");
        }
        assert_eq!(
            classify(&error(-32601, "nope")).1.as_deref(),
            Some("rpc error -32601: nope")
        );
    }

    #[test]
    fn pruned_state_is_a_full_node() {
        assert_eq!(archive_of(&Ok(json!("0x0"))), Archive::Archive);
        assert_eq!(
            archive_of(&error(-32000, "missing trie node 1a2b (path )")),
            Archive::Full
        );
        assert_eq!(
            archive_of(&error(-32000, "header not found")),
            Archive::Unknown
        );
        assert_eq!(archive_of(&transport("timeout")), Archive::Unknown);
        assert_eq!(
            archive_of(&Err(ProbeFailure::Budget)),
            Archive::SkippedBudget
        );
    }

    #[test]
    fn rate_limits_are_recognized() {
        assert!(is_rate_limited(&RpcFailure::Transport("HTTP 429".into())));
        assert!(!is_rate_limited(&RpcFailure::Transport("HTTP 503".into())));
        let limit = |code, message: &str| RpcFailure::Error {
            code,
            message: message.into(),
            data: None,
        };
        assert!(is_rate_limited(&limit(-32005, "limit exceeded")));
        assert!(is_rate_limited(&limit(-32000, "Too Many Requests")));
        assert!(!is_rate_limited(&limit(-32000, "execution reverted")));
        assert_eq!(http_status("HTTP 401"), Some(401));
        assert_eq!(http_status("timeout"), None);
    }
}
