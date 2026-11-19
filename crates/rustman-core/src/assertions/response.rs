//! The parts of a response that assertions and captures read.

use std::borrow::Cow;
use std::time::Duration;

/// A completed response, independent of the transport that produced it.
#[derive(Clone, Debug, Default)]
pub struct ResponseView<'a> {
    pub status: u16,
    /// Header names and values in the order received; names may repeat.
    pub headers: Vec<(String, String)>,
    /// The whole decoded body.
    pub body: Cow<'a, [u8]>,
    /// The body is not valid UTF-8.
    pub binary: bool,
    /// Time to first byte.
    pub ttfb: Duration,
    /// Total time of the exchange.
    pub total: Duration,
    /// The outcome of a gRPC call; `None` for every other kind.
    pub grpc: Option<GrpcView>,
}

/// What a gRPC call adds to its response: the final status and the
/// trailers (the response headers are [`ResponseView::headers`]).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct GrpcView {
    pub code: i32,
    /// The canonical code name, such as `NOT_FOUND`.
    pub name: String,
    /// Trailer names and values, in the order received.
    pub trailers: Vec<(String, String)>,
}

impl GrpcView {
    /// `NOT_FOUND (5)`.
    pub fn label(&self) -> String {
        format!("{} ({})", self.name, self.code)
    }
}

impl ResponseView<'_> {
    /// Every value of header `name`, matched case-insensitively.
    pub fn header_values(&self, name: &str) -> Vec<&str> {
        self.headers
            .iter()
            .filter(|(n, _)| n.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
            .collect()
    }

    /// Every value of gRPC metadata `name` (case-insensitive): the response
    /// headers, then the trailers. Empty for a response of another kind.
    pub fn metadata_values(&self, name: &str) -> Vec<&str> {
        let Some(grpc) = &self.grpc else {
            return Vec::new();
        };
        let trailers = grpc
            .trailers
            .iter()
            .filter(|(n, _)| n.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str());
        let mut values = self.header_values(name);
        values.extend(trailers);
        values
    }

    /// The body as text, or `Err("body is binary")`.
    pub fn text(&self) -> Result<&str, &'static str> {
        if self.binary {
            return Err("body is binary");
        }
        std::str::from_utf8(&self.body).map_err(|_| "body is binary")
    }

    /// The body parsed as JSON, or `Err("response body is not JSON")`.
    pub fn json(&self) -> Result<serde_json::Value, &'static str> {
        const NOT_JSON: &str = "response body is not JSON";
        let text = self.text().map_err(|_| NOT_JSON)?;
        serde_json::from_str(text).map_err(|_| NOT_JSON)
    }

    /// The value of cookie `name` set by this response (`Set-Cookie`), the
    /// last one when it is set more than once.
    pub fn cookie(&self, name: &str) -> Option<&str> {
        self.header_values("set-cookie")
            .into_iter()
            .filter_map(|line| {
                let pair = line.split(';').next()?;
                let (n, v) = pair.split_once('=')?;
                (n.trim() == name).then(|| v.trim())
            })
            .next_back()
    }
}

/// A response with its body parsed as JSON at most once.
pub(crate) struct Parsed<'r, 'a> {
    pub view: &'r ResponseView<'a>,
    json: std::cell::OnceCell<Result<serde_json::Value, &'static str>>,
}

impl<'r, 'a> Parsed<'r, 'a> {
    pub fn new(view: &'r ResponseView<'a>) -> Self {
        Self {
            view,
            json: std::cell::OnceCell::new(),
        }
    }

    pub fn json(&self) -> Result<&serde_json::Value, &'static str> {
        self.json
            .get_or_init(|| self.view.json())
            .as_ref()
            .map_err(|e| *e)
    }
}
