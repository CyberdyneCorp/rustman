//! Test plugins for transformers, request kinds and importers
//! (add-plugin-system tasks 5.4 to 5.6): an XOR "cipher" transformer, a
//! request kind that echoes the request it runs, and an importer that
//! copies its source as a request file. WebAssembly text, wrapped into
//! components in-process like the other fixtures.

use super::{AREA, Data, component};

const TRANSFORMER: &str = "rustman:plugin/body-transformer@1.0.0";
const KIND: &str = "rustman:plugin/request-kind@1.0.0";
const IMPORTER: &str = "rustman:plugin/format-importer@1.0.0";

/// The JSON Schema of the [`kind_plugin`] kind: `endpoint` and `action`
/// are required strings.
pub const SOAP_SCHEMA: &str = r#"{"type":"object","required":["endpoint","action"],"properties":{"endpoint":{"type":"string"},"action":{"type":"string"},"envelope":{"type":"string"}}}"#;

/// `$xor (ptr, len)`: XOR every byte of a buffer with 1, in place.
const XOR: &str = r#"
  (func $xor (param $p i32) (param $n i32)
    (local $i i32)
    (block $done
      (loop $next
        (br_if $done (i32.ge_u (local.get $i) (local.get $n)))
        (i32.store8 (i32.add (local.get $p) (local.get $i))
          (i32.xor (i32.load8_u (i32.add (local.get $p) (local.get $i))) (i32.const 1)))
        (local.set $i (i32.add (local.get $i) (i32.const 1)))
        (br $next))))
"#;

/// A transformer (`encrypt` for requests, `decrypt` for responses) that
/// XORs every body byte with 1 and keeps the headers: a stand-in cipher
/// that keeps ASCII text ASCII, so either direction undoes the other.
pub fn xor_transformer() -> Vec<u8> {
    let mut data = Data::default();
    let request = data.string_list(&["encrypt"]);
    let response = data.string_list(&["decrypt"]);
    let body = format!(
        r#"
      {XOR}
      (func $ok_message (param $h i32) (param $hl i32) (param $b i32) (param $bl i32) (result i32)
        (call $xor (local.get $b) (local.get $bl))
        (i32.store8 (i32.const {AREA}) (i32.const 0))
        (i32.store (i32.const {a4}) (local.get $h))
        (i32.store (i32.const {a8}) (local.get $hl))
        (i32.store (i32.const {a12}) (local.get $b))
        (i32.store (i32.const {a16}) (local.get $bl))
        (i32.const {AREA}))
      (func (export "{TRANSFORMER}#request-transformers") (result i32) (i32.const {request}))
      (func (export "{TRANSFORMER}#response-transformers") (result i32) (i32.const {response}))
      (func (export "{TRANSFORMER}#transform-request")
        (param i32 i32 i32 i32 i32 i32 i32 i32) (result i32)
        (call $ok_message (local.get 4) (local.get 5) (local.get 6) (local.get 7)))
      (func (export "{TRANSFORMER}#transform-response")
        (param i32 i32 i32 i32 i32 i32 i32) (result i32)
        (call $ok_message (local.get 3) (local.get 4) (local.get 5) (local.get 6)))
    "#,
        a4 = AREA + 4,
        a8 = AREA + 8,
        a12 = AREA + 12,
        a16 = AREA + 16,
    );
    component("transformer", "", &data, &body)
}

