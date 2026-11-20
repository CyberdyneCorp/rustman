//! The test cases of a plugin, `rustman-plugin-tests.toml` (plugins spec,
//! "Host test harness"): fakes shared by every case, and per case the
//! grants, one call into an extension point and what its output must hold.

use std::collections::BTreeMap;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::Deserialize;

use crate::limits::LimitsTable;

/// The test cases file, next to the plugin's manifest.
pub const TESTS_FILE: &str = "rustman-plugin-tests.toml";

/// The fake clock, randomness, environment, secrets and network a case
/// runs against. Nothing real is reachable from a test.
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Fakes {
    /// `2026-01-01T00:00:00Z` (a TOML datetime or a string, UTC) or Unix
    /// seconds; 1970-01-01T00:00:00Z when unset.
    pub clock: Option<toml::Value>,
    /// The seed of the deterministic random bytes; 0 when unset.
    pub random_seed: Option<u64>,
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    #[serde(default)]
    pub secrets: BTreeMap<String, String>,
    /// Canned responses, matched by method and exact URL.
    #[serde(default)]
    pub http: Vec<FakeResponse>,
}

impl Fakes {
    /// These fakes with `case`'s on top: its clock and seed win, its maps
    /// add to and replace entries, its responses are matched first.
    pub fn merged(&self, case: &Fakes) -> Fakes {
        let mut env = self.env.clone();
        env.extend(case.env.clone());
        let mut secrets = self.secrets.clone();
        secrets.extend(case.secrets.clone());
        let mut http = case.http.clone();
        http.extend(self.http.iter().cloned());
        Fakes {
            clock: case.clock.clone().or_else(|| self.clock.clone()),
            random_seed: case.random_seed.or(self.random_seed),
            env,
            secrets,
            http,
        }
    }

    /// The fixed time of the fake clock.
    pub fn now(&self) -> Result<SystemTime, String> {
        match &self.clock {
            None => Ok(UNIX_EPOCH),
            Some(toml::Value::Integer(seconds)) => u64::try_from(*seconds)
                .map(|s| UNIX_EPOCH + Duration::from_secs(s))
                .map_err(|_| format!("clock {seconds} is before 1970")),
            Some(toml::Value::String(text)) => parse_utc(text),
            Some(toml::Value::Datetime(datetime)) => parse_utc(&datetime.to_string()),
            Some(other) => Err(format!("clock {other} is not a time")),
        }
    }
}

/// One canned response of the fake network.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FakeResponse {
    #[serde(default = "get")]
    pub method: String,
    pub url: String,
    #[serde(default = "ok")]
    pub status: u16,
    #[serde(default)]
    pub headers: BTreeMap<String, String>,
    #[serde(default)]
    pub body: String,
}

/// The whole file.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TestFile {
    #[serde(default)]
    pub fakes: Fakes,
    #[serde(default, rename = "case")]
    pub cases: Vec<RawCase>,
}

/// One `[[case]]` as written: exactly one of the call tables is set.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RawCase {
    pub name: String,
    #[serde(default)]
    pub grants: Vec<String>,
    #[serde(default)]
    pub limits: LimitsTable,
    #[serde(default)]
    pub fakes: Fakes,
    /// Output lines that must have exactly these values, such as
    /// `"header.X-Timestamp" = "1767225600"` or `value = "acme"`.
    #[serde(default)]
    pub expect: BTreeMap<String, String>,
    /// The call must fail with a message containing this text.
    pub expect_error: Option<String>,
    sign: Option<SignCall>,
    check: Option<CheckCall>,
    generate: Option<GenerateCall>,
    transform_request: Option<TransformRequestCall>,
    transform_response: Option<TransformResponseCall>,
    convert: Option<ConvertCall>,
    execute: Option<ExecuteCall>,
    render: Option<RenderCall>,
}

