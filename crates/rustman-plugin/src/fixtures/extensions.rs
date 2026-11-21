//! Test plugins for the extension points (add-plugin-system section 5):
//! an HMAC-SHA256 request signer, an auth plugin that rewrites the host,
//! an auth plugin that traps, a `faker` value generator and a
//! `price-consistent` assertion function. Like the other fixtures they are
//! WebAssembly text, wrapped into components in-process.

use super::{AREA, Data, RET, RET2, component};

const AUTH: &str = "rustman:plugin/auth-scheme@1.0.0";
const GENERATOR: &str = "rustman:plugin/value-generator@1.0.0";
const ASSERTION: &str = "rustman:plugin/assertion-fn@1.0.0";
const REPORT: &str = "rustman:plugin/report-renderer@1.0.0";

/// What [`report_renderer`] writes before the report.
pub const REPORT_PREFIX: &[u8] = b"ACME\x01XLSX\n";

/// The SHA-256 round constants.
const K: [u32; 64] = [
    0x428a_2f98,
    0x7137_4491,
    0xb5c0_fbcf,
    0xe9b5_dba5,
    0x3956_c25b,
    0x59f1_11f1,
    0x923f_82a4,
    0xab1c_5ed5,
    0xd807_aa98,
    0x1283_5b01,
    0x2431_85be,
    0x550c_7dc3,
    0x72be_5d74,
    0x80de_b1fe,
    0x9bdc_06a7,
    0xc19b_f174,
    0xe49b_69c1,
    0xefbe_4786,
    0x0fc1_9dc6,
    0x240c_a1cc,
    0x2de9_2c6f,
    0x4a74_84aa,
    0x5cb0_a9dc,
    0x76f9_88da,
    0x983e_5152,
    0xa831_c66d,
    0xb003_27c8,
    0xbf59_7fc7,
    0xc6e0_0bf3,
    0xd5a7_9147,
    0x06ca_6351,
    0x1429_2967,
    0x27b7_0a85,
    0x2e1b_2138,
    0x4d2c_6dfc,
    0x5338_0d13,
    0x650a_7354,
    0x766a_0abb,
    0x81c2_c92e,
    0x9272_2c85,
    0xa2bf_e8a1,
    0xa81a_664b,
    0xc24b_8b70,
    0xc76c_51a3,
    0xd192_e819,
    0xd699_0624,
    0xf40e_3585,
    0x106a_a070,
    0x19a4_c116,
    0x1e37_6c08,
    0x2748_774c,
    0x34b0_bcb5,
    0x391c_0cb3,
    0x4ed8_aa4a,
    0x5b9c_ca4f,
    0x682e_6ff3,
    0x748f_82ee,
    0x78a5_636f,
    0x84c8_7814,
    0x8cc7_0208,
    0x90be_fffa,
    0xa450_6ceb,
    0xbef9_a3f7,
    0xc671_78f2,
];

/// The SHA-256 initial hash values.
const H: [u32; 8] = [
    0x6a09_e667,
    0xbb67_ae85,
    0x3c6e_f372,
    0xa54f_f53a,
    0x510e_527f,
    0x9b05_688c,
    0x1f83_d9ab,
    0x5be0_cd19,
];

