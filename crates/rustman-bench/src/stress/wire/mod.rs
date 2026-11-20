//! Wire-fault families (stress-testing "Wire-fault families", "Slow-client
//! bounds", design D6): raw HTTP/1.1 that `hyper` would refuse to send,
//! written by the private `writer` module (the one place that writes raw
//! HTTP bytes) over the engine's connector.
//!
//! Wire faults run only with the stress gate's [`WireConsent`] (the target
//! listed in `[stress] targets`, never production, `--allow-wire-faults`),
//! at most `max_wire_connections` sockets at a time whatever the mode,
//! after the load phase. They are never available to agents.
//!
//! | Family | Sent | Finding |
//! |--------|------|---------|
//! | `oversized-headers` | a 70 KiB header | `connection-dropped-without-response` (low, 431 expected) |
//! | `many-headers` | 1000 headers | `connection-dropped-without-response` (low, 431 expected) |
//! | `cl-te-conflict` | `Content-Length` and `Transfer-Encoding: chunked`, the body complete only by `Content-Length` | a 2xx: `ambiguous-framing-accepted` (high, 400 expected) |
//! | `invalid-chunked` | a chunk size that is not hex | a 2xx: `ambiguous-framing-accepted` |
//! | `invalid-request-line` | a garbage request line | a 2xx: `ambiguous-framing-accepted` |
//! | `slow-body` | a body at 1 byte/s for at most 60 s | every connection held open: `no-slow-client-timeout` (low) |
//!
//! A family whose connections the server closed without an answer also
//! gets `connection-dropped-without-response`.

mod writer;

use std::fmt;
use std::str::FromStr;
use std::sync::Arc;
use std::time::Duration;

use http::Uri;
use rustman_core::safety::Origin;
use rustman_core::stress::WIRE_FAMILIES;
use rustman_http::{SendOptions, open_raw_stream};
use rustman_report::exploration::Severity;
use tokio::sync::Semaphore;
use tokio_util::sync::CancellationToken;

use crate::stress::{Evidence, StressFinding};

pub const AMBIGUOUS_FRAMING: &str = "ambiguous-framing-accepted";
pub const DROPPED: &str = "connection-dropped-without-response";
pub const NO_SLOW_CLIENT_TIMEOUT: &str = "no-slow-client-timeout";

/// Requests sent per family (all but `slow-body`).
pub const ATTEMPTS: usize = 3;
/// The longest a `slow-body` connection is held.
pub const MAX_SLOW_BODY: Duration = Duration::from_secs(60);

/// A wire-fault family.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum WireFamily {
    OversizedHeaders,
    ManyHeaders,
    SlowBody,
    ClTeConflict,
    InvalidChunked,
    InvalidRequestLine,
}

impl WireFamily {
    pub const ALL: [Self; 6] = [
        Self::OversizedHeaders,
        Self::ManyHeaders,
        Self::SlowBody,
        Self::ClTeConflict,
        Self::InvalidChunked,
        Self::InvalidRequestLine,
    ];

    /// The name of `--wire`.
    pub fn name(self) -> &'static str {
        let index = Self::ALL.iter().position(|f| *f == self).unwrap_or(0);
        WIRE_FAMILIES[index]
    }

    /// The status a correct server answers.
    pub fn expected_status(self) -> u16 {
        match self {
            Self::OversizedHeaders | Self::ManyHeaders => 431,
            Self::SlowBody => 408,
            _ => 400,
        }
    }

    /// Whether a 2xx answer means the server accepted broken framing.
    fn is_framing(self) -> bool {
        matches!(
            self,
            Self::ClTeConflict | Self::InvalidChunked | Self::InvalidRequestLine
        )
    }
}

impl fmt::Display for WireFamily {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

impl FromStr for WireFamily {
    type Err = String;

    fn from_str(text: &str) -> Result<Self, String> {
        Self::ALL
            .into_iter()
            .find(|f| f.name() == text)
            .ok_or_else(|| format!("unknown wire family '{text}'"))
    }
}

/// The stress gate's permission to send wire faults: built only by
/// [`crate::stress::gate::wire_consent`], once every rule passed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WireConsent {
    target: Origin,
    families: Vec<WireFamily>,
    max_connections: u32,
}

impl WireConsent {
    pub(crate) fn new(target: Origin, families: Vec<WireFamily>, max_connections: u32) -> Self {
        Self {
            target,
            families,
            max_connections: max_connections.max(1),
        }
    }

    pub fn families(&self) -> &[WireFamily] {
        &self.families
    }

    pub fn max_connections(&self) -> u32 {
        self.max_connections
    }
}

