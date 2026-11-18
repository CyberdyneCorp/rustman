//! HAR 1.2 captures (task 4.1).
//!
//! `log.entries` is read one entry at a time (a streaming visitor), and only
//! the request and the response MIME type are kept: response bodies are
//! skipped by the parser and never stored. Entries are filtered (static
//! assets, `--host`, `--method`, `--content-type`), deduplicated by method,
//! normalized URL and body hash (design D5), and grouped by host.

use std::collections::HashSet;
use std::fmt;
use std::hash::{DefaultHasher, Hash, Hasher};
use std::path::Path;

use serde::Deserialize;
use serde::de::{self, DeserializeSeed, IgnoredAny, MapAccess, SeqAccess, Visitor};

use crate::common::{self, path_of, split_url, text_body};
use crate::error::ImportError;
use crate::ir::{
    Folder, ImportedBody, ImportedCollection, ImportedPart, ImportedRequest, Item, Location, Note,
    NoteKind, Pair, PartValue,
};

/// Which entries to import. Empty lists accept everything.
#[derive(Clone, Debug, Default)]
pub struct HarFilter {
    /// Hosts to keep (`api.example.com`, or with a port), case-insensitive.
    pub hosts: Vec<String>,
    /// Methods to keep, case-insensitive.
    pub methods: Vec<String>,
    /// MIME types (`application/json`) of the request or response to keep.
    pub content_types: Vec<String>,
    /// Keep images, fonts, stylesheets and scripts.
    pub include_static: bool,
}

/// Whether `doc` is a HAR file: an object with `log.entries`.
pub fn is_har(doc: &serde_json::Value) -> bool {
    doc["log"]["entries"].is_array()
}

#[derive(Deserialize)]
struct Entry {
    request: Request,
    #[serde(default)]
    response: Response,
}

#[derive(Deserialize)]
struct Request {
    method: String,
    url: String,
    #[serde(default)]
    headers: Vec<NameValue>,
    /// The decoded query parameters, a copy of the URL's in browser
    /// exports; used only when the URL has no query.
    #[serde(rename = "queryString", default)]
    query_string: Vec<NameValue>,
    #[serde(rename = "postData", default)]
    post_data: Option<PostData>,
}

#[derive(Deserialize)]
struct NameValue {
    name: String,
    #[serde(default)]
    value: String,
}

#[derive(Deserialize)]
struct PostData {
    #[serde(rename = "mimeType", default)]
    mime_type: String,
    #[serde(default)]
    text: Option<String>,
    #[serde(default)]
    params: Vec<Param>,
}

#[derive(Deserialize)]
struct Param {
    name: String,
    #[serde(default)]
    value: Option<String>,
    #[serde(rename = "fileName", default)]
    file_name: Option<String>,
    #[serde(rename = "contentType", default)]
    content_type: Option<String>,
}

#[derive(Default, Deserialize)]
struct Response {
    #[serde(default)]
    content: Content,
}

/// Only the MIME type: `text` (the body) is skipped by the parser.
#[derive(Default, Deserialize)]
struct Content {
    #[serde(rename = "mimeType", default)]
    mime_type: String,
}

/// Parse the HAR `text`, read from `file`, keeping the entries `filter`
/// accepts.
pub fn parse(
    text: &str,
    file: &str,
    filter: &HarFilter,
) -> Result<ImportedCollection, ImportError> {
    let mut builder = Builder::new(file, filter);
    let seed = Field {
        key: "log",
        seed: Field {
            key: "entries",
            seed: Entries(&mut |entry| builder.add(entry)),
        },
    };
    let mut de = serde_json::Deserializer::from_str(text);
    seed.deserialize(&mut de)
        .and_then(|()| de.end())
        .map_err(|e| ImportError::invalid(file, e.to_string()))?;
    Ok(builder.finish())
}

/// Visits a map, handing the value of `key` to `seed` and skipping the rest.
struct Field<S> {
    key: &'static str,
    seed: S,
}