/// String and formatting helpers: `$streq`, `$put`, `$utoa`, `$hex`.
const TEXT: &str = r#"
  (func $streq (param $a i32) (param $al i32) (param $b i32) (param $bl i32) (result i32)
    (local $i i32)
    (if (i32.ne (local.get $al) (local.get $bl)) (then (return (i32.const 0))))
    (block $done
      (loop $next
        (br_if $done (i32.ge_u (local.get $i) (local.get $al)))
        (if (i32.ne (i32.load8_u (i32.add (local.get $a) (local.get $i)))
                    (i32.load8_u (i32.add (local.get $b) (local.get $i))))
          (then (return (i32.const 0))))
        (local.set $i (i32.add (local.get $i) (i32.const 1)))
        (br $next)))
    (i32.const 1))
  (func $put (param $dst i32) (param $src i32) (param $len i32) (result i32)
    (memory.copy (local.get $dst) (local.get $src) (local.get $len))
    (i32.add (local.get $dst) (local.get $len)))
  (func $utoa (param $dst i32) (param $n i64) (result i32)
    (local $p i32) (local $len i32)
    (local.set $p (i32.add (call $alloc (i32.const 24) (i32.const 1)) (i32.const 24)))
    (loop $digit
      (local.set $p (i32.sub (local.get $p) (i32.const 1)))
      (i32.store8 (local.get $p)
        (i32.add (i32.const 48) (i32.wrap_i64 (i64.rem_u (local.get $n) (i64.const 10)))))
      (local.set $len (i32.add (local.get $len) (i32.const 1)))
      (local.set $n (i64.div_u (local.get $n) (i64.const 10)))
      (br_if $digit (i64.ne (local.get $n) (i64.const 0))))
    (call $put (local.get $dst) (local.get $p) (local.get $len)))
  (func $nibble (param $n i32) (result i32)
    (i32.add (local.get $n)
      (select (i32.const 48) (i32.const 87) (i32.lt_u (local.get $n) (i32.const 10)))))
  (func $hex (param $dst i32) (param $src i32) (param $len i32) (result i32)
    (local $i i32) (local $b i32)
    (block $done
      (loop $next
        (br_if $done (i32.ge_u (local.get $i) (local.get $len)))
        (local.set $b (i32.load8_u (i32.add (local.get $src) (local.get $i))))
        (i32.store8 (local.get $dst) (call $nibble (i32.shr_u (local.get $b) (i32.const 4))))
        (i32.store8 (i32.add (local.get $dst) (i32.const 1))
          (call $nibble (i32.and (local.get $b) (i32.const 15))))
        (local.set $dst (i32.add (local.get $dst) (i32.const 2)))
        (local.set $i (i32.add (local.get $i) (i32.const 1)))
        (br $next)))
    (local.get $dst))
"#;

