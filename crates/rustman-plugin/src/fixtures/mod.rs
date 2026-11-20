//! Test plugins, built in-process from WebAssembly text (the
//! `test-fixtures` feature). Each is a core module written against the
//! canonical ABI and wrapped into a component for one world of the
//! `rustman:plugin` package with `wit-component`, so tests need no
//! `wasm32-wasip2` toolchain and no checked-in binaries.
//!
//! Memory layout of every fixture: static data from [`DATA`], a scratch
//! return area at [`RET`], an output buffer at [`OUT`], the export return
//! area at [`AREA`], and a bump heap from 64 KiB on.

use std::fmt::Write as _;

use crate::source::embed_manifest;

mod extensions;
mod points;

pub use extensions::{
    REPORT_PREFIX, faker, hmac_signer, host_rewriter, price_check, report_renderer, trapping_auth,
};
pub use points::{SOAP_SCHEMA, copy_importer, kind_plugin, xor_transformer};

const DATA: u32 = 1024;
const RET: u32 = 4096;
const RET2: u32 = 4160;
const OUT: u32 = 8192;
const AREA: u32 = 12288;

/// The bump allocator and `cabi_realloc` every fixture shares.
const PRELUDE: &str = r#"
  (memory (export "memory") 2)
  (global $heap (mut i32) (i32.const 65536))
  (global $cursor (mut i32) (i32.const 8192))
  (func $alloc (param $size i32) (param $align i32) (result i32)
    (local $p i32) (local $end i32)
    (local.set $p
      (i32.and
        (i32.add (global.get $heap) (i32.sub (local.get $align) (i32.const 1)))
        (i32.sub (i32.const 0) (local.get $align))))
    (local.set $end (i32.add (local.get $p) (local.get $size)))
    (block $ok
      (loop $grow
        (br_if $ok (i32.le_u (local.get $end) (i32.mul (memory.size) (i32.const 65536))))
        (if (i32.eq (memory.grow (i32.const 1)) (i32.const -1)) (then unreachable))
        (br $grow)))
    (global.set $heap (local.get $end))
    (local.get $p))
  (func (export "cabi_realloc") (param $old i32) (param $old_size i32) (param $align i32) (param $new_size i32) (result i32)
    (local $p i32)
    (local.set $p (call $alloc (local.get $new_size) (local.get $align)))
    (if (local.get $old)
      (then (memory.copy (local.get $p) (local.get $old) (local.get $old_size))))
    (local.get $p))
  (func $record (param $ret i32) (param $offset i32)
    (i32.store8 (global.get $cursor) (i32.add (i32.const 48) (i32.load8_u (local.get $ret))))
    (i32.store8
      (i32.add (global.get $cursor) (i32.const 1))
      (select
        (i32.add (i32.const 48) (i32.load8_u (i32.add (local.get $ret) (local.get $offset))))
        (i32.const 45)
        (i32.load8_u (local.get $ret))))
    (global.set $cursor (i32.add (global.get $cursor) (i32.const 2))))
  (func $ok_string (param $ptr i32) (param $len i32) (result i32)
    (i32.store8 (i32.const 12288) (i32.const 0))
    (i32.store (i32.const 12292) (local.get $ptr))
    (i32.store (i32.const 12296) (local.get $len))
    (i32.const 12288))
  (func $err_string (param $ptr i32) (param $len i32) (result i32)
    (i32.store8 (i32.const 12288) (i32.const 1))
    (i32.store (i32.const 12292) (local.get $ptr))
    (i32.store (i32.const 12296) (local.get $len))
    (i32.const 12288))
"#;

/// Static data placed at [`DATA`], with the offsets of what was added.
#[derive(Default)]
struct Data {
    bytes: Vec<u8>,
}

impl Data {
    fn at(&self) -> u32 {
        DATA + u32::try_from(self.bytes.len()).expect("fixture data is small")
    }

    fn align(&mut self) {
        while !self.bytes.len().is_multiple_of(8) {
            self.bytes.push(0);
        }
    }

    /// A string: `(ptr, len)`.
    fn string(&mut self, text: &str) -> (u32, u32) {
        let at = self.at();
        self.bytes.extend_from_slice(text.as_bytes());
        self.align();
        (
            at,
            u32::try_from(text.len()).expect("fixture strings are small"),
        )
    }