/// How a wire-fault request ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WireOutcome {
    Status(u16),
    /// Closed without an answer.
    Closed,
    /// Reset, or the connection failed.
    Reset,
    /// No answer within the read timeout.
    TimedOut,
    /// An answer that is not an HTTP/1.x status line.
    Unreadable,
}

impl fmt::Display for WireOutcome {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Status(code) => write!(f, "{code}"),
            Self::Closed => f.write_str("closed without a response"),
            Self::Reset => f.write_str("connection reset"),
            Self::TimedOut => f.write_str("no response in time"),
            Self::Unreadable => f.write_str("not an HTTP/1.x status line"),
        }
    }
}

/// The clocks of the wire phase; tests shorten them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WireTuning {
    /// How long a status line may take.
    pub read_timeout: Duration,
    /// How long `slow-body` holds its connections (at most
    /// [`MAX_SLOW_BODY`]).
    pub slow_body_for: Duration,
}

impl Default for WireTuning {
    fn default() -> Self {
        Self {
            read_timeout: Duration::from_secs(10),
            slow_body_for: MAX_SLOW_BODY,
        }
    }
}

/// What one family observed.
#[derive(Clone, Debug, PartialEq)]
pub struct WireResult {
    pub family: WireFamily,
    /// One outcome per request (all but `slow-body`).
    pub outcomes: Vec<WireOutcome>,
    /// `slow-body`: per connection, when the server ended it (`None`: held
    /// open for the whole hold).
    pub slow: Vec<Option<Duration>>,
    /// `slow-body`: how long connections were held.
    pub hold: Duration,
}

/// Send every consented family to the target, at most
/// `consent.max_connections()` sockets at a time, then judge the answers.
pub async fn run(
    consent: &WireConsent,
    options: &SendOptions,
    tuning: &WireTuning,
    stop: &CancellationToken,
) -> (Vec<WireResult>, Vec<StressFinding>) {
    let uri: Uri = consent
        .target
        .to_string()
        .parse()
        .expect("an origin is a valid URI");
    let host = uri
        .authority()
        .map_or_else(String::new, ToString::to_string);
    let limit = Arc::new(Semaphore::new(consent.max_connections as usize));
    let ctx = Ctx {
        uri: &uri,
        host: &host,
        options,
        tuning,
        limit: &limit,
        stop,
    };
    let mut results = Vec::new();
    for family in &consent.families {
        if stop.is_cancelled() {
            break;
        }
        results.push(ctx.family(*family, consent.max_connections).await);
    }
    let findings = results.iter().flat_map(|r| judge(r, &host)).collect();
    (results, findings)
}

struct Ctx<'a> {
    uri: &'a Uri,
    host: &'a str,
    options: &'a SendOptions,
    tuning: &'a WireTuning,
    limit: &'a Semaphore,
    stop: &'a CancellationToken,
}

impl Ctx<'_> {
    async fn family(&self, family: WireFamily, connections: u32) -> WireResult {
        let hold = self.tuning.slow_body_for.min(MAX_SLOW_BODY);
        let mut result = WireResult {
            family,
            outcomes: Vec::new(),
            slow: Vec::new(),
            hold,
        };
        if family == WireFamily::SlowBody {
            let all = (0..connections).map(|_| self.slow(hold));
            result.slow = futures_util::future::join_all(all).await;
        } else {
            let all = (0..ATTEMPTS).map(|_| self.attempt(family));
            let outcomes = futures_util::future::join_all(all).await;
            result.outcomes = outcomes.into_iter().flatten().collect();
        }
        result
    }

    /// One request of `family`; `None` when the run stopped first (its
    /// socket is closed at once, and it judges nothing).
    async fn attempt(&self, family: WireFamily) -> Option<WireOutcome> {
        tokio::select! {
            biased;
            () = self.stop.cancelled() => None,
            outcome = self.exchange(family) => Some(outcome),
        }
    }

    async fn exchange(&self, family: WireFamily) -> WireOutcome {
        let Ok(_permit) = self.limit.acquire().await else {
            return WireOutcome::Reset;
        };
        let bytes = writer::request_bytes(family, self.host).unwrap_or_default();
        match open_raw_stream(self.uri, self.options).await {
            Ok(stream) => writer::exchange(stream, &bytes, self.tuning.read_timeout).await,
            Err(_) => WireOutcome::Reset,
        }
    }

    async fn slow(&self, hold: Duration) -> Option<Duration> {
        let Ok(_permit) = self.limit.acquire().await else {
            return Some(Duration::ZERO);
        };
        match open_raw_stream(self.uri, self.options).await {
            Ok(stream) => writer::slow_body(stream, self.host, hold, self.stop).await,
            Err(_) => Some(Duration::ZERO),
        }
    }
}