/// `$sha256 (msg, len, out)` and `$hmac (key, key_len, msg, len, out)`,
/// with the round constants at `k`.
fn crypto(k: u32) -> String {
    let [h0, h1, h2, h3, h4, h5, h6, h7] = H.map(|h| format!("(i32.const {h})"));
    format!(
        r#"
  (func $bswap (param $x i32) (result i32)
    (i32.or
      (i32.or (i32.shl (local.get $x) (i32.const 24))
              (i32.shl (i32.and (local.get $x) (i32.const 0xff00)) (i32.const 8)))
      (i32.or (i32.and (i32.shr_u (local.get $x) (i32.const 8)) (i32.const 0xff00))
              (i32.shr_u (local.get $x) (i32.const 24)))))
  (func $w_at (param $w i32) (param $t i32) (result i32)
    (i32.load (i32.add (local.get $w) (i32.shl (local.get $t) (i32.const 2)))))
  (func $schedule (param $w i32) (param $blk i32)
    (local $t i32) (local $x i32) (local $y i32)
    (block $d1 (loop $l1
      (br_if $d1 (i32.ge_u (local.get $t) (i32.const 16)))
      (i32.store (i32.add (local.get $w) (i32.shl (local.get $t) (i32.const 2)))
        (call $bswap (i32.load (i32.add (local.get $blk) (i32.shl (local.get $t) (i32.const 2))))))
      (local.set $t (i32.add (local.get $t) (i32.const 1)))
      (br $l1)))
    (block $d2 (loop $l2
      (br_if $d2 (i32.ge_u (local.get $t) (i32.const 64)))
      (local.set $x (call $w_at (local.get $w) (i32.sub (local.get $t) (i32.const 15))))
      (local.set $y (call $w_at (local.get $w) (i32.sub (local.get $t) (i32.const 2))))
      (i32.store (i32.add (local.get $w) (i32.shl (local.get $t) (i32.const 2)))
        (i32.add
          (i32.add (call $w_at (local.get $w) (i32.sub (local.get $t) (i32.const 16)))
            (i32.xor (i32.xor (i32.rotr (local.get $x) (i32.const 7))
                              (i32.rotr (local.get $x) (i32.const 18)))
                     (i32.shr_u (local.get $x) (i32.const 3))))
          (i32.add (call $w_at (local.get $w) (i32.sub (local.get $t) (i32.const 7)))
            (i32.xor (i32.xor (i32.rotr (local.get $y) (i32.const 17))
                              (i32.rotr (local.get $y) (i32.const 19)))
                     (i32.shr_u (local.get $y) (i32.const 10))))))
      (local.set $t (i32.add (local.get $t) (i32.const 1)))
      (br $l2))))
  (func $compress (param $state i32) (param $w i32)
    (local $t i32) (local $t1 i32) (local $t2 i32)
    (local $a i32) (local $b i32) (local $c i32) (local $d i32)
    (local $e i32) (local $f i32) (local $g i32) (local $h i32)
    (local.set $a (i32.load (local.get $state)))
    (local.set $b (i32.load offset=4 (local.get $state)))
    (local.set $c (i32.load offset=8 (local.get $state)))
    (local.set $d (i32.load offset=12 (local.get $state)))
    (local.set $e (i32.load offset=16 (local.get $state)))
    (local.set $f (i32.load offset=20 (local.get $state)))
    (local.set $g (i32.load offset=24 (local.get $state)))
    (local.set $h (i32.load offset=28 (local.get $state)))
    (block $done (loop $round
      (br_if $done (i32.ge_u (local.get $t) (i32.const 64)))
      (local.set $t1
        (i32.add
          (i32.add (local.get $h)
            (i32.xor (i32.xor (i32.rotr (local.get $e) (i32.const 6))
                              (i32.rotr (local.get $e) (i32.const 11)))
                     (i32.rotr (local.get $e) (i32.const 25))))
          (i32.add
            (i32.xor (i32.and (local.get $e) (local.get $f))
                     (i32.and (i32.xor (local.get $e) (i32.const -1)) (local.get $g)))
            (i32.add (i32.load (i32.add (i32.const {k}) (i32.shl (local.get $t) (i32.const 2))))
                     (call $w_at (local.get $w) (local.get $t))))))
      (local.set $t2
        (i32.add
          (i32.xor (i32.xor (i32.rotr (local.get $a) (i32.const 2))
                            (i32.rotr (local.get $a) (i32.const 13)))
                   (i32.rotr (local.get $a) (i32.const 22)))
          (i32.xor (i32.xor (i32.and (local.get $a) (local.get $b))
                            (i32.and (local.get $a) (local.get $c)))
                   (i32.and (local.get $b) (local.get $c)))))
      (local.set $h (local.get $g))
      (local.set $g (local.get $f))
      (local.set $f (local.get $e))
      (local.set $e (i32.add (local.get $d) (local.get $t1)))
      (local.set $d (local.get $c))
      (local.set $c (local.get $b))
      (local.set $b (local.get $a))
      (local.set $a (i32.add (local.get $t1) (local.get $t2)))
      (local.set $t (i32.add (local.get $t) (i32.const 1)))
      (br $round)))
    (i32.store (local.get $state) (i32.add (i32.load (local.get $state)) (local.get $a)))
    (i32.store offset=4 (local.get $state) (i32.add (i32.load offset=4 (local.get $state)) (local.get $b)))
    (i32.store offset=8 (local.get $state) (i32.add (i32.load offset=8 (local.get $state)) (local.get $c)))
    (i32.store offset=12 (local.get $state) (i32.add (i32.load offset=12 (local.get $state)) (local.get $d)))
    (i32.store offset=16 (local.get $state) (i32.add (i32.load offset=16 (local.get $state)) (local.get $e)))
    (i32.store offset=20 (local.get $state) (i32.add (i32.load offset=20 (local.get $state)) (local.get $f)))
    (i32.store offset=24 (local.get $state) (i32.add (i32.load offset=24 (local.get $state)) (local.get $g)))
    (i32.store offset=28 (local.get $state) (i32.add (i32.load offset=28 (local.get $state)) (local.get $h))))
  (func $sha256 (param $msg i32) (param $len i32) (param $out i32)
    (local $total i32) (local $padded i32) (local $blk i32) (local $w i32) (local $state i32) (local $i i32)
    (local.set $total (i32.and (i32.add (local.get $len) (i32.const 72)) (i32.const -64)))
    (local.set $padded (call $alloc (local.get $total) (i32.const 8)))
    (memory.copy (local.get $padded) (local.get $msg) (local.get $len))
    (i32.store8 (i32.add (local.get $padded) (local.get $len)) (i32.const 0x80))
    (i32.store (i32.sub (i32.add (local.get $padded) (local.get $total)) (i32.const 8))
      (call $bswap (i32.shr_u (local.get $len) (i32.const 29))))
    (i32.store (i32.sub (i32.add (local.get $padded) (local.get $total)) (i32.const 4))
      (call $bswap (i32.shl (local.get $len) (i32.const 3))))
    (local.set $w (call $alloc (i32.const 256) (i32.const 4)))
    (local.set $state (call $alloc (i32.const 32) (i32.const 4)))
    (i32.store (local.get $state) {h0})
    (i32.store offset=4 (local.get $state) {h1})
    (i32.store offset=8 (local.get $state) {h2})
    (i32.store offset=12 (local.get $state) {h3})
    (i32.store offset=16 (local.get $state) {h4})
    (i32.store offset=20 (local.get $state) {h5})
    (i32.store offset=24 (local.get $state) {h6})
    (i32.store offset=28 (local.get $state) {h7})
    (local.set $blk (local.get $padded))
    (block $done (loop $blocks
      (br_if $done (i32.ge_u (local.get $blk) (i32.add (local.get $padded) (local.get $total))))
      (call $schedule (local.get $w) (local.get $blk))
      (call $compress (local.get $state) (local.get $w))
      (local.set $blk (i32.add (local.get $blk) (i32.const 64)))
      (br $blocks)))
    (block $copied (loop $word
      (br_if $copied (i32.ge_u (local.get $i) (i32.const 32)))
      (i32.store (i32.add (local.get $out) (local.get $i))
        (call $bswap (i32.load (i32.add (local.get $state) (local.get $i)))))
      (local.set $i (i32.add (local.get $i) (i32.const 4)))
      (br $word))))
  (func $hmac (param $key i32) (param $klen i32) (param $msg i32) (param $mlen i32) (param $out i32)
    (local $inner i32) (local $outer i32) (local $i i32) (local $k i32)
    (if (i32.gt_u (local.get $klen) (i32.const 64))
      (then
        (local.set $k (call $alloc (i32.const 32) (i32.const 4)))
        (call $sha256 (local.get $key) (local.get $klen) (local.get $k))
        (local.set $key (local.get $k))
        (local.set $klen (i32.const 32))))
    (local.set $inner (call $alloc (i32.add (i32.const 64) (local.get $mlen)) (i32.const 8)))
    (local.set $outer (call $alloc (i32.const 96) (i32.const 8)))
    (block $done (loop $pad
      (br_if $done (i32.ge_u (local.get $i) (i32.const 64)))
      (local.set $k
        (select (i32.load8_u (i32.add (local.get $key) (local.get $i)))
                (i32.const 0)
                (i32.lt_u (local.get $i) (local.get $klen))))
      (i32.store8 (i32.add (local.get $inner) (local.get $i)) (i32.xor (local.get $k) (i32.const 0x36)))
      (i32.store8 (i32.add (local.get $outer) (local.get $i)) (i32.xor (local.get $k) (i32.const 0x5c)))
      (local.set $i (i32.add (local.get $i) (i32.const 1)))
      (br $pad)))
    (memory.copy (i32.add (local.get $inner) (i32.const 64)) (local.get $msg) (local.get $mlen))
    (call $sha256 (local.get $inner) (i32.add (i32.const 64) (local.get $mlen))
      (i32.add (local.get $outer) (i32.const 64)))
    (call $sha256 (local.get $outer) (i32.const 96) (local.get $out)))
"#
    )
}