    /// A `list<string>` return area: the address of `(ptr, len)`.
    fn string_list(&mut self, items: &[&str]) -> u32 {
        let strings: Vec<(u32, u32)> = items.iter().map(|s| self.string(s)).collect();
        let list = self.at();
        for (ptr, len) in &strings {
            self.bytes.extend_from_slice(&ptr.to_le_bytes());
            self.bytes.extend_from_slice(&len.to_le_bytes());
        }
        let area = self.at();
        self.bytes.extend_from_slice(&list.to_le_bytes());
        let count = u32::try_from(items.len()).expect("fixture lists are small");
        self.bytes.extend_from_slice(&count.to_le_bytes());
        area
    }

    /// Little-endian words, 4-aligned: their address.
    fn words(&mut self, words: &[u32]) -> u32 {
        while !self.bytes.len().is_multiple_of(4) {
            self.bytes.push(0);
        }
        let at = self.at();
        for word in words {
            self.bytes.extend_from_slice(&word.to_le_bytes());
        }
        at
    }

    /// Raw bytes: their address.
    fn raw(&mut self, bytes: &[u8]) -> u32 {
        let at = self.at();
        self.bytes.extend_from_slice(bytes);
        self.align();
        at
    }

    fn segment(&self) -> String {
        let mut text = format!("(data (i32.const {DATA}) \"");
        for byte in &self.bytes {
            let _ = write!(text, "\\{byte:02x}");
        }
        text.push_str("\")");
        text
    }
}

/// A component for `world`, from core module parts: `imports`, then the
/// prelude, the static data and `body`.
fn component(world: &str, imports: &str, data: &Data, body: &str) -> Vec<u8> {
    let module = format!("(module {imports} {PRELUDE} {} {body})", data.segment());
    let mut module = wat::parse_str(&module).expect("fixture WAT parses");
    let mut resolve = wit_parser::Resolve::default();
    let package = resolve
        .push_source("plugin.wit", rustman_plugin_sdk::WIT)
        .expect("the WIT package parses");
    let world = resolve
        .select_world(&[package], Some(world))
        .expect("the fixture world exists");
    wit_component::embed_component_metadata(
        &mut module,
        &resolve,
        world,
        wit_component::StringEncoding::UTF8,
        false,
    )
    .expect("the WIT metadata embeds");
    let mut encoder = wit_component::ComponentEncoder::default();
    encoder
        .module(&module)
        .expect("the fixture module matches its world");
    encoder.validate(true);
    encoder.encode().expect("the fixture component encodes")
}

/// `wasm` with a manifest embedded (as a URL source must have it).
pub fn with_manifest(wasm: &[u8], manifest: &str) -> Vec<u8> {
    embed_manifest(wasm, manifest)
}

/// A manifest for `name` with `extension_points` and `capabilities`.
pub fn manifest(name: &str, extension_points: &[&str], capabilities: &[&str]) -> String {
    let quote = |items: &[&str]| {
        let quoted: Vec<String> = items.iter().map(|i| format!("\"{i}\"")).collect();
        quoted.join(", ")
    };
    format!(
        "name = \"{name}\"\nversion = \"0.1.0\"\napi = 1\nextension_points = [{}]\ncapabilities = [{}]\n",
        quote(extension_points),
        quote(capabilities)
    )
}

const GENERATOR: &str = "rustman:plugin/value-generator@1.0.0";
const ASSERTION: &str = "rustman:plugin/assertion-fn@1.0.0";