impl RawCase {
    /// The case's one call.
    pub fn call(&self) -> Result<Call, String> {
        let calls: Vec<Call> = [
            self.sign.clone().map(Call::Sign),
            self.check.clone().map(Call::Check),
            self.generate.clone().map(Call::Generate),
            self.transform_request.clone().map(Call::TransformRequest),
            self.transform_response.clone().map(Call::TransformResponse),
            self.convert.clone().map(Call::Convert),
            self.execute.clone().map(Call::Execute),
            self.render.clone().map(Call::Render),
        ]
        .into_iter()
        .flatten()
        .collect();
        match <[Call; 1]>::try_from(calls) {
            Ok([call]) => Ok(call),
            Err(calls) => Err(format!(
                "case '{}' must have exactly one of sign, check, generate, transform_request, transform_response, convert, execute or render ({} found)",
                self.name,
                calls.len()
            )),
        }
    }
}

/// A call into one extension point.
#[derive(Clone, Debug)]
pub enum Call {
    Sign(SignCall),
    Check(CheckCall),
    Generate(GenerateCall),
    TransformRequest(TransformRequestCall),
    TransformResponse(TransformResponseCall),
    Convert(ConvertCall),
    Execute(ExecuteCall),
    Render(RenderCall),
}

/// `auth-scheme.sign`: `scheme` is the plugin's own name for it (`v1`).
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SignCall {
    pub scheme: String,
    #[serde(default = "get")]
    pub method: String,
    pub url: String,
    #[serde(default)]
    pub headers: BTreeMap<String, String>,
    #[serde(default)]
    pub body: String,
    #[serde(default)]
    pub params: BTreeMap<String, String>,
}

/// `assertion-fn.check`, with the exchange's parts and `args` as a JSON
/// object.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CheckCall {
    pub function: String,
    #[serde(default = "get")]
    pub method: String,
    #[serde(default = "local_url")]
    pub url: String,
    #[serde(default)]
    pub request_headers: BTreeMap<String, String>,
    #[serde(default)]
    pub request_body: String,
    #[serde(default = "ok")]
    pub status: u16,
    #[serde(default)]
    pub headers: BTreeMap<String, String>,
    #[serde(default)]
    pub body: String,
    #[serde(default)]
    pub duration_ms: u64,
    #[serde(default = "empty_object")]
    pub args: String,
}

/// `value-generator.generate`.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GenerateCall {
    pub name: String,
    #[serde(default)]
    pub args: Vec<String>,
}

/// `body-transformer.transform-request`.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TransformRequestCall {
    pub name: String,
    #[serde(default = "local_url")]
    pub url: String,
    #[serde(default)]
    pub headers: BTreeMap<String, String>,
    #[serde(default)]
    pub body: String,
}

/// `body-transformer.transform-response`.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TransformResponseCall {
    pub name: String,
    #[serde(default = "ok")]
    pub status: u16,
    #[serde(default)]
    pub headers: BTreeMap<String, String>,
    #[serde(default)]
    pub body: String,
}

/// `format-importer.convert`, with the source's content inline.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConvertCall {
    pub format: String,
    pub source_path: String,
    pub source: String,
}

/// `request-kind.execute`, with the request file as JSON.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExecuteCall {
    pub kind: String,
    pub request: String,
}

/// `report-renderer.render`, with the report document as JSON.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RenderCall {
    pub format: String,
    pub report: String,
}

fn get() -> String {
    "GET".to_owned()
}

fn ok() -> u16 {
    200
}

fn local_url() -> String {
    "http://localhost/".to_owned()
}

fn empty_object() -> String {
    "{}".to_owned()
}