/// The round constants as little-endian words: their address.
fn round_constants(data: &mut Data) -> u32 {
    data.align();
    let at = data.at();
    for word in K {
        data.bytes.extend_from_slice(&word.to_le_bytes());
    }
    at
}

/// An auth plugin (`hmac`'s `v1` scheme) that signs
/// `<unix seconds>\n<method>\n<url>` with HMAC-SHA256 under the secret
/// named by its `key_var` param (revealed through its grant), and sets
/// `X-Timestamp` and `X-Signature` (lowercase hex).
pub fn hmac_signer() -> Vec<u8> {
    let imports = r#"
      (import "rustman:plugin/secrets@1.0.0" "lookup" (func $lookup (param i32 i32 i32)))
      (import "rustman:plugin/secrets@1.0.0" "[method]secret-ref.reveal"
        (func $reveal (param i32 i32)))
      (import "rustman:plugin/clock@1.0.0" "now" (func $now (param i32)))
    "#;
    let mut data = Data::default();
    let schemes = data.string_list(&["v1"]);
    let k = round_constants(&mut data);
    let (key_var, key_var_len) = data.string("key_var");
    let (no_param, no_param_len) = data.string("missing param key_var");
    let (missing, missing_len) = data.string("no such secret");
    let (x_sig, x_sig_len) = data.string("X-Signature");
    let (x_ts, x_ts_len) = data.string("X-Timestamp");
    let crypto = crypto(k);
    let body = format!(
        r#"
      {TEXT}
      {crypto}
      (func (export "{AUTH}#schemes") (result i32) (i32.const {schemes}))
      (func $secret_param (param $params i32) (param $count i32) (result i32)
        (local $i i32) (local $p i32)
        (block $none (loop $scan
          (br_if $none (i32.ge_u (local.get $i) (local.get $count)))
          (local.set $p (i32.add (local.get $params) (i32.shl (local.get $i) (i32.const 4))))
          (if (call $streq (i32.load (local.get $p)) (i32.load offset=4 (local.get $p))
                (i32.const {key_var}) (i32.const {key_var_len}))
            (then (return (local.get $p))))
          (local.set $i (i32.add (local.get $i) (i32.const 1)))
          (br $scan)))
        (i32.const 0))
      (func (export "{AUTH}#sign") (param i32 i32 i32 i32 i32 i32 i32 i32 i32 i32 i32 i32) (result i32)
        (local $param i32) (local $key i32) (local $keylen i32)
        (local $ts i32) (local $tslen i32) (local $msg i32) (local $end i32)
        (local $mac i32) (local $sig i32) (local $headers i32)
        (local.set $param (call $secret_param (local.get 10) (local.get 11)))
        (if (i32.eqz (local.get $param))
          (then (return (call $err_string (i32.const {no_param}) (i32.const {no_param_len})))))
        (call $lookup (i32.load offset=8 (local.get $param)) (i32.load offset=12 (local.get $param))
          (i32.const {RET}))
        (if (i32.eqz (i32.load8_u (i32.const {RET})))
          (then (return (call $err_string (i32.const {missing}) (i32.const {missing_len})))))
        (call $reveal (i32.load (i32.const {ret4})) (i32.const {RET2}))
        (if (i32.load8_u (i32.const {RET2}))
          (then (return (call $err_string
            (i32.load (i32.const {ret2_8})) (i32.load (i32.const {ret2_12}))))))
        (local.set $key (i32.load (i32.const {ret2_4})))
        (local.set $keylen (i32.load (i32.const {ret2_8})))
        (call $now (i32.const {RET}))
        (if (i32.load8_u (i32.const {RET}))
          (then (return (call $err_string
            (i32.load (i32.const {ret12})) (i32.load (i32.const {ret16}))))))
        (local.set $ts (call $alloc (i32.const 24) (i32.const 1)))
        (local.set $tslen
          (i32.sub (call $utoa (local.get $ts) (i64.load (i32.const {ret8}))) (local.get $ts)))
        (local.set $msg (call $alloc
          (i32.add (i32.add (local.get $tslen) (local.get 3)) (i32.add (local.get 5) (i32.const 2)))
          (i32.const 1)))
        (local.set $end (call $put (local.get $msg) (local.get $ts) (local.get $tslen)))
        (i32.store8 (local.get $end) (i32.const 10))
        (local.set $end (call $put (i32.add (local.get $end) (i32.const 1)) (local.get 2) (local.get 3)))
        (i32.store8 (local.get $end) (i32.const 10))
        (local.set $end (call $put (i32.add (local.get $end) (i32.const 1)) (local.get 4) (local.get 5)))
        (local.set $mac (call $alloc (i32.const 32) (i32.const 4)))
        (call $hmac (local.get $key) (local.get $keylen)
          (local.get $msg) (i32.sub (local.get $end) (local.get $msg)) (local.get $mac))
        (local.set $sig (call $alloc (i32.const 64) (i32.const 1)))
        (drop (call $hex (local.get $sig) (local.get $mac) (i32.const 32)))
        (local.set $headers (call $alloc (i32.const 32) (i32.const 4)))
        (i32.store (local.get $headers) (i32.const {x_sig}))
        (i32.store offset=4 (local.get $headers) (i32.const {x_sig_len}))
        (i32.store offset=8 (local.get $headers) (local.get $sig))
        (i32.store offset=12 (local.get $headers) (i32.const 64))
        (i32.store offset=16 (local.get $headers) (i32.const {x_ts}))
        (i32.store offset=20 (local.get $headers) (i32.const {x_ts_len}))
        (i32.store offset=24 (local.get $headers) (local.get $ts))
        (i32.store offset=28 (local.get $headers) (local.get $tslen))
        (i32.store8 (i32.const {AREA}) (i32.const 0))
        (i32.store (i32.const {a4}) (local.get $headers))
        (i32.store (i32.const {a8}) (i32.const 2))
        (i32.store (i32.const {a12}) (i32.const 0))
        (i32.store (i32.const {a16}) (i32.const 0))
        (i32.store8 (i32.const {a20}) (i32.const 0))
        (i32.const {AREA}))
    "#,
        ret4 = RET + 4,
        ret8 = RET + 8,
        ret12 = RET + 12,
        ret16 = RET + 16,
        ret2_4 = RET2 + 4,
        ret2_8 = RET2 + 8,
        ret2_12 = RET2 + 12,
        a4 = AREA + 4,
        a8 = AREA + 8,
        a12 = AREA + 12,
        a16 = AREA + 16,
        a20 = AREA + 20,
    );
    component("auth", imports, &data, &body)
}

