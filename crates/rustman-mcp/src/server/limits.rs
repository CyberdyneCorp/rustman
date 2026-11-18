//! Output limits (spec "Output size limits and pagination", design D7):
//! the cap on one serialized result, body truncation with a `body_ref`
//! kept in a per-session LRU, and opaque pagination cursors.

use std::collections::VecDeque;
use std::hash::{DefaultHasher, Hash, Hasher};

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use bytes::Bytes;
use serde::{Deserialize, Serialize};

use super::error::{ErrorCode, ToolError};

/// Bodies longer than this are truncated in results.
pub const MAX_INLINE_BODY: usize = 32 * 1024;

/// Memory kept for truncated bodies per session.
pub const BODY_STORE_BYTES: usize = 64 * 1024 * 1024;

/// Items per page when the agent gives no `limit`.
pub const DEFAULT_PAGE: usize = 50;

/// The URI prefix of stored bodies.
pub const BODY_URI_PREFIX: &str = "rustman://bodies/";

/// Refuse a result whose serialized form exceeds `max` bytes.
pub fn check_size(result: &serde_json::Value, max: usize) -> Result<(), ToolError> {
    let size = serde_json::to_vec(result).map_or(usize::MAX, |bytes| bytes.len());
    if size <= max {
        return Ok(());
    }
    Err(ToolError::new(
        ErrorCode::InvalidInput,
        format!(
            "the result is {size} bytes, over the {max}-byte limit (max_output_bytes); narrow the request or page through it"
        ),
    ))
}

/// A body as a tool result shows it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct BodyView {
    /// The text, cut at [`MAX_INLINE_BODY`] bytes; base64 for binary bodies.
    pub text: String,
    /// `base64` for binary bodies, else `utf-8`.
    pub encoding: &'static str,
    /// The size of the whole body, in bytes.
    pub size: u64,
    pub truncated: bool,
    /// `rustman://bodies/<id>`: the whole (redacted) body, when truncated.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub body_ref: Option<String>,
}

/// The longest prefix of `text` within `max` bytes that ends on a character
/// boundary.
fn cut(text: &str, max: usize) -> &str {
    if text.len() <= max {
        return text;
    }
    let mut end = max;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    &text[..end]
}

/// Bodies kept for `resources/read`, the least recently used dropped first
/// once their total exceeds the capacity.
#[derive(Debug)]
pub struct BodyStore {
    capacity: usize,
    used: usize,
    next: u64,
    entries: VecDeque<(String, Bytes)>,
}

impl Default for BodyStore {
    fn default() -> Self {
        Self::with_capacity(BODY_STORE_BYTES)
    }
}

impl BodyStore {
    pub fn with_capacity(capacity: usize) -> Self {
        Self {
            capacity,
            used: 0,
            next: 0,
            entries: VecDeque::new(),
        }
    }

    /// Keep `body` and return its id. A body larger than the whole capacity
    /// is not kept and gets no id.
    pub fn insert(&mut self, body: Bytes) -> Option<String> {
        if body.len() > self.capacity {
            return None;
        }
        while self.used + body.len() > self.capacity {
            let Some((_, old)) = self.entries.pop_front() else {
                break;
            };
            self.used -= old.len();
        }
        self.next += 1;
        let id = format!("b{}", self.next);
        self.used += body.len();
        self.entries.push_back((id.clone(), body));
        Some(id)
    }

    /// The body with `id`, marked as the most recently used.
    pub fn get(&mut self, id: &str) -> Option<Bytes> {
        let at = self.entries.iter().position(|(key, _)| key == id)?;
        let entry = self.entries.remove(at)?;
        let body = entry.1.clone();
        self.entries.push_back(entry);
        Some(body)
    }

    /// Show `body` (already redacted; `size` is the size of the whole body as
    /// received): inline when short, else truncated with a `body_ref`.
    pub fn view(&mut self, body: Bytes, size: u64, binary: bool) -> BodyView {
        let encoding = if binary { "base64" } else { "utf-8" };
        let full = body_text(&body, binary);
        if full.len() <= MAX_INLINE_BODY && body.len() as u64 >= size {
            return BodyView {
                text: full,
                encoding,
                size,
                truncated: false,
                body_ref: None,
            };
        }
        let text = cut(&full, MAX_INLINE_BODY).to_owned();
        let body_ref = self.insert(body).map(|id| format!("{BODY_URI_PREFIX}{id}"));
        BodyView {
            text,
            encoding,
            size,
            truncated: true,
            body_ref,
        }
    }
}

fn body_text(body: &[u8], binary: bool) -> String {
    if binary {
        base64::engine::general_purpose::STANDARD.encode(body)
    } else {
        String::from_utf8_lossy(body).into_owned()
    }
}

/// One page of a list.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Page<T> {
    pub items: Vec<T>,
    /// Pass it back to get the next page; `None` on the last one.
    pub next_cursor: Option<String>,
}

#[derive(Serialize, Deserialize)]
struct Cursor {
    offset: usize,
    snapshot: u64,
}

/// A fingerprint of a list, so a cursor from another listing is refused.
pub fn snapshot<K: Hash>(keys: impl IntoIterator<Item = K>) -> u64 {
    let mut hasher = DefaultHasher::new();
    for key in keys {
        key.hash(&mut hasher);
    }
    hasher.finish()
}