impl<'de, S: DeserializeSeed<'de, Value = ()>> DeserializeSeed<'de> for Field<S> {
    type Value = ();

    fn deserialize<D: de::Deserializer<'de>>(self, d: D) -> Result<(), D::Error> {
        d.deserialize_map(self)
    }
}

impl<'de, S: DeserializeSeed<'de, Value = ()>> Visitor<'de> for Field<S> {
    type Value = ();

    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "an object with `{}`", self.key)
    }

    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<(), A::Error> {
        let mut seed = Some(self.seed);
        while let Some(key) = map.next_key::<String>()? {
            match seed.take_if(|_| key == self.key) {
                Some(inner) => map.next_value_seed(inner)?,
                None => {
                    map.next_value::<IgnoredAny>()?;
                }
            }
        }
        match seed {
            None => Ok(()),
            Some(_) => Err(de::Error::missing_field(self.key)),
        }
    }
}

/// Hands each element of `entries` to the callback as it is parsed.
struct Entries<'f>(&'f mut dyn FnMut(Entry));

impl<'de> DeserializeSeed<'de> for Entries<'_> {
    type Value = ();

    fn deserialize<D: de::Deserializer<'de>>(self, d: D) -> Result<(), D::Error> {
        d.deserialize_seq(self)
    }
}

impl<'de> Visitor<'de> for Entries<'_> {
    type Value = ();

    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("a list of HAR entries")
    }

    fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<(), A::Error> {
        while let Some(entry) = seq.next_element::<Entry>()? {
            (self.0)(entry);
        }
        Ok(())
    }
}

/// Collects the accepted entries, grouped by host in first-seen order.
struct Builder<'a> {
    file: &'a str,
    filter: &'a HarFilter,
    count: usize,
    seen: HashSet<(String, String, u64)>,
    hosts: Vec<Folder>,
    notes: Vec<Note>,
    filtered: usize,
    duplicates: usize,
}

impl<'a> Builder<'a> {
    fn new(file: &'a str, filter: &'a HarFilter) -> Self {
        Self {
            file,
            filter,
            count: 0,
            seen: HashSet::new(),
            hosts: Vec::new(),
            notes: Vec::new(),
            filtered: 0,
            duplicates: 0,
        }
    }

    fn add(&mut self, entry: Entry) {
        self.count += 1;
        let at = Location::item(self.file, &format!("entries[{}]", self.count - 1));
        let method = common::method(Some(&entry.request.method), &at, &mut self.notes);
        let (base, query) = url_and_query(&entry.request);
        let host = host_of(&base);
        if !self.filter.include_static && is_static(&base, &entry.response.content.mime_type) {
            let message = format!("static asset dropped: {method} {base}");
            self.notes.push(Note::new(NoteKind::Dropped, message, &at));
            return;
        }
        if !self.accepts(&method, &host, &entry) {
            self.filtered += 1;
            return;
        }
        if !self.seen.insert(dedup_key(&method, &entry.request)) {
            self.duplicates += 1;
            return;
        }
        let headers = clean_headers(&entry.request.headers);
        let request = ImportedRequest {
            name: format!("{method} {}", path_of(&base)),
            method,
            url: base,
            query,
            body: entry.request.post_data.and_then(body),
            headers,
            location: at,
            ..ImportedRequest::default()
        };
        self.host_folder(&host)
            .items
            .push(Item::Request(Box::new(request)));
    }

    fn accepts(&self, method: &str, host: &str, entry: &Entry) -> bool {
        let f = self.filter;
        let any = |list: &[String], value: &str| {
            list.is_empty() || list.iter().any(|v| v.eq_ignore_ascii_case(value))
        };
        let host_name = host.split(':').next().unwrap_or(host);
        let host_ok = any(&f.hosts, host) || any(&f.hosts, host_name);
        let request_type = entry
            .request
            .post_data
            .as_ref()
            .map(|p| p.mime_type.as_str());
        let types = [
            request_type.unwrap_or_default(),
            &entry.response.content.mime_type,
        ];
        let type_ok = f.content_types.is_empty()
            || types
                .iter()
                .any(|t| any(&f.content_types, mime_base(t)) && !t.is_empty());
        host_ok && any(&f.methods, method) && type_ok
    }