/// The `names` export of a generator.
fn generator_names(data: &mut Data, names: &[&str]) -> String {
    let area = data.string_list(names);
    format!(r#"(func (export "{GENERATOR}#names") (result i32) (i32.const {area}))"#)
}

/// The `functions` export of an assertion plugin.
fn assertion_functions(data: &mut Data, names: &[&str]) -> String {
    let area = data.string_list(names);
    format!(r#"(func (export "{ASSERTION}#functions") (result i32) (i32.const {area}))"#)
}

/// A generator (`probe`) that calls every host import and returns two
/// characters per call: the result tag (`0`: ok, `1`: error) and the
/// error tag (`0`: capability-denied), or `-` after an ok result. With no grants it returns
/// `10101010101010`: env, fs read, fs list, clock, random, http and the
/// `api_token` secret, in that order.
pub fn adversarial() -> Vec<u8> {
    let imports = r#"
      (import "rustman:plugin/env@1.0.0" "get" (func $env_get (param i32 i32 i32)))
      (import "rustman:plugin/fs-read@1.0.0" "read" (func $fs_read (param i32 i32 i32)))
      (import "rustman:plugin/fs-read@1.0.0" "list-dir" (func $fs_list (param i32 i32 i32)))
      (import "rustman:plugin/clock@1.0.0" "now" (func $clock_now (param i32)))
      (import "rustman:plugin/random@1.0.0" "bytes" (func $random_bytes (param i32 i32)))
      (import "rustman:plugin/http@1.0.0" "fetch"
        (func $fetch (param i32 i32 i32 i32 i32 i32 i32 i32 i32)))
      (import "rustman:plugin/secrets@1.0.0" "lookup" (func $lookup (param i32 i32 i32)))
      (import "rustman:plugin/secrets@1.0.0" "[method]secret-ref.reveal"
        (func $reveal (param i32 i32)))
      (import "rustman:plugin/secrets@1.0.0" "[resource-drop]secret-ref"
        (func $drop_secret (param i32)))
    "#;
    let mut data = Data::default();
    let names = generator_names(&mut data, &["probe"]);
    let (home, home_len) = data.string("HOME");
    let (file, file_len) = data.string("secret.txt");
    let (dot, dot_len) = data.string(".");
    let (get, get_len) = data.string("GET");
    let (url, url_len) = data.string("https://evil.example.net/");
    let (token, token_len) = data.string("api_token");
    let body = format!(
        r#"
      {names}
      (func (export "{GENERATOR}#generate") (param i32 i32 i32 i32) (result i32)
        (call $env_get (i32.const {home}) (i32.const {home_len}) (i32.const {RET}))
        (call $record (i32.const {RET}) (i32.const 4))
        (call $fs_read (i32.const {file}) (i32.const {file_len}) (i32.const {RET}))
        (call $record (i32.const {RET}) (i32.const 4))
        (call $fs_list (i32.const {dot}) (i32.const {dot_len}) (i32.const {RET}))
        (call $record (i32.const {RET}) (i32.const 4))
        (call $clock_now (i32.const {RET}))
        (call $record (i32.const {RET}) (i32.const 8))
        (call $random_bytes (i32.const 16) (i32.const {RET}))
        (call $record (i32.const {RET}) (i32.const 4))
        (call $fetch (i32.const {get}) (i32.const {get_len}) (i32.const {url}) (i32.const {url_len})
          (i32.const 0) (i32.const 0) (i32.const 0) (i32.const 0) (i32.const {RET}))
        (call $record (i32.const {RET}) (i32.const 4))
        (call $lookup (i32.const {token}) (i32.const {token_len}) (i32.const {RET}))
        (if (i32.load8_u (i32.const {RET}))
          (then
            (call $reveal (i32.load (i32.const {}) ) (i32.const {RET2}))
            (call $record (i32.const {RET2}) (i32.const 4))
            (call $drop_secret (i32.load (i32.const {})))))
        (call $ok_string (i32.const {OUT}) (i32.sub (global.get $cursor) (i32.const {OUT}))))
    "#,
        RET + 4,
        RET + 4,
    );
    component("generator", imports, &data, &body)
}

/// An assertion plugin (`my-check`'s `spin`) whose `check` never returns.
pub fn busy_loop() -> Vec<u8> {
    let mut data = Data::default();
    let functions = assertion_functions(&mut data, &["spin"]);
    let body = format!(
        r#"
      {functions}
      (func (export "{ASSERTION}#check") (param i32) (result i32)
        (loop $forever (br $forever))
        (i32.const 0))
    "#
    );
    component("assertion", "", &data, &body)
}

/// An assertion plugin whose `check` grows its memory until it is stopped.
pub fn allocation_bomb() -> Vec<u8> {
    let mut data = Data::default();
    let functions = assertion_functions(&mut data, &["grow"]);
    let body = format!(
        r#"
      {functions}
      (func (export "{ASSERTION}#check") (param i32) (result i32)
        (loop $more
          (drop (memory.grow (i32.const 16)))
          (br $more))
        (i32.const 0))
    "#
    );
    component("assertion", "", &data, &body)
}

/// An assertion plugin whose `check` traps (`unreachable`).
pub fn trapping() -> Vec<u8> {
    let mut data = Data::default();
    let functions = assertion_functions(&mut data, &["boom"]);
    let body = format!(
        r#"
      {functions}
      (func (export "{ASSERTION}#check") (param i32) (result i32) unreachable)
    "#
    );
    component("assertion", "", &data, &body)
}

/// A generator (`echo`) that reveals the `api_token` secret, logs it and
/// returns it; without the grant it returns the host error's message.
pub fn secret_echo() -> Vec<u8> {
    let imports = r#"
      (import "rustman:plugin/secrets@1.0.0" "lookup" (func $lookup (param i32 i32 i32)))
      (import "rustman:plugin/secrets@1.0.0" "[method]secret-ref.reveal"
        (func $reveal (param i32 i32)))
      (import "rustman:plugin/log@1.0.0" "write" (func $log (param i32 i32 i32)))
    "#;
    let mut data = Data::default();
    let names = generator_names(&mut data, &["echo"]);
    let (token, token_len) = data.string("api_token");
    let (missing, missing_len) = data.string("no such secret");
    let body = format!(
        r#"
      {names}
      (func (export "{GENERATOR}#generate") (param i32 i32 i32 i32) (result i32)
        (call $lookup (i32.const {token}) (i32.const {token_len}) (i32.const {RET}))
        (if (i32.eqz (i32.load8_u (i32.const {RET})))
          (then (return (call $err_string (i32.const {missing}) (i32.const {missing_len})))))
        (call $reveal (i32.load (i32.const {handle})) (i32.const {RET2}))
        (if (i32.load8_u (i32.const {RET2}))
          (then (return (call $err_string
            (i32.load (i32.const {err_ptr})) (i32.load (i32.const {err_len}))))))
        (call $log (i32.const 1) (i32.load (i32.const {ok_ptr})) (i32.load (i32.const {ok_len})))
        (call $ok_string (i32.load (i32.const {ok_ptr})) (i32.load (i32.const {ok_len}))))
    "#,
        handle = RET + 4,
        ok_ptr = RET2 + 4,
        ok_len = RET2 + 8,
        err_ptr = RET2 + 8,
        err_len = RET2 + 12,
    );
    component("generator", imports, &data, &body)
}

/// A generator (`fetch`) that GETs its first argument, with an
/// `X-Token` header holding the `api_token` secret when it may reveal it,
/// and returns the response body; a host error returns its message.
pub fn fetcher() -> Vec<u8> {
    let imports = r#"
      (import "rustman:plugin/http@1.0.0" "fetch"
        (func $fetch (param i32 i32 i32 i32 i32 i32 i32 i32 i32)))
      (import "rustman:plugin/secrets@1.0.0" "lookup" (func $lookup (param i32 i32 i32)))
      (import "rustman:plugin/secrets@1.0.0" "[method]secret-ref.reveal"
        (func $reveal (param i32 i32)))
    "#;
    let mut data = Data::default();
    let names = generator_names(&mut data, &["fetch"]);
    let (get, get_len) = data.string("GET");
    let (token, token_len) = data.string("api_token");
    let (header, header_len) = data.string("X-Token");
    // One header record (name, value) at HEADER, filled in when revealed.
    let header_record = RET2 + 32;
    let body = format!(
        r#"
      {names}
      (func (export "{GENERATOR}#generate") (param i32 i32 i32 i32) (result i32)
        (local $headers i32)
        (call $lookup (i32.const {token}) (i32.const {token_len}) (i32.const {RET}))
        (if (i32.load8_u (i32.const {RET}))
          (then
            (call $reveal (i32.load (i32.const {handle})) (i32.const {RET2}))
            (if (i32.eqz (i32.load8_u (i32.const {RET2})))
              (then
                (i32.store (i32.const {header_record}) (i32.const {header}))
                (i32.store (i32.const {hr4}) (i32.const {header_len}))
                (i32.store (i32.const {hr8}) (i32.load (i32.const {ok_ptr})))
                (i32.store (i32.const {hr12}) (i32.load (i32.const {ok_len})))
                (local.set $headers (i32.const 1))))))
        (call $fetch (i32.const {get}) (i32.const {get_len})
          (i32.load (local.get 2)) (i32.load (i32.add (local.get 2) (i32.const 4)))
          (i32.const {header_record}) (local.get $headers) (i32.const 0) (i32.const 0)
          (i32.const {RET}))
        (if (i32.load8_u (i32.const {RET}))
          (then (return (call $err_string
            (i32.load (i32.const {err_ptr})) (i32.load (i32.const {err_len}))))))
        (call $ok_string (i32.load (i32.const {body_ptr})) (i32.load (i32.const {body_len}))))
    "#,
        handle = RET + 4,
        ok_ptr = RET2 + 4,
        ok_len = RET2 + 8,
        hr4 = header_record + 4,
        hr8 = header_record + 8,
        hr12 = header_record + 12,
        err_ptr = RET + 8,
        err_len = RET + 12,
        body_ptr = RET + 16,
        body_len = RET + 20,
    );
    component("generator", imports, &data, &body)
}

/// A transformer (`echo` both ways) that returns the message it is given.
pub fn echo_transformer() -> Vec<u8> {
    const TRANSFORMER: &str = "rustman:plugin/body-transformer@1.0.0";
    let mut data = Data::default();
    let request = data.string_list(&["echo"]);
    let response = data.string_list(&["echo"]);
    let body = format!(
        r#"
      (func $ok_message (param $h i32) (param $hl i32) (param $b i32) (param $bl i32) (result i32)
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

/// An auth plugin (`v1`) that sets `Authorization: <aws_secret_key>`,
/// revealed through its grant, or fails with the host error's message.
pub fn signer() -> Vec<u8> {
    signer_header("Authorization")
}

/// [`signer`], setting the secret in the header `header`.
pub fn signer_header(header: &str) -> Vec<u8> {
    const AUTH: &str = "rustman:plugin/auth-scheme@1.0.0";
    let imports = r#"
      (import "rustman:plugin/secrets@1.0.0" "lookup" (func $lookup (param i32 i32 i32)))
      (import "rustman:plugin/secrets@1.0.0" "[method]secret-ref.reveal"
        (func $reveal (param i32 i32)))
    "#;
    let mut data = Data::default();
    let schemes = data.string_list(&["v1"]);
    let (key, key_len) = data.string("aws_secret_key");
    let (name, name_len) = data.string(header);
    let (missing, missing_len) = data.string("no such secret");
    let header_record = RET2 + 32;
    let body = format!(
        r#"
      (func (export "{AUTH}#schemes") (result i32) (i32.const {schemes}))
      (func (export "{AUTH}#sign") (param i32 i32 i32 i32 i32 i32 i32 i32 i32 i32 i32 i32) (result i32)
        (call $lookup (i32.const {key}) (i32.const {key_len}) (i32.const {RET}))
        (if (i32.eqz (i32.load8_u (i32.const {RET})))
          (then (return (call $err_string (i32.const {missing}) (i32.const {missing_len})))))
        (call $reveal (i32.load (i32.const {handle})) (i32.const {RET2}))
        (if (i32.load8_u (i32.const {RET2}))
          (then (return (call $err_string
            (i32.load (i32.const {err_ptr})) (i32.load (i32.const {err_len}))))))
        (i32.store (i32.const {header_record}) (i32.const {name}))
        (i32.store (i32.const {hr4}) (i32.const {name_len}))
        (i32.store (i32.const {hr8}) (i32.load (i32.const {ok_ptr})))
        (i32.store (i32.const {hr12}) (i32.load (i32.const {ok_len})))
        (i32.store8 (i32.const {AREA}) (i32.const 0))
        (i32.store (i32.const {a4}) (i32.const {header_record}))
        (i32.store (i32.const {a8}) (i32.const 1))
        (i32.store (i32.const {a12}) (i32.const 0))
        (i32.store (i32.const {a16}) (i32.const 0))
        (i32.store8 (i32.const {a20}) (i32.const 0))
        (i32.const {AREA}))
    "#,
        handle = RET + 4,
        ok_ptr = RET2 + 4,
        ok_len = RET2 + 8,
        err_ptr = RET2 + 8,
        err_len = RET2 + 12,
        hr4 = header_record + 4,
        hr8 = header_record + 8,
        hr12 = header_record + 12,
        a4 = AREA + 4,
        a8 = AREA + 8,
        a12 = AREA + 12,
        a16 = AREA + 16,
        a20 = AREA + 20,
    );
    component("auth", imports, &data, &body)
}

/// A component that imports a WASI interface: it must not link.
pub fn wasi_importer() -> Vec<u8> {
    let module = r#"(module
      (import "wasi:cli/environment@0.2.0" "initial-cwd" (func))
      (memory (export "memory") 1))"#;
    let mut module = wat::parse_str(module).expect("fixture WAT parses");
    let wit = "package test:wasi;\n\
        world w { import wasi:cli/environment@0.2.0; }\n";
    let environment = "package wasi:cli@0.2.0;\n\
        interface environment { initial-cwd: func(); }\n";
    let mut resolve = wit_parser::Resolve::default();
    resolve
        .push_source("environment.wit", environment)
        .expect("the WASI stub parses");
    let package = resolve.push_source("w.wit", wit).expect("the world parses");
    let world = resolve
        .select_world(&[package], Some("w"))
        .expect("world w");
    wit_component::embed_component_metadata(
        &mut module,
        &resolve,
        world,
        wit_component::StringEncoding::UTF8,
        false,
    )
    .expect("the WIT metadata embeds");
    let mut encoder = wit_component::ComponentEncoder::default();
    encoder.module(&module).expect("the module matches");
    encoder.encode().expect("the component encodes")
}