/// An auth plugin (`v1`) whose result moves the request to
/// `https://evil.example.net/`.
pub fn host_rewriter() -> Vec<u8> {
    let mut data = Data::default();
    let schemes = data.string_list(&["v1"]);
    let (evil, evil_len) = data.string("https://evil.example.net/");
    let body = format!(
        r#"
      (func (export "{AUTH}#schemes") (result i32) (i32.const {schemes}))
      (func (export "{AUTH}#sign") (param i32 i32 i32 i32 i32 i32 i32 i32 i32 i32 i32 i32) (result i32)
        (i32.store8 (i32.const {AREA}) (i32.const 0))
        (i32.store (i32.const {a4}) (i32.const 0))
        (i32.store (i32.const {a8}) (i32.const 0))
        (i32.store (i32.const {a12}) (i32.const 0))
        (i32.store (i32.const {a16}) (i32.const 0))
        (i32.store8 (i32.const {a20}) (i32.const 1))
        (i32.store (i32.const {a24}) (i32.const {evil}))
        (i32.store (i32.const {a28}) (i32.const {evil_len}))
        (i32.const {AREA}))
    "#,
        a4 = AREA + 4,
        a8 = AREA + 8,
        a12 = AREA + 12,
        a16 = AREA + 16,
        a20 = AREA + 20,
        a24 = AREA + 24,
        a28 = AREA + 28,
    );
    component("auth", "", &data, &body)
}