/// The page of `items` that `cursor` points to (the first without one), of
/// `limit` items (default [`DEFAULT_PAGE`]). `snapshot` fingerprints the
/// whole list.
pub fn paginate<T>(
    items: Vec<T>,
    snapshot: u64,
    cursor: Option<&str>,
    limit: Option<usize>,
) -> Result<Page<T>, ToolError> {
    let offset = match cursor {
        Some(cursor) => decode_cursor(cursor, snapshot)?,
        None => 0,
    };
    let limit = limit.unwrap_or(DEFAULT_PAGE).max(1);
    let end = offset.saturating_add(limit).min(items.len());
    let next_cursor = (end < items.len()).then(|| encode_cursor(end, snapshot));
    let items = items
        .into_iter()
        .skip(offset)
        .take(end.saturating_sub(offset));
    Ok(Page {
        items: items.collect(),
        next_cursor,
    })
}

fn encode_cursor(offset: usize, snapshot: u64) -> String {
    let json = serde_json::to_vec(&Cursor { offset, snapshot }).unwrap_or_default();
    URL_SAFE_NO_PAD.encode(json)
}

fn decode_cursor(cursor: &str, snapshot: u64) -> Result<usize, ToolError> {
    let invalid = || ToolError::new(ErrorCode::InvalidInput, "invalid or expired cursor");
    let bytes = URL_SAFE_NO_PAD.decode(cursor).map_err(|_| invalid())?;
    let cursor: Cursor = serde_json::from_slice(&bytes).map_err(|_| invalid())?;
    if cursor.snapshot != snapshot {
        return Err(ToolError::new(
            ErrorCode::InvalidInput,
            "the list changed since this cursor was issued; start again without a cursor",
        ));
    }
    Ok(cursor.offset)
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn a_2_mib_body_is_cut_to_32_kib_with_its_size_and_a_ref() {
        let body = Bytes::from(vec![b'a'; 2 * 1024 * 1024]);
        let mut store = BodyStore::default();
        let view = store.view(body.clone(), 2_097_152, false);
        assert!(view.truncated);
        assert_eq!(view.size, 2_097_152);
        assert_eq!(view.text.len(), 32 * 1024);
        let body_ref = view.body_ref.unwrap();
        let id = body_ref.strip_prefix(BODY_URI_PREFIX).unwrap();
        assert_eq!(store.get(id).unwrap(), body);
        let json = serde_json::to_value(store.view(Bytes::from_static(b"ok"), 2, false)).unwrap();
        assert_eq!(
            json,
            json!({"text": "ok", "encoding": "utf-8", "size": 2, "truncated": false})
        );
    }

    #[test]
    fn truncation_never_splits_a_character_and_spilled_bodies_are_truncated() {
        let text = "é".repeat(MAX_INLINE_BODY);
        let view = BodyStore::default().view(Bytes::from(text), 0, false);
        assert!(view.truncated);
        assert_eq!(view.text.len(), MAX_INLINE_BODY);
        // Only the first bytes of a spilled body are in memory.
        let view = BodyStore::default().view(Bytes::from_static(b"head"), 1 << 30, false);
        assert!(view.truncated);
        assert_eq!(view.size, 1 << 30);
        let binary = BodyStore::default().view(Bytes::from_static(&[0, 159, 146]), 3, true);
        assert_eq!((binary.text.as_str(), binary.encoding), ("AJ+S", "base64"));
    }

    #[test]
    fn the_store_drops_the_least_recently_used_body() {
        let mut store = BodyStore::with_capacity(10);
        let a = store.insert(Bytes::from_static(b"aaaa")).unwrap();
        let b = store.insert(Bytes::from_static(b"bbbb")).unwrap();
        assert!(store.get(&a).is_some());
        let c = store.insert(Bytes::from_static(b"cccc")).unwrap();
        assert!(store.get(&b).is_none());
        assert!(store.get(&a).is_some() && store.get(&c).is_some());
        assert!(store.insert(Bytes::from(vec![0; 11])).is_none());
    }

    #[test]
    fn one_hundred_twenty_items_page_as_50_50_and_20() {
        let items: Vec<u32> = (0..120).collect();
        let snap = snapshot(&items);
        let first = paginate(items.clone(), snap, None, None).unwrap();
        let second = paginate(items.clone(), snap, first.next_cursor.as_deref(), None).unwrap();
        let third = paginate(items.clone(), snap, second.next_cursor.as_deref(), None).unwrap();
        assert_eq!(first.items, (0..50).collect::<Vec<_>>());
        assert_eq!(second.items, (50..100).collect::<Vec<_>>());
        assert_eq!(third.items, (100..120).collect::<Vec<_>>());
        assert!(third.next_cursor.is_none());
    }

    #[test]
    fn foreign_or_stale_cursors_are_refused() {
        let items: Vec<u32> = (0..120).collect();
        let page = paginate(items.clone(), snapshot(&items), None, Some(10)).unwrap();
        assert_eq!(page.items.len(), 10);
        let cursor = page.next_cursor.unwrap();
        let changed: Vec<u32> = (0..121).collect();
        let err = paginate(changed.clone(), snapshot(&changed), Some(&cursor), None).unwrap_err();
        assert_eq!(err.code, ErrorCode::InvalidInput);
        let err = paginate(items, 0, Some("not a cursor"), None).unwrap_err();
        assert_eq!(err.message, "invalid or expired cursor");
    }

    #[test]
    fn results_over_the_cap_are_refused() {
        let big = json!({"text": "x".repeat(300 * 1024)});
        let err = check_size(&big, 256 * 1024).unwrap_err();
        assert_eq!(err.code, ErrorCode::InvalidInput);
        assert!(err.message.contains("max_output_bytes"), "{}", err.message);
        assert!(check_size(&json!({"ok": true}), 256 * 1024).is_ok());
    }
}