/// The findings of one family's results.
pub fn judge(result: &WireResult, host: &str) -> Vec<StressFinding> {
    let family = result.family;
    if family == WireFamily::SlowBody {
        return slow_body_finding(result).into_iter().collect();
    }
    let mut findings = Vec::new();
    let accepted = result.outcomes.iter().find_map(|o| match o {
        WireOutcome::Status(code @ 200..=299) => Some(*code),
        _ => None,
    });
    if let (true, Some(code)) = (family.is_framing(), accepted) {
        findings.push(finding(
            AMBIGUOUS_FRAMING,
            Severity::High,
            family,
            format!("{family}: the server answered {code}; it should refuse the request with 400"),
            &WireOutcome::Status(code),
            host,
        ));
    }
    let dropped = result
        .outcomes
        .iter()
        .find(|o| matches!(o, WireOutcome::Closed | WireOutcome::Reset));
    if let Some(outcome) = dropped {
        let expected = family.expected_status();
        findings.push(finding(
            DROPPED,
            Severity::Low,
            family,
            format!("{family}: the server dropped the connection without a response; it should answer {expected}"),
            outcome,
            host,
        ));
    }
    findings
}

fn slow_body_finding(result: &WireResult) -> Option<StressFinding> {
    if result.slow.is_empty() || result.slow.iter().any(Option::is_some) {
        return None;
    }
    let message = format!(
        "slow-body: the server kept all {} slow connections open for {}s (1 byte/s); it should time slow clients out (408)",
        result.slow.len(),
        result.hold.as_secs()
    );
    let mut finding = StressFinding::new(NO_SLOW_CLIENT_TIMEOUT, Severity::Low, message);
    finding.family = Some(WireFamily::SlowBody.name().to_owned());
    Some(finding)
}

fn finding(
    rule: &'static str,
    severity: Severity,
    family: WireFamily,
    message: String,
    outcome: &WireOutcome,
    host: &str,
) -> StressFinding {
    let bytes = writer::request_bytes(family, host).unwrap_or_default();
    let line = bytes.split(|b| *b == b'\n').next().unwrap_or_default();
    let request = String::from_utf8_lossy(line)
        .trim_end()
        .escape_debug()
        .to_string();
    StressFinding {
        rule,
        severity,
        message,
        family: Some(family.name().to_owned()),
        evidence: Some(Evidence {
            request,
            headers: Vec::new(),
            variant: format!("wire: {family}"),
            body: String::new(),
            response: outcome.to_string(),
            ..Evidence::default()
        }),
        file: None,
        minimization: None,
        input: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn result(family: WireFamily, outcomes: Vec<WireOutcome>) -> WireResult {
        WireResult {
            family,
            outcomes,
            slow: Vec::new(),
            hold: Duration::from_secs(60),
        }
    }

    fn rules(result: &WireResult) -> Vec<&'static str> {
        judge(result, "h").iter().map(|f| f.rule).collect()
    }

    #[test]
    fn families_round_trip_their_names() {
        for family in WireFamily::ALL {
            assert_eq!(family.name().parse::<WireFamily>(), Ok(family));
        }
    }

    #[test]
    fn accepted_framing_and_dropped_connections_are_findings() {
        let ok = WireOutcome::Status(200);
        let bad = WireOutcome::Status(400);
        assert_eq!(
            rules(&result(WireFamily::ClTeConflict, vec![bad, ok])),
            [AMBIGUOUS_FRAMING]
        );
        assert_eq!(
            rules(&result(WireFamily::ClTeConflict, vec![bad; 3])),
            Vec::<&str>::new()
        );
        // A large header answered 200 is fine: only framing must be refused.
        assert_eq!(
            rules(&result(WireFamily::OversizedHeaders, vec![ok])),
            Vec::<&str>::new()
        );
        let dropped = result(WireFamily::OversizedHeaders, vec![WireOutcome::Closed]);
        let findings = judge(&dropped, "h");
        assert_eq!(findings[0].rule, DROPPED);
        assert_eq!(findings[0].severity, Severity::Low);
        assert!(
            findings[0].message.contains("431"),
            "{}",
            findings[0].message
        );
        let evidence = findings[0].evidence.as_ref().unwrap();
        assert_eq!(evidence.request, "GET / HTTP/1.1");
        assert_eq!(evidence.response, "closed without a response");
    }

    #[test]
    fn slow_clients_held_open_for_the_whole_hold_are_a_finding() {
        let mut held = result(WireFamily::SlowBody, Vec::new());
        held.slow = vec![None; 4];
        assert_eq!(rules(&held), [NO_SLOW_CLIENT_TIMEOUT]);
        held.slow[2] = Some(Duration::from_secs(15));
        assert_eq!(rules(&held), Vec::<&str>::new());
    }
}
