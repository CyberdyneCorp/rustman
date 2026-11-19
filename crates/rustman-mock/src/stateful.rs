//! Stateful mode (spec: Stateful mode, Stateful reset and limits; design
//! D5): resources come from the `api-exploration` dependency discovery (a
//! `POST` collection whose response holds the id that the item path's
//! parameter takes). Their items live in an in-memory [`Store`] that is
//! kept outside the loaded document, so it survives reloads.

use std::collections::{BTreeMap, HashMap};
use std::sync::Mutex;

use http::StatusCode;
use rustman_explore::ApiModel;
use rustman_explore::explore::graph::Graph;
use rustman_explore::explore::resources::creators;
use rustman_explore::openapi::model::Operation;
use rustman_explore::synth::Synthesizer;
use serde_json::{Map, Value};

use crate::faults::Picks;
use crate::problem::Problem;
use crate::respond::{Chosen, Source, default_response, mix};

/// The default most items per resource.
pub const DEFAULT_MAX_ITEMS: usize = 10_000;

/// The default most bytes of JSON the store holds, all resources together.
pub const DEFAULT_MAX_STORE_BYTES: usize = 64 * 1024 * 1024;

/// How new ids are made, from the id's schema.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IdKind {
    /// `format: uuid`: a UUIDv4 from the seeded stream.
    Uuid,
    /// `type: integer`: 1, 2, 3...
    Integer,
    /// Anything else: "1", "2", "3"...
    Text,
}

/// One resource: its collection, item path and where its id lives.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Resource {
    /// `/users`.
    pub collection: String,
    /// `/users/{id}`.
    pub item: String,
    /// `id`: the item path's parameter.
    pub param: String,
    /// `/id`: the id in an item.
    pub pointer: String,
    pub ids: IdKind,
}

/// What an operation does to its resource.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Role {
    Create,
    List,
    Read,
    Replace,
    Patch,
    Delete,
}

/// The resources of one document.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Resources {
    list: Vec<Resource>,
}

impl Resources {
    pub fn new(model: &ApiModel) -> Self {
        let graph = Graph::build(model, &BTreeMap::new());
        let synth = Synthesizer::new(&model.components);
        let list = creators(model, &graph)
            .into_iter()
            .filter_map(|creator| {
                let collection = creator.create.strip_prefix("POST ")?.to_owned();
                let (item, param) = item_path(model, &collection)?;
                let create = model
                    .operations
                    .iter()
                    .find(|op| op.method == "POST" && op.path == collection)?;
                let ids = id_kind(create, &creator.pointer, &synth);
                Some(Resource {
                    collection,
                    item,
                    param,
                    pointer: creator.pointer,
                    ids,
                })
            })
            .collect();
        Self { list }
    }

    pub fn list(&self) -> &[Resource] {
        &self.list
    }

    /// The resource `operation` acts on, and how.
    pub fn for_operation(&self, operation: &Operation) -> Option<(&Resource, Role)> {
        self.list.iter().find_map(|resource| {
            let on_collection = operation.path == resource.collection;
            let on_item = operation.path == resource.item;
            let role = match (operation.method.as_str(), on_collection, on_item) {
                ("POST", true, _) => Role::Create,
                ("GET", true, _) => Role::List,
                ("GET", _, true) => Role::Read,
                ("PUT", _, true) => Role::Replace,
                ("PATCH", _, true) => Role::Patch,
                ("DELETE", _, true) => Role::Delete,
                _ => return None,
            };
            Some((resource, role))
        })
    }
}

/// The documented `collection/{param}` path.
fn item_path(model: &ApiModel, collection: &str) -> Option<(String, String)> {
    model.operations.iter().find_map(|op| {
        let rest = op.path.strip_prefix(collection.trim_end_matches('/'))?;
        let param = rest.strip_prefix("/{")?.strip_suffix('}')?;
        (!param.contains(['/', '{', '}'])).then(|| (op.path.clone(), param.to_owned()))
    })
}

/// The kind of the id at `pointer` in the create operation's success body.
fn id_kind(create: &Operation, pointer: &str, synth: &Synthesizer<'_>) -> IdKind {
    let (response, _) = default_response(create);
    let schema = response
        .and_then(|r| r.content.iter().find_map(|m| m.schema.as_ref()))
        .and_then(|s| schema_at(s, pointer, synth));
    let Some(schema) = schema else {
        return IdKind::Text;
    };
    if schema.get("format").and_then(Value::as_str) == Some("uuid") {
        return IdKind::Uuid;
    }
    match schema.get("type").and_then(Value::as_str) {
        Some("integer") => IdKind::Integer,
        _ => IdKind::Text,
    }
}