    fn host_folder(&mut self, host: &str) -> &mut Folder {
        let index = match self.hosts.iter().position(|h| h.name == host) {
            Some(index) => index,
            None => {
                self.hosts.push(Folder {
                    name: host.to_owned(),
                    location: Location::item(self.file, host),
                    ..Folder::default()
                });
                self.hosts.len() - 1
            }
        };
        &mut self.hosts[index]
    }

    fn finish(mut self) -> ImportedCollection {
        let at = Location::item(self.file, "");
        if self.duplicates > 0 {
            let message = format!("{} duplicate entries skipped", self.duplicates);
            self.notes.push(Note::new(NoteKind::Info, message, &at));
        }
        if self.filtered > 0 {
            let message = format!("{} entries left out by the filters", self.filtered);
            self.notes.push(Note::new(NoteKind::Info, message, &at));
        }
        let name = Path::new(self.file)
            .file_stem()
            .map_or_else(|| "har".to_owned(), |s| s.to_string_lossy().into_owned());
        ImportedCollection {
            root: Folder {
                name: name.clone(),
                items: self
                    .hosts
                    .into_iter()
                    .map(|h| Item::Folder(Box::new(h)))
                    .collect(),
                location: at,
                ..Folder::default()
            },
            name,
            environments: Vec::new(),
            notes: self.notes,
        }
    }
}

/// `host[:port]` of an absolute URL (without user info).
fn host_of(url: &str) -> String {
    let after_scheme = url.split_once("://").map_or(url, |(_, rest)| rest);
    let authority = after_scheme.split('/').next().unwrap_or_default();
    let host = authority.rsplit_once('@').map_or(authority, |(_, h)| h);
    host.to_ascii_lowercase()
}

fn mime_base(mime: &str) -> &str {
    mime.split(';').next().unwrap_or_default().trim()
}

const STATIC_EXTENSIONS: &[&str] = &[
    "png", "jpg", "jpeg", "gif", "svg", "ico", "webp", "avif", "bmp", "woff", "woff2", "ttf",
    "otf", "eot", "css", "js", "mjs", "map",
];

/// Images, fonts, stylesheets and scripts, by response MIME type or URL
/// extension.
fn is_static(url: &str, mime: &str) -> bool {
    let mime = mime_base(mime).to_ascii_lowercase();
    let by_type = mime.starts_with("image/")
        || mime.starts_with("font/")
        || mime == "text/css"
        || mime.ends_with("javascript")
        || mime.ends_with("ecmascript");
    let last = path_of(url).rsplit('/').next().unwrap_or_default();
    let by_extension = last
        .rsplit_once('.')
        .is_some_and(|(_, ext)| STATIC_EXTENSIONS.contains(&ext.to_ascii_lowercase().as_str()));
    by_type || by_extension
}

/// Transport and browser-managed headers, never written to request files.
fn is_transport_header(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    lower.starts_with(':')
        || lower.starts_with("sec-")
        || matches!(
            lower.as_str(),
            "host"
                | "content-length"
                | "connection"
                | "transfer-encoding"
                | "accept-encoding"
                | "keep-alive"
                | "proxy-connection"
                | "upgrade"
                | "te"
                | "trailer"
        )
}

fn clean_headers(headers: &[NameValue]) -> Vec<Pair> {
    headers
        .iter()
        .filter(|h| !is_transport_header(&h.name))
        .map(|h| Pair::new(&h.name, &h.value))
        .collect()
}

/// The URL without its query, and the query parameters: those of the URL
/// as written (still percent-encoded), or `queryString` when the URL has
/// none. Browsers fill both with the same parameters, so taking one never
/// duplicates them.
fn url_and_query(request: &Request) -> (String, Vec<Pair>) {
    let (base, query) = split_url(&request.url);
    if !query.is_empty() {
        return (base, query);
    }
    let from_list = request
        .query_string
        .iter()
        .map(|p| Pair::new(&p.name, &p.value))
        .collect();
    (base, from_list)
}

