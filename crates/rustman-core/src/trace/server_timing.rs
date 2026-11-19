//! The `Server-Timing` response header (design D5): a lenient parser that
//! keeps each metric's name, first `dur` and first `desc`.

use serde::{Deserialize, Serialize};

pub const SERVER_TIMING: &str = "server-timing";

/// One `Server-Timing` metric, such as `db;dur=53.2;desc="query"`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ServerTiming {
    pub name: String,
    /// `None` when the metric carries no valid `dur`.
    pub dur_ms: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub desc: Option<String>,
}

/// Every metric of `values` (the header may repeat), in order. A malformed
/// metric is skipped; the others are kept.
pub fn parse<'a>(values: impl IntoIterator<Item = &'a str>) -> Vec<ServerTiming> {
    parse_with(values, |_| {})
}

/// [`parse`], calling `skipped` with each malformed metric (callers log it).
pub fn parse_with<'a>(
    values: impl IntoIterator<Item = &'a str>,
    mut skipped: impl FnMut(&str),
) -> Vec<ServerTiming> {
    values
        .into_iter()
        .flat_map(split_unquoted)
        .filter_map(|metric| {
            let parsed = parse_metric(metric);
            if parsed.is_none() {
                skipped(metric);
            }
            parsed
        })
        .collect()
}

/// `value` split on commas outside quoted strings, without empty parts.
fn split_unquoted(value: &str) -> Vec<&str> {
    split_outside_quotes(value, ',')
        .into_iter()
        .map(str::trim)
        .filter(|part| !part.is_empty())
        .collect()
}

fn split_outside_quotes(value: &str, separator: char) -> Vec<&str> {
    let mut parts = Vec::new();
    let (mut start, mut quoted, mut escaped) = (0, false, false);
    for (i, c) in value.char_indices() {
        match c {
            _ if escaped => escaped = false,
            '\\' if quoted => escaped = true,
            '"' => quoted = !quoted,
            c if c == separator && !quoted => {
                parts.push(&value[start..i]);
                start = i + c.len_utf8();
            }
            _ => {}
        }
    }
    parts.push(&value[start..]);
    parts
}

fn parse_metric(metric: &str) -> Option<ServerTiming> {
    let mut params = split_outside_quotes(metric, ';').into_iter().map(str::trim);
    let name = params.next().filter(|n| is_token(n))?.to_owned();
    let mut timing = ServerTiming {
        name,
        dur_ms: None,
        desc: None,
    };
    for param in params {
        let (key, value) = param.split_once('=')?;
        let value = unquote(value.trim())?;
        match key.trim().to_ascii_lowercase().as_str() {
            "dur" if timing.dur_ms.is_none() => {
                timing.dur_ms = Some(value.parse::<f64>().ok().filter(|d| d.is_finite())?);
            }
            "desc" if timing.desc.is_none() => timing.desc = Some(value),
            _ => {}
        }
    }
    Some(timing)
}

/// A token as is, or a quoted string with its escapes removed.
fn unquote(value: &str) -> Option<String> {
    let Some(inner) = value.strip_prefix('"') else {
        return is_token(value).then(|| value.to_owned());
    };
    let inner = inner.strip_suffix('"')?;
    let mut out = String::with_capacity(inner.len());
    let mut chars = inner.chars();
    while let Some(c) = chars.next() {
        out.push(if c == '\\' { chars.next()? } else { c });
    }
    Some(out)
}

fn is_token(text: &str) -> bool {
    !text.is_empty()
        && text
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || b"!#$%&'*+-.^_`|~".contains(&c))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn timing(name: &str, dur: Option<f64>, desc: Option<&str>) -> ServerTiming {
        ServerTiming {
            name: name.to_owned(),
            dur_ms: dur,
            desc: desc.map(str::to_owned),
        }
    }

    #[test]
    fn valid_metrics_keep_name_duration_and_description() {
        assert_eq!(
            parse(["db;dur=53.2, app;dur=12"]),
            [
                timing("db", Some(53.2), None),
                timing("app", Some(12.0), None)
            ]
        );
        assert_eq!(
            parse([
                r#"cache;desc="Cache Read";dur=23.2"#,
                "miss",
                "total;dur=1;dur=9"
            ]),
            [
                timing("cache", Some(23.2), Some("Cache Read")),
                timing("miss", None, None),
                timing("total", Some(1.0), None),
            ]
        );
        assert_eq!(
            parse([r#"q;desc="a, \"b\"; c";dur=2"#]),
            [timing("q", Some(2.0), Some(r#"a, "b"; c"#))]
        );
        assert_eq!(
            parse(["edge;DUR=3;Desc=fra"]),
            [timing("edge", Some(3.0), Some("fra"))]
        );
    }

    #[test]
    fn malformed_metrics_are_skipped() {
        assert_eq!(
            parse(["db;dur=abc, ok;dur=1, ;dur=2, bad name;dur=3, q;desc=\"open, x;dur"]),
            [timing("ok", Some(1.0), None)]
        );
        assert!(parse(["", " , ", "a;dur", "b;dur=inf"]).is_empty());
        assert!(parse(Vec::<&str>::new()).is_empty());
        let mut skipped = Vec::new();
        parse_with(["a;dur=x, b"], |m| skipped.push(m.to_owned()));
        assert_eq!(skipped, ["a;dur=x"]);
    }
}