fn schema_at(schema: &Value, pointer: &str, synth: &Synthesizer<'_>) -> Option<Value> {
    let mut current = Value::Object(synth.object_view(schema)?);
    for token in pointer.split('/').skip(1) {
        let next = current.get("properties")?.get(unescape(token))?.clone();
        current = synth
            .object_view(&next)
            .map_or_else(|| synth.resolve(&next).clone(), Value::Object);
    }
    Some(current)
}

fn unescape(token: &str) -> String {
    token.replace("~1", "/").replace("~0", "~")
}

#[derive(Debug, Default)]
struct Collection {
    created: u64,
    /// Each item's id, value and size as JSON.
    items: Vec<(String, Value, usize)>,
}

#[derive(Debug, Default)]
struct Inner {
    collections: HashMap<String, Collection>,
    /// The JSON size of every stored item, all resources together.
    bytes: usize,
}

/// Why a write was refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Full {
    /// The resource already holds the most items.
    Items(usize),
    /// The items would take more than this many bytes, all resources
    /// together.
    Bytes(usize),
}

/// The items of every resource, by collection path.
#[derive(Debug)]
pub struct Store {
    max_items: usize,
    max_bytes: usize,
    seed: u64,
    inner: Mutex<Inner>,
}

impl Store {
    pub fn new(max_items: usize, seed: u64) -> Self {
        Self {
            max_items,
            max_bytes: DEFAULT_MAX_STORE_BYTES,
            seed,
            inner: Mutex::new(Inner::default()),
        }
    }

    /// This store holding at most `max_bytes` of JSON in all.
    #[must_use]
    pub fn with_max_bytes(mut self, max_bytes: usize) -> Self {
        self.max_bytes = max_bytes;
        self
    }

    fn with<T>(&self, collection: &str, f: impl FnOnce(&mut Collection, &mut usize) -> T) -> T {
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        let Inner { collections, bytes } = &mut *inner;
        f(collections.entry(collection.to_owned()).or_default(), bytes)
    }

    /// Forget every item.
    pub fn reset(&self) {
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        *inner = Inner::default();
    }

    /// How many items `resource` holds.
    pub fn len(&self, resource: &Resource) -> usize {
        self.with(&resource.collection, |c, _| c.items.len())
    }

    /// How many bytes of JSON the store holds, all resources together.
    pub fn bytes(&self) -> usize {
        self.inner.lock().unwrap_or_else(|e| e.into_inner()).bytes
    }

    /// Whether `size` more bytes fit once `freed` bytes are released.
    fn fits(&self, used: usize, freed: usize, size: usize) -> Result<(), Full> {
        if used - freed + size > self.max_bytes {
            return Err(Full::Bytes(self.max_bytes));
        }
        Ok(())
    }

    /// Store `body` with a new id; refused when the resource or the store
    /// is full.
    pub fn create(&self, resource: &Resource, body: Value) -> Result<Value, Full> {
        self.with(&resource.collection, |c, used| {
            if c.items.len() >= self.max_items {
                return Err(Full::Items(self.max_items));
            }
            let id = new_id(resource, c.created + 1, self.seed);
            let mut item = as_object(body);
            set_pointer(&mut item, &resource.pointer, id.clone());
            let size = json_size(&item);
            self.fits(*used, 0, size)?;
            c.created += 1;
            *used += size;
            c.items.push((id_text(&id), item.clone(), size));
            Ok(item)
        })
    }

    pub fn list(&self, resource: &Resource) -> Vec<Value> {
        self.with(&resource.collection, |c, _| {
            c.items.iter().map(|(_, v, _)| v.clone()).collect()
        })
    }

    pub fn get(&self, resource: &Resource, id: &str) -> Option<Value> {
        self.with(&resource.collection, |c, _| {
            c.items
                .iter()
                .find(|(i, _, _)| i == id)
                .map(|(_, v, _)| v.clone())
        })
    }

    /// Replace (`patch: false`) or merge-patch the item `id`: `Ok(None)`
    /// when there is no such item, refused when the store would be full.
    pub fn update(
        &self,
        resource: &Resource,
        id: &str,
        body: Value,
        patch: bool,
    ) -> Result<Option<Value>, Full> {
        self.with(&resource.collection, |c, used| {
            let Some((_, item, size)) = c.items.iter_mut().find(|(i, _, _)| i == id) else {
                return Ok(None);
            };
            let kept = item.pointer(&resource.pointer).cloned();
            let mut next = if patch {
                merge_patch(item.clone(), body)
            } else {
                as_object(body)
            };
            if let Some(kept) = kept {
                set_pointer(&mut next, &resource.pointer, kept);
            }
            let next_size = json_size(&next);
            self.fits(*used, *size, next_size)?;
            *used = *used - *size + next_size;
            *size = next_size;
            *item = next.clone();
            Ok(Some(next))
        })
    }