/// Method, URL with its query parameters sorted, and a hash of the body.
fn dedup_key(method: &str, request: &Request) -> (String, String, u64) {
    let (base, mut query) = url_and_query(request);
    query.sort_by(|a, b| (&a.name, &a.value).cmp(&(&b.name, &b.value)));
    let query: Vec<String> = query
        .iter()
        .map(|p| format!("{}={}", p.name, p.value))
        .collect();
    let mut hasher = DefaultHasher::new();
    if let Some(data) = &request.post_data {
        data.text.hash(&mut hasher);
        for p in &data.params {
            (&p.name, &p.value, &p.file_name).hash(&mut hasher);
        }
    }
    (
        method.to_owned(),
        format!("{base}?{}", query.join("&")),
        hasher.finish(),
    )
}

fn body(data: PostData) -> Option<ImportedBody> {
    let base = mime_base(&data.mime_type).to_ascii_lowercase();
    let text = data.text.unwrap_or_default();
    let body = match base.as_str() {
        "application/x-www-form-urlencoded" => ImportedBody::Form {
            fields: if data.params.is_empty() {
                common::query_pairs(&text)
            } else {
                data.params
                    .into_iter()
                    .map(|p| Pair::new(p.name, p.value.unwrap_or_default()))
                    .collect()
            },
        },
        "multipart/form-data" if !data.params.is_empty() => ImportedBody::Multipart {
            parts: data.params.into_iter().map(part).collect(),
        },
        // Chromium exports multipart bodies as `text` only.
        "multipart/form-data" => match multipart_text(&data.mime_type, &text) {
            Some(parts) => ImportedBody::Multipart { parts },
            None => return text_body(&data.mime_type, &text),
        },
        _ => return text_body(&data.mime_type, &text),
    };
    Some(body)
}

/// The parts of a raw `multipart/form-data` body, split at the boundary
/// of `mime`; `None` when there is no boundary or no part.
fn multipart_text(mime: &str, text: &str) -> Option<Vec<ImportedPart>> {
    let boundary = mime
        .split(';')
        .filter_map(|p| p.trim().strip_prefix("boundary="))
        .next()?
        .trim_matches('"');
    let delimiter = format!("--{boundary}");
    let parts: Vec<ImportedPart> = text
        .split(delimiter.as_str())
        .filter_map(multipart_part)
        .collect();
    (!parts.is_empty()).then_some(parts)
}

/// One part: its `Content-Disposition` name and filename, its
/// `Content-Type`, and the value after the blank line.
fn multipart_part(raw: &str) -> Option<ImportedPart> {
    let raw = raw
        .strip_prefix("\r\n")
        .or_else(|| raw.strip_prefix('\n'))?;
    let (head, body) = raw
        .split_once("\r\n\r\n")
        .or_else(|| raw.split_once("\n\n"))?;
    let body = body
        .strip_suffix("\r\n")
        .or_else(|| body.strip_suffix('\n'))
        .unwrap_or(body);
    let mut name = None;
    let mut file_name = None;
    let mut content_type = None;
    for line in head.lines() {
        let (key, value) = line.split_once(':')?;
        if key.trim().eq_ignore_ascii_case("content-type") {
            content_type = Some(value.trim().to_owned());
        } else if key.trim().eq_ignore_ascii_case("content-disposition") {
            name = disposition(value, "name");
            file_name = disposition(value, "filename");
        }
    }
    let value = match file_name {
        Some(file) => PartValue::File(file),
        None => PartValue::Text(body.to_owned()),
    };
    Some(ImportedPart {
        name: name?,
        value,
        content_type,
    })
}

/// The `key="value"` parameter of a `Content-Disposition` header.
fn disposition(header: &str, key: &str) -> Option<String> {
    header.split(';').find_map(|p| {
        let (k, v) = p.trim().split_once('=')?;
        (k == key).then(|| v.trim_matches('"').to_owned())
    })
}