/// An auth plugin (`v1`) that adds the value of its first param, as it
/// received it, as the query parameter `p`: what a plugin sees of a
/// param, redacted or not, reaches the server.
pub fn param_echo() -> Vec<u8> {
    let mut data = Data::default();
    let schemes = data.string_list(&["v1"]);
    let (name, name_len) = data.string("p");
    let (no_param, no_param_len) = data.string("no params");
    let pair = RET2 + 32;
    let body = format!(
        r#"
      (func (export "{AUTH}#schemes") (result i32) (i32.const {schemes}))
      (func (export "{AUTH}#sign") (param i32 i32 i32 i32 i32 i32 i32 i32 i32 i32 i32 i32) (result i32)
        (if (i32.eqz (local.get 11))
          (then (return (call $err_string (i32.const {no_param}) (i32.const {no_param_len})))))
        (i32.store (i32.const {pair}) (i32.const {name}))
        (i32.store (i32.const {p4}) (i32.const {name_len}))
        (i32.store (i32.const {p8}) (i32.load offset=8 (local.get 10)))
        (i32.store (i32.const {p12}) (i32.load offset=12 (local.get 10)))
        (i32.store8 (i32.const {AREA}) (i32.const 0))
        (i32.store (i32.const {a4}) (i32.const 0))
        (i32.store (i32.const {a8}) (i32.const 0))
        (i32.store (i32.const {a12}) (i32.const {pair}))
        (i32.store (i32.const {a16}) (i32.const 1))
        (i32.store8 (i32.const {a20}) (i32.const 0))
        (i32.const {AREA}))
    "#,
        p4 = pair + 4,
        p8 = pair + 8,
        p12 = pair + 12,
        a4 = AREA + 4,
        a8 = AREA + 8,
        a12 = AREA + 12,
        a16 = AREA + 16,
        a20 = AREA + 20,
    );
    component("auth", "", &data, &body)
}