/// A kind plugin registering the kind `name` with [`SOAP_SCHEMA`] and a
/// pane of three fields (`endpoint` as text, `action` as a select,
/// `envelope` as XML code) and two views (raw, JSON tree). Executing a
/// request returns the summary `<name> <request JSON>`, status 200 and the
/// request JSON as both bodies.
pub fn kind_plugin(name: &str) -> Vec<u8> {
    let mut data = Data::default();
    let string = |data: &mut Data, text: &str| data.string(text);
    let kind = string(&mut data, name);
    let schema = string(&mut data, SOAP_SCHEMA);
    let endpoint = string(&mut data, "endpoint");
    let endpoint_label = string(&mut data, "Endpoint");
    let action = string(&mut data, "action");
    let action_label = string(&mut data, "SOAP action");
    let envelope = string(&mut data, "envelope");
    let envelope_label = string(&mut data, "Envelope");
    let xml = string(&mut data, "xml");
    let options = [
        string(&mut data, "GetOrder"),
        string(&mut data, "ListOrders"),
    ];
    let option_words: Vec<u32> = options.iter().flat_map(|(p, l)| [*p, *l]).collect();
    let option_list = data.words(&option_words);
    // pane-field: name, label, editor (tag; payload).
    let fields = data.words(&[
        endpoint.0,
        endpoint.1,
        endpoint_label.0,
        endpoint_label.1,
        0,
        0,
        0,
        action.0,
        action.1,
        action_label.0,
        action_label.1,
        3,
        option_list,
        2,
        envelope.0,
        envelope.1,
        envelope_label.0,
        envelope_label.1,
        1,
        xml.0,
        xml.1,
    ]);
    let views = data.raw(&[0, 1]);
    let info = data.words(&[kind.0, kind.1, schema.0, schema.1, fields, 3, views, 2]);
    let kinds = data.words(&[info, 1]);
    let prefix = format!("{name} ");
    let (prefix, prefix_len) = data.string(&prefix);
    let body = format!(
        r#"
      (func $copy (param $to i32) (param $from i32) (param $len i32) (result i32)
        (memory.copy (local.get $to) (local.get $from) (local.get $len))
        (i32.add (local.get $to) (local.get $len)))
      (func (export "{KIND}#kinds") (result i32) (i32.const {kinds}))
      (func (export "{KIND}#execute") (param i32 i32 i32 i32) (result i32)
        (local $summary i32)
        (local.set $summary (call $alloc (i32.add (local.get 3) (i32.const {prefix_len})) (i32.const 1)))
        (drop (call $copy
          (call $copy (local.get $summary) (i32.const {prefix}) (i32.const {prefix_len}))
          (local.get 2) (local.get 3)))
        (i32.store8 (i32.const {AREA}) (i32.const 0))
        (i32.store (i32.const {a4}) (local.get $summary))
        (i32.store (i32.const {a8}) (i32.add (local.get 3) (i32.const {prefix_len})))
        (i32.store (i32.const {a12}) (local.get 2))
        (i32.store (i32.const {a16}) (local.get 3))
        (i32.store8 (i32.const {a20}) (i32.const 1))
        (i32.store16 (i32.const {a22}) (i32.const 200))
        (i32.store (i32.const {a24}) (i32.const 0))
        (i32.store (i32.const {a28}) (i32.const 0))
        (i32.store (i32.const {a32}) (local.get 2))
        (i32.store (i32.const {a36}) (local.get 3))
        (i32.const {AREA}))
    "#,
        a4 = AREA + 4,
        a8 = AREA + 8,
        a12 = AREA + 12,
        a16 = AREA + 16,
        a20 = AREA + 20,
        a22 = AREA + 22,
        a24 = AREA + 24,
        a28 = AREA + 28,
        a32 = AREA + 32,
        a36 = AREA + 36,
    );
    component("kind", "", &data, &body)
}

/// An importer (format `copy`) that returns its source, unchanged, as the
/// request file `imported.rman.yaml`, and no environment.
pub fn copy_importer() -> Vec<u8> {
    let mut data = Data::default();
    let formats = data.string_list(&["copy"]);
    let (path, path_len) = data.string("imported.rman.yaml");
    // One imported-file record (path, content), filled in per call.
    let file = data.words(&[path, path_len, 0, 0]);
    let body = format!(
        r#"
      (func (export "{IMPORTER}#formats") (result i32) (i32.const {formats}))
      (func (export "{IMPORTER}#convert") (param i32 i32 i32 i32 i32 i32) (result i32)
        (i32.store (i32.const {f8}) (local.get 4))
        (i32.store (i32.const {f12}) (local.get 5))
        (i32.store8 (i32.const {AREA}) (i32.const 0))
        (i32.store (i32.const {a4}) (i32.const {file}))
        (i32.store (i32.const {a8}) (i32.const 1))
        (i32.store (i32.const {a12}) (i32.const 0))
        (i32.store (i32.const {a16}) (i32.const 0))
        (i32.const {AREA}))
    "#,
        f8 = file + 8,
        f12 = file + 12,
        a4 = AREA + 4,
        a8 = AREA + 8,
        a12 = AREA + 12,
        a16 = AREA + 16,
    );
    component("importer", "", &data, &body)
}