    pub fn delete(&self, resource: &Resource, id: &str) -> bool {
        self.with(&resource.collection, |c, used| {
            let Some(at) = c.items.iter().position(|(i, _, _)| i == id) else {
                return false;
            };
            let (_, _, size) = c.items.remove(at);
            *used -= size;
            true
        })
    }
}

/// The size of `value` as compact JSON.
fn json_size(value: &Value) -> usize {
    serde_json::to_vec(value).map_or(0, |bytes| bytes.len())
}

fn new_id(resource: &Resource, n: u64, seed: u64) -> Value {
    match resource.ids {
        IdKind::Integer => Value::from(n),
        IdKind::Text => Value::String(n.to_string()),
        IdKind::Uuid => {
            let mut picks = Picks::new(mix(seed, &[&resource.collection, &n.to_string()]));
            Value::String(uuid_v4(picks.next_u64(), picks.next_u64()))
        }
    }
}

/// A UUIDv4 string from 128 random bits.
fn uuid_v4(high: u64, low: u64) -> String {
    let high = (high & !0xF000) | 0x4000;
    let low = (low & !(0b11 << 62)) | (0b10 << 62);
    let hex = format!("{high:016x}{low:016x}");
    format!(
        "{}-{}-{}-{}-{}",
        &hex[0..8],
        &hex[8..12],
        &hex[12..16],
        &hex[16..20],
        &hex[20..32]
    )
}

fn id_text(id: &Value) -> String {
    match id {
        Value::String(text) => text.clone(),
        other => other.to_string(),
    }
}

fn as_object(body: Value) -> Value {
    match body {
        Value::Object(_) => body,
        _ => Value::Object(Map::new()),
    }
}

/// Set `pointer` in `target`, creating objects on the way.
fn set_pointer(target: &mut Value, pointer: &str, value: Value) {
    let tokens: Vec<String> = pointer.split('/').skip(1).map(unescape).collect();
    let Some((last, parents)) = tokens.split_last() else {
        return;
    };
    let mut current = target;
    for token in parents {
        let Value::Object(map) = current else { return };
        current = map
            .entry(token.clone())
            .or_insert_with(|| Value::Object(Map::new()));
    }
    if let Value::Object(map) = current {
        map.insert(last.clone(), value);
    }
}

/// RFC 7386 JSON merge patch.
fn merge_patch(target: Value, patch: Value) -> Value {
    let Value::Object(patch) = patch else {
        return patch;
    };
    let mut target = match target {
        Value::Object(map) => map,
        _ => Map::new(),
    };
    for (key, value) in patch {
        if value.is_null() {
            target.remove(&key);
        } else {
            let current = target.remove(&key).unwrap_or(Value::Null);
            target.insert(key, merge_patch(current, value));
        }
    }
    Value::Object(target)
}

/// The response of `operation` (acting as `role` on `resource`) from the
/// store, or `None` to answer from the document instead.
pub fn answer(
    operation: &Operation,
    resource: &Resource,
    role: Role,
    id: Option<&str>,
    body: Option<Value>,
    store: &Store,
) -> Option<Result<Chosen, Problem>> {
    let body = body.unwrap_or(Value::Null);
    let item = |found: Option<Value>| {
        found
            .map(|v| ok(operation, Some(v)))
            .unwrap_or_else(|| Err(unknown(resource, id)))
    };
    let reply = match role {
        Role::Create => match store.create(resource, body) {
            Ok(created) => ok(operation, Some(created)),
            Err(why) => Err(full(resource, why)),
        },
        Role::List if lists_items(operation) => {
            ok(operation, Some(Value::Array(store.list(resource))))
        }
        Role::List => return None,
        Role::Read => item(store.get(resource, id?)),
        Role::Replace | Role::Patch => {
            match store.update(resource, id?, body, role == Role::Patch) {
                Ok(found) => item(found),
                Err(why) => Err(full(resource, why)),
            }
        }
        Role::Delete if store.delete(resource, id?) => ok(operation, None),
        Role::Delete => Err(unknown(resource, id)),
    };
    Some(reply)
}

/// Whether the success response of a collection `GET` is an array.
fn lists_items(operation: &Operation) -> bool {
    let (response, _) = default_response(operation);
    response
        .and_then(|r| r.content.iter().find_map(|m| m.schema.as_ref()))
        .and_then(|s| s.get("type"))
        .and_then(Value::as_str)
        == Some("array")
}

/// The operation's documented success status, with `value` as JSON
/// (nothing for `204`).
fn ok(operation: &Operation, value: Option<Value>) -> Result<Chosen, Problem> {
    let (_, status) = default_response(operation);
    let status = if status.is_success() {
        status
    } else {
        StatusCode::OK
    };
    if status == StatusCode::NO_CONTENT || value.is_none() {
        return Ok(Chosen {
            status,
            content_type: None,
            body: Vec::new(),
            source: Source::Stateful,
        });
    }
    let body = serde_json::to_vec(&value.unwrap_or(Value::Null)).unwrap_or_default();
    Ok(Chosen {
        status,
        content_type: Some("application/json".to_owned()),
        body,
        source: Source::Stateful,
    })
}