fn part(p: Param) -> ImportedPart {
    let value = match p.file_name {
        Some(file) if !file.is_empty() => PartValue::File(file),
        _ => PartValue::Text(p.value.unwrap_or_default()),
    };
    ImportedPart {
        name: p.name,
        value,
        content_type: p.content_type.filter(|c| !c.is_empty()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hosts_and_paths_come_from_the_url() {
        assert_eq!(
            host_of("https://user@API.example.com:8443/a/b"),
            "api.example.com:8443"
        );
        assert_eq!(path_of("https://x.test/a/b"), "/a/b");
        assert_eq!(path_of("https://x.test"), "/");
    }

    #[test]
    fn static_assets_are_recognized_by_type_or_extension() {
        assert!(is_static("https://x/logo", "image/png"));
        assert!(is_static("https://x/app.JS", ""));
        assert!(is_static(
            "https://x/a",
            "application/javascript; charset=utf-8"
        ));
        assert!(is_static("https://x/f.woff2", ""));
        assert!(!is_static("https://x/api/users", "application/json"));
        assert!(!is_static("https://x/api.v1/users", "application/json"));
    }

    #[test]
    fn transport_headers_are_dropped() {
        for name in [
            ":authority",
            "Content-Length",
            "Host",
            "sec-ch-ua",
            "Accept-Encoding",
        ] {
            assert!(is_transport_header(name), "{name}");
        }
        assert!(!is_transport_header("Accept"));
        assert!(!is_transport_header("Cookie"));
    }

    fn queries(har: &str) -> Vec<Vec<(String, String)>> {
        let collection = parse(har, "x.har", &HarFilter::default()).unwrap();
        let mut root = collection.root;
        root.requests_mut()
            .iter()
            .map(|r| {
                r.query
                    .iter()
                    .map(|p| (p.name.clone(), p.value.clone()))
                    .collect()
            })
            .collect()
    }

    #[test]
    fn raw_multipart_text_is_split_into_parts() {
        let text = "------B\r\nContent-Disposition: form-data; name=\"user\"\r\n\r\nme\r\n\
                    ------B\r\nContent-Disposition: form-data; name=\"password\"\r\n\r\npw-1\r\n\
                    ------B\r\nContent-Disposition: form-data; name=\"f\"; filename=\"a.png\"\r\n\
                    Content-Type: image/png\r\n\r\nPNG\r\n------B--\r\n";
        let parts = multipart_text("multipart/form-data; boundary=----B", text).unwrap();
        let summary: Vec<(String, PartValue)> =
            parts.into_iter().map(|p| (p.name, p.value)).collect();
        assert_eq!(
            summary,
            [
                ("user".into(), PartValue::Text("me".into())),
                ("password".into(), PartValue::Text("pw-1".into())),
                ("f".into(), PartValue::File("a.png".into())),
            ]
        );
        assert!(multipart_text("multipart/form-data", text).is_none());
    }

    #[test]
    fn query_comes_from_the_url_or_else_from_query_string() {
        let both = r#"{"log": {"entries": [{"request": {"method": "GET",
            "url": "https://x.test/a?q=red%20lamp&n=1",
            "queryString": [{"name": "q", "value": "red lamp"}, {"name": "n", "value": "1"}]}}]}}"#;
        assert_eq!(
            queries(both),
            [[("q".into(), "red%20lamp".into()), ("n".into(), "1".into())]]
        );
        let list_only = r#"{"log": {"entries": [{"request": {"method": "GET",
            "url": "https://x.test/a",
            "queryString": [{"name": "page", "value": "2"}]}}]}}"#;
        assert_eq!(queries(list_only), [[("page".into(), "2".into())]]);
    }

    #[test]
    fn a_document_without_log_entries_is_invalid() {
        let err = parse(
            r#"{"log": {"version": "1.2"}}"#,
            "x.har",
            &HarFilter::default(),
        );
        assert!(err.is_err());
        let ok = parse(
            r#"{"log": {"entries": []}, "x": [1]}"#,
            "x.har",
            &HarFilter::default(),
        );
        assert!(ok.unwrap().root.items.is_empty());
    }
}