/// An auth plugin (`v1`) whose `sign` traps (`unreachable`).
pub fn trapping_auth() -> Vec<u8> {
    let mut data = Data::default();
    let schemes = data.string_list(&["v1"]);
    let body = format!(
        r#"
      (func (export "{AUTH}#schemes") (result i32) (i32.const {schemes}))
      (func (export "{AUTH}#sign") (param i32 i32 i32 i32 i32 i32 i32 i32 i32 i32 i32 i32) (result i32)
        unreachable)
    "#
    );
    component("auth", "", &data, &body)
}

/// A generator (`faker`'s `email`) returning `user-<8 hex digits>@example.test`
/// from 4 random bytes, so each occurrence differs; without the `random`
/// grant it fails with the host error's message.
pub fn faker() -> Vec<u8> {
    let imports = r#"
      (import "rustman:plugin/random@1.0.0" "bytes" (func $random_bytes (param i32 i32)))
    "#;
    let mut data = Data::default();
    let names = data.string_list(&["email"]);
    let (user, user_len) = data.string("user-");
    let (domain, domain_len) = data.string("@example.test");
    let body = format!(
        r#"
      {TEXT}
      (func (export "{GENERATOR}#names") (result i32) (i32.const {names}))
      (func (export "{GENERATOR}#generate") (param i32 i32 i32 i32) (result i32)
        (local $out i32) (local $end i32)
        (call $random_bytes (i32.const 4) (i32.const {RET}))
        (if (i32.load8_u (i32.const {RET}))
          (then (return (call $err_string
            (i32.load (i32.const {ret8})) (i32.load (i32.const {ret12}))))))
        (local.set $out (call $alloc (i32.const 64) (i32.const 1)))
        (local.set $end (call $put (local.get $out) (i32.const {user}) (i32.const {user_len})))
        (local.set $end (call $hex (local.get $end) (i32.load (i32.const {ret4})) (i32.const 4)))
        (local.set $end (call $put (local.get $end) (i32.const {domain}) (i32.const {domain_len})))
        (call $ok_string (local.get $out) (i32.sub (local.get $end) (local.get $out))))
    "#,
        ret4 = RET + 4,
        ret8 = RET + 8,
        ret12 = RET + 12,
    );
    component("generator", imports, &data, &body)
}