fn unknown(resource: &Resource, id: Option<&str>) -> Problem {
    Problem::new(
        StatusCode::NOT_FOUND,
        "unknown-id",
        "Unknown id",
        format!(
            "{} has no item {}",
            resource.collection,
            id.unwrap_or_default()
        ),
    )
}

fn full(resource: &Resource, why: Full) -> Problem {
    let detail = match why {
        Full::Items(max) => format!("{} already holds {max} items", resource.collection),
        Full::Bytes(max) => format!("the store would hold more than {max} bytes"),
    };
    Problem::new(
        StatusCode::INSUFFICIENT_STORAGE,
        "store-full",
        "Store full",
        detail,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn resource(ids: IdKind) -> Resource {
        Resource {
            collection: "/users".into(),
            item: "/users/{id}".into(),
            param: "id".into(),
            pointer: "/id".into(),
            ids,
        }
    }

    #[test]
    fn crud_round_trip() {
        let store = Store::new(10, 0);
        let users = resource(IdKind::Integer);
        let created = store
            .create(&users, json!({"name": "Ana", "id": 99}))
            .unwrap();
        assert_eq!(created, json!({"name": "Ana", "id": 1}));
        assert_eq!(store.get(&users, "1"), Some(created));
        let patched = store
            .update(&users, "1", json!({"age": 3, "name": null}), true)
            .unwrap();
        assert_eq!(patched, Some(json!({"id": 1, "age": 3})));
        let replaced = store
            .update(&users, "1", json!({"name": "Bo", "id": 7}), false)
            .unwrap();
        assert_eq!(replaced, Some(json!({"name": "Bo", "id": 1})));
        assert_eq!(store.update(&users, "2", json!({}), false), Ok(None));
        assert_eq!(store.list(&users).len(), 1);
        assert!(store.delete(&users, "1"));
        assert!(!store.delete(&users, "1"));
        assert_eq!(store.get(&users, "1"), None);
    }

    #[test]
    fn ids_follow_their_schema_and_the_seed() {
        let a = Store::new(10, 0)
            .create(&resource(IdKind::Uuid), json!({}))
            .unwrap();
        let b = Store::new(10, 0)
            .create(&resource(IdKind::Uuid), json!({}))
            .unwrap();
        let c = Store::new(10, 7)
            .create(&resource(IdKind::Uuid), json!({}))
            .unwrap();
        assert_eq!(a, b);
        assert_ne!(a, c);
        let id = a["id"].as_str().unwrap();
        assert_eq!(id.len(), 36);
        assert_eq!(&id[14..15], "4");
        assert!(matches!(&id[19..20], "8" | "9" | "a" | "b"), "{id}");
        let text = Store::new(10, 0)
            .create(&resource(IdKind::Text), json!(5))
            .unwrap();
        assert_eq!(text, json!({"id": "1"}));
    }

    #[test]
    fn the_cap_and_reset() {
        let store = Store::new(2, 0);
        let users = resource(IdKind::Integer);
        assert!(store.create(&users, json!({})).is_ok());
        assert!(store.create(&users, json!({})).is_ok());
        assert_eq!(store.create(&users, json!({})), Err(Full::Items(2)));
        store.reset();
        assert_eq!(store.len(&users), 0);
        assert_eq!(store.bytes(), 0);
    }

    #[test]
    fn the_byte_budget_covers_every_resource() {
        let store = Store::new(100, 0).with_max_bytes(40);
        let users = resource(IdKind::Integer);
        let big = json!({"name": "a".repeat(20)});
        let created = store.create(&users, big.clone()).unwrap();
        assert_eq!(store.bytes(), json_size(&created));
        assert_eq!(store.create(&users, big.clone()), Err(Full::Bytes(40)));
        let grown = json!({"name": "a".repeat(40)});
        assert_eq!(
            store.update(&users, "1", grown, false),
            Err(Full::Bytes(40))
        );
        assert_eq!(store.get(&users, "1"), Some(created));
        assert!(store.delete(&users, "1"));
        assert_eq!(store.bytes(), 0);
        assert!(store.create(&users, big).is_ok());
        let problem = full(&users, Full::Bytes(40));
        assert_eq!(problem.status, 507);
    }

    #[test]
    fn nested_pointers_are_created() {
        let mut value = json!({});
        set_pointer(&mut value, "/data/id", json!(1));
        assert_eq!(value, json!({"data": {"id": 1}}));
    }
}
