//! Recorded exchanges, read from history entries or a run report. Both are
//! already redacted when they are recorded.

use rustman_core::history::{Entry, HistoryError, HistoryStore, Query};
use rustman_core::run::HeaderRecord;
use rustman_report::run::{Response, RunReport};

/// One recorded request and its response.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RecordedExchange {
    /// The history entry id, or `<request id>#<iteration>` in a run report.
    pub id: String,
    /// Where it was read from: a history entry file or `<report>#/requests/<n>`.
    pub location: String,
    pub method: String,
    /// The URL with its query string.
    pub url: String,
    pub request_headers: Vec<(String, String)>,
    pub request_body: Option<String>,
    pub response: Option<RecordedResponse>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RecordedResponse {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    /// The text body; `None` when it was binary or only its start was kept.
    pub body: Option<String>,
}

fn pairs(headers: &[HeaderRecord]) -> Vec<(String, String)> {
    headers
        .iter()
        .map(|h| (h.name.clone(), h.value.clone()))
        .collect()
}

fn response(response: &Response) -> RecordedResponse {
    let whole = !response.body_truncated && !response.binary;
    RecordedResponse {
        status: response.status,
        headers: pairs(&response.headers),
        body: response.body.clone().filter(|_| whole),
    }
}

impl RecordedExchange {
    /// A history entry; `location` names its file.
    pub fn from_entry(entry: &Entry, location: String) -> Self {
        let recorded = entry.record.pointer("/exchange/response").cloned();
        let response = recorded
            .and_then(|value| serde_json::from_value::<Response>(value).ok())
            .map(|r| self::response(&r));
        Self {
            id: entry.id.clone(),
            location,
            method: entry.sent.method.to_uppercase(),
            url: entry.sent.url.clone(),
            request_headers: pairs(&entry.sent.headers),
            request_body: entry.sent.body.clone(),
            response,
        }
    }

    /// The exchanges of a run report named `name`, in run order.
    pub fn from_run_report(report: &RunReport, name: &str) -> Vec<Self> {
        report
            .requests
            .iter()
            .enumerate()
            .filter_map(|(index, request)| {
                let exchange = request.exchange.as_ref()?;
                Some(Self {
                    id: format!("{}#{}", request.id, request.iteration),
                    location: format!("{name}#/requests/{index}"),
                    method: exchange.request.method.to_uppercase(),
                    url: exchange.request.url.clone(),
                    request_headers: pairs(&exchange.request.headers),
                    request_body: exchange.request.body.clone(),
                    response: Some(response(&exchange.response)),
                })
            })
            .collect()
    }

    /// The path (without origin and query) and the raw query string.
    pub fn path_and_query(&self) -> (&str, Option<&str>) {
        let rest = match self.url.split_once("://") {
            Some((_, rest)) => rest.find('/').map_or("/", |at| &rest[at..]),
            None => self.url.as_str(),
        };
        let rest = rest.split_once('#').map_or(rest, |(before, _)| before);
        match rest.split_once('?') {
            Some((path, query)) => (path, Some(query)),
            None => (rest, None),
        }
    }
}

/// The history entries `query` selects, oldest first, and why any entry
/// was skipped.
pub fn from_history(
    store: &HistoryStore,
    query: &Query,
) -> Result<(Vec<RecordedExchange>, Vec<String>), HistoryError> {
    let listing = store.list()?;
    let mut exchanges: Vec<RecordedExchange> = query
        .apply(listing.entries)
        .iter()
        .map(|entry| {
            let file = store.dir().join(format!("{}.json", entry.id));
            RecordedExchange::from_entry(entry, file.display().to_string())
        })
        .collect();
    exchanges.reverse();
    Ok((exchanges, listing.warnings))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn urls_split_into_path_and_query() {
        let exchange = |url: &str| RecordedExchange {
            url: url.to_owned(),
            ..RecordedExchange::default()
        };
        assert_eq!(
            exchange("http://h:8/users/7?a=1").path_and_query(),
            ("/users/7", Some("a=1"))
        );
        assert_eq!(exchange("https://h").path_and_query(), ("/", None));
        assert_eq!(exchange("/x#frag").path_and_query(), ("/x", None));
    }
}