/// `YYYY-MM-DDTHH:MM:SSZ` (optionally with fractional seconds) as a time.
fn parse_utc(text: &str) -> Result<SystemTime, String> {
    let invalid = || format!("clock '{text}' is not YYYY-MM-DDTHH:MM:SSZ");
    let body = text.strip_suffix('Z').ok_or_else(invalid)?;
    let body = body.split('.').next().unwrap_or(body);
    let (date, time) = body.split_once('T').ok_or_else(invalid)?;
    let numbers = |part: &str, sep: char| -> Option<[i64; 3]> {
        let parsed: Option<Vec<i64>> = part.split(sep).map(|n| n.parse().ok()).collect();
        parsed?.try_into().ok()
    };
    let [year, month, day] = numbers(date, '-').ok_or_else(invalid)?;
    let [hour, minute, second] = numbers(time, ':').ok_or_else(invalid)?;
    let in_range = (1..=12).contains(&month)
        && (1..=31).contains(&day)
        && (0..24).contains(&hour)
        && (0..60).contains(&minute)
        && (0..61).contains(&second);
    if !in_range {
        return Err(invalid());
    }
    let days = days_from_civil(year, month, day);
    let seconds = days * 86_400 + hour * 3600 + minute * 60 + second;
    u64::try_from(seconds)
        .map(|s| UNIX_EPOCH + Duration::from_secs(s))
        .map_err(|_| invalid())
}

/// Days since 1970-01-01 of a proleptic Gregorian date.
fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let year = if month <= 2 { year - 1 } else { year };
    let era = year.div_euclid(400);
    let year_of_era = year - era * 400;
    let month_index = (month + 9) % 12;
    let day_of_year = (153 * month_index + 2) / 5 + day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    era * 146_097 + day_of_era - 719_468
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seconds(time: SystemTime) -> u64 {
        time.duration_since(UNIX_EPOCH).unwrap().as_secs()
    }

    #[test]
    fn clocks_read_as_utc() {
        let fakes = |clock: &str| -> Fakes { toml::from_str(&format!("clock = {clock}")).unwrap() };
        assert_eq!(
            seconds(fakes("\"2026-01-01T00:00:00Z\"").now().unwrap()),
            1_767_225_600
        );
        assert_eq!(
            seconds(fakes("2026-01-01T00:00:00Z").now().unwrap()),
            1_767_225_600
        );
        assert_eq!(
            seconds(fakes("2000-02-29T12:30:01.5Z").now().unwrap()),
            951_827_401
        );
        assert_eq!(seconds(fakes("86400").now().unwrap()), 86_400);
        assert_eq!(seconds(Fakes::default().now().unwrap()), 0);
        assert!(fakes("\"2026-01-01 00:00\"").now().is_err());
        assert!(fakes("-1").now().is_err());
        assert!(fakes("true").now().is_err());
    }

    #[test]
    fn case_fakes_override_the_shared_ones() {
        let file: TestFile = toml::from_str(
            r#"
[fakes]
clock = 10
env = { A = "1", B = "2" }
http = [{ url = "https://x.example/" }]

[[case]]
name = "c"
fakes = { env = { B = "3" }, http = [{ url = "https://y.example/", status = 201 }] }
generate = { name = "n" }
"#,
        )
        .unwrap();
        let merged = file.fakes.merged(&file.cases[0].fakes);
        assert_eq!(seconds(merged.now().unwrap()), 10);
        assert_eq!(merged.env["A"], "1");
        assert_eq!(merged.env["B"], "3");
        let urls: Vec<&str> = merged.http.iter().map(|r| r.url.as_str()).collect();
        assert_eq!(urls, ["https://y.example/", "https://x.example/"]);
        assert_eq!(merged.http[1].method, "GET");
        assert!(matches!(file.cases[0].call(), Ok(Call::Generate(_))));
    }

    #[test]
    fn a_case_has_exactly_one_call() {
        let file: TestFile = toml::from_str(
            "[[case]]\nname = \"none\"\n\n[[case]]\nname = \"two\"\ngenerate = { name = \"a\" }\nrender = { format = \"f\", report = \"{}\" }\n",
        )
        .unwrap();
        let errors: Vec<String> = file.cases.iter().map(|c| c.call().unwrap_err()).collect();
        assert!(errors[0].ends_with("(0 found)"), "{}", errors[0]);
        assert!(errors[1].ends_with("(2 found)"), "{}", errors[1]);
        let typo: Result<TestFile, _> = toml::from_str("[[case]]\nname = \"x\"\nsgin = {}\n");
        assert!(typo.is_err());
    }
}