/// An assertion function (`shop`'s `price-consistent`): the response body's
/// `"total":` must equal the sum of its `"amount":` values, read with one
/// decimal. Fails with `total 99.0 != sum 98.5`; errors without a total.
pub fn price_check() -> Vec<u8> {
    let mut data = Data::default();
    let functions = data.string_list(&["price-consistent"]);
    let (amount, amount_len) = data.string("\"amount\":");
    let (total, total_len) = data.string("\"total\":");
    let (no_total, no_total_len) = data.string("the response has no total");
    let (total_word, total_word_len) = data.string("total ");
    let (sum_word, sum_word_len) = data.string(" != sum ");
    let body = format!(
        r#"
      {TEXT}
      (func (export "{ASSERTION}#functions") (result i32) (i32.const {functions}))
      (func $verdict (param $tag i32) (param $ptr i32) (param $len i32) (result i32)
        (i32.store8 (i32.const {AREA}) (local.get $tag))
        (i32.store (i32.const {a4}) (local.get $ptr))
        (i32.store (i32.const {a8}) (local.get $len))
        (i32.const {AREA}))
      (func $starts (param $p i32) (param $end i32) (param $lit i32) (param $n i32) (result i32)
        (if (i32.gt_u (i32.add (local.get $p) (local.get $n)) (local.get $end))
          (then (return (i32.const 0))))
        (call $streq (local.get $p) (local.get $n) (local.get $lit) (local.get $n)))
      (func $tenths (param $p i32) (param $end i32) (result i32)
        (local $v i32) (local $c i32) (local $frac i32)
        (block $s (loop $space
          (br_if $s (i32.ge_u (local.get $p) (local.get $end)))
          (br_if $s (i32.ne (i32.load8_u (local.get $p)) (i32.const 32)))
          (local.set $p (i32.add (local.get $p) (i32.const 1)))
          (br $space)))
        (block $d (loop $digit
          (br_if $d (i32.ge_u (local.get $p) (local.get $end)))
          (local.set $c (i32.sub (i32.load8_u (local.get $p)) (i32.const 48)))
          (br_if $d (i32.gt_u (local.get $c) (i32.const 9)))
          (local.set $v (i32.add (i32.mul (local.get $v) (i32.const 10)) (local.get $c)))
          (local.set $p (i32.add (local.get $p) (i32.const 1)))
          (br $digit)))
        (if (i32.lt_u (i32.add (local.get $p) (i32.const 1)) (local.get $end))
          (then (if (i32.eq (i32.load8_u (local.get $p)) (i32.const 46))
            (then
              (local.set $c (i32.sub (i32.load8_u (i32.add (local.get $p) (i32.const 1))) (i32.const 48)))
              (if (i32.le_u (local.get $c) (i32.const 9)) (then (local.set $frac (local.get $c))))))))
        (i32.add (i32.mul (local.get $v) (i32.const 10)) (local.get $frac)))
      (func $tenths_text (param $w i32) (param $v i32) (result i32)
        (local.set $w (call $utoa (local.get $w)
          (i64.extend_i32_u (i32.div_u (local.get $v) (i32.const 10)))))
        (i32.store8 (local.get $w) (i32.const 46))
        (i32.store8 (i32.add (local.get $w) (i32.const 1))
          (i32.add (i32.const 48) (i32.rem_u (local.get $v) (i32.const 10))))
        (i32.add (local.get $w) (i32.const 2)))
      (func (export "{ASSERTION}#check") (param $args i32) (result i32)
        (local $p i32) (local $end i32) (local $sum i32) (local $total i32) (local $has i32)
        (local $out i32) (local $w i32)
        (local.set $p (i32.load offset=52 (local.get $args)))
        (local.set $end (i32.add (local.get $p) (i32.load offset=56 (local.get $args))))
        (block $done (loop $scan
          (br_if $done (i32.ge_u (local.get $p) (local.get $end)))
          (if (call $starts (local.get $p) (local.get $end) (i32.const {amount}) (i32.const {amount_len}))
            (then (local.set $sum (i32.add (local.get $sum)
              (call $tenths (i32.add (local.get $p) (i32.const {amount_len})) (local.get $end))))))
          (if (call $starts (local.get $p) (local.get $end) (i32.const {total}) (i32.const {total_len}))
            (then
              (local.set $total
                (call $tenths (i32.add (local.get $p) (i32.const {total_len})) (local.get $end)))
              (local.set $has (i32.const 1))))
          (local.set $p (i32.add (local.get $p) (i32.const 1)))
          (br $scan)))
        (if (i32.eqz (local.get $has))
          (then (return (call $verdict (i32.const 2) (i32.const {no_total}) (i32.const {no_total_len})))))
        (if (i32.eq (local.get $total) (local.get $sum))
          (then (return (call $verdict (i32.const 0) (i32.const 0) (i32.const 0)))))
        (local.set $out (call $alloc (i32.const 64) (i32.const 1)))
        (local.set $w (call $put (local.get $out) (i32.const {total_word}) (i32.const {total_word_len})))
        (local.set $w (call $tenths_text (local.get $w) (local.get $total)))
        (local.set $w (call $put (local.get $w) (i32.const {sum_word}) (i32.const {sum_word_len})))
        (local.set $w (call $tenths_text (local.get $w) (local.get $sum)))
        (call $verdict (i32.const 1) (local.get $out) (i32.sub (local.get $w) (local.get $out))))
    "#,
        a4 = AREA + 4,
        a8 = AREA + 8,
    );
    component("assertion", "", &data, &body)
}

/// A report renderer (`acme`'s `xlsx`) returning [`REPORT_PREFIX`] followed
/// by the report it was given.
pub fn report_renderer() -> Vec<u8> {
    let mut data = Data::default();
    let formats = data.string_list(&["xlsx"]);
    let prefix = std::str::from_utf8(REPORT_PREFIX).expect("the prefix is ASCII");
    let (prefix, prefix_len) = data.string(prefix);
    let body = format!(
        r#"
      {TEXT}
      (func (export "{REPORT}#formats") (result i32) (i32.const {formats}))
      (func (export "{REPORT}#render") (param i32 i32 i32 i32) (result i32)
        (local $out i32) (local $end i32)
        (local.set $out (call $alloc (i32.add (local.get 3) (i32.const {prefix_len})) (i32.const 1)))
        (local.set $end (call $put (local.get $out) (i32.const {prefix}) (i32.const {prefix_len})))
        (drop (call $put (local.get $end) (local.get 2) (local.get 3)))
        (call $ok_string (local.get $out) (i32.add (local.get 3) (i32.const {prefix_len}))))
    "#
    );
    component("report", "", &data, &body)
}
