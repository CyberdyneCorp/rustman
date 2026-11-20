//! The SOPS document format (design D5): `ENC[AES256_GCM,...]` values,
//! each encrypted with the file's data key and its key path as additional
//! data, and a MAC over every value checked before anything is returned.

use aes_gcm::aead::consts::U32;
use aes_gcm::aead::{Aead, KeyInit, Payload};
use aes_gcm::aes::Aes256;
use aes_gcm::{AesGcm, Nonce};
use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use serde_json::{Map, Number, Value};
use sha2::{Digest, Sha512};
use zeroize::Zeroizing;

/// AES-256-GCM with SOPS' 32-byte IVs.
type Cipher = AesGcm<Aes256, U32>;

/// The data key of a SOPS file.
pub type DataKey = Zeroizing<Vec<u8>>;

/// Why a document could not be decrypted. Never holds a value.
#[derive(Debug, PartialEq, Eq)]
pub enum TreeError {
    Malformed(String),
    /// A value or the MAC did not authenticate with the data key.
    Decrypt,
    MacMismatch,
}

/// One encrypted value: `ENC[AES256_GCM,data:...,iv:...,tag:...,type:...]`.
struct Encrypted {
    data: Vec<u8>,
    iv: Vec<u8>,
    tag: Vec<u8>,
    kind: String,
}

fn parse_enc(text: &str) -> Option<Encrypted> {
    let inner = text.strip_prefix("ENC[AES256_GCM,")?.strip_suffix(']')?;
    let mut parts = std::collections::HashMap::new();
    for part in inner.split(',') {
        let (key, value) = part.split_once(':')?;
        parts.insert(key, value);
    }
    let decode = |key: &str| STANDARD.decode(parts.get(key)?).ok();
    Some(Encrypted {
        data: decode("data")?,
        iv: decode("iv")?,
        tag: decode("tag")?,
        kind: (*parts.get("type")?).to_owned(),
    })
}

/// Decrypt `enc` with `key`, authenticating `aad`.
fn decrypt(enc: &Encrypted, key: &[u8], aad: &str) -> Result<Zeroizing<Vec<u8>>, TreeError> {
    let cipher = Cipher::new_from_slice(key).map_err(|_| TreeError::Decrypt)?;
    let nonce = Nonce::<U32>::try_from(enc.iv.as_slice()).map_err(|_| TreeError::Decrypt)?;
    let mut message = enc.data.clone();
    message.extend_from_slice(&enc.tag);
    let payload = Payload {
        msg: &message,
        aad: aad.as_bytes(),
    };
    cipher
        .decrypt(&nonce, payload)
        .map(Zeroizing::new)
        .map_err(|_| TreeError::Decrypt)
}

/// The typed value of decrypted `plain`.
fn typed(plain: &str, kind: &str) -> Result<Value, TreeError> {
    let bad = || TreeError::Malformed(format!("invalid {kind} value"));
    Ok(match kind {
        "str" | "bytes" => Value::String(plain.to_owned()),
        "int" => Value::Number(plain.parse::<i64>().map_err(|_| bad())?.into()),
        "float" => {
            let f = plain.parse::<f64>().map_err(|_| bad())?;
            Value::Number(Number::from_f64(f).ok_or_else(bad)?)
        }
        "bool" => Value::Bool(plain.eq_ignore_ascii_case("true")),
        _ => return Err(TreeError::Malformed(format!("unknown value type '{kind}'"))),
    })
}

/// SOPS' text form of a value for the MAC (`ToBytes`).
fn mac_bytes(value: &Value) -> String {
    match value {
        Value::String(s) => s.clone(),
        Value::Bool(true) => "True".to_owned(),
        Value::Bool(false) => "False".to_owned(),
        Value::Number(n) => n.to_string(),
        _ => String::new(),
    }
}

/// Walks a document, decrypting values and hashing them for the MAC.
struct Walker<'k> {
    key: &'k [u8],
    hash: Sha512,
    mac_only_encrypted: bool,
}

impl Walker<'_> {
    fn value(&mut self, value: &Value, path: &mut Vec<String>) -> Result<Value, TreeError> {
        match value {
            Value::Object(map) => self.map(map, path).map(Value::Object),
            Value::Array(items) => items
                .iter()
                .map(|item| self.value(item, path))
                .collect::<Result<_, _>>()
                .map(Value::Array),
            Value::String(text) if text.starts_with("ENC[") => self.encrypted(text, path),
            leaf => {
                if !self.mac_only_encrypted {
                    self.hash.update(mac_bytes(leaf).as_bytes());
                }
                Ok(leaf.clone())
            }
        }
    }

    fn map(
        &mut self,
        map: &Map<String, Value>,
        path: &mut Vec<String>,
    ) -> Result<Map<String, Value>, TreeError> {
        let mut out = Map::new();
        for (key, value) in map {
            path.push(key.clone());
            let decrypted = self.value(value, path);
            path.pop();
            out.insert(key.clone(), decrypted?);
        }
        Ok(out)
    }

    fn encrypted(&mut self, text: &str, path: &[String]) -> Result<Value, TreeError> {
        let enc =
            parse_enc(text).ok_or_else(|| TreeError::Malformed("invalid ENC value".into()))?;
        let aad = format!("{}:", path.join(":"));
        let plain = decrypt(&enc, self.key, &aad)?;
        let plain = std::str::from_utf8(&plain).map_err(|_| TreeError::Decrypt)?;
        let value = typed(plain, &enc.kind)?;
        self.hash.update(mac_bytes(&value).as_bytes());
        Ok(value)
    }
}

/// The decrypted `document` (without its `sops` metadata), once its MAC
/// matches. `sops` is the metadata mapping.
pub fn decrypt_document(
    document: &Map<String, Value>,
    sops: &Value,
    key: &[u8],
) -> Result<Value, TreeError> {
    let mut walker = Walker {
        key,
        hash: Sha512::new(),
        mac_only_encrypted: sops.get("mac_only_encrypted").and_then(Value::as_bool) == Some(true),
    };
    let mut body = document.clone();
    body.remove("sops");
    let decrypted = walker.map(&body, &mut Vec::new())?;
    let computed: String = walker
        .hash
        .finalize()
        .iter()
        .map(|b| format!("{b:02X}"))
        .collect();
    let mac = sops
        .get("mac")
        .and_then(Value::as_str)
        .ok_or(TreeError::MacMismatch)?;
    let last_modified = sops
        .get("lastmodified")
        .and_then(Value::as_str)
        .ok_or_else(|| TreeError::Malformed("missing lastmodified".into()))?;
    let enc = parse_enc(mac).ok_or(TreeError::MacMismatch)?;
    let stored = decrypt(&enc, key, last_modified)?;
    if stored.as_slice() != computed.as_bytes() {
        return Err(TreeError::MacMismatch);
    }
    Ok(Value::Object(decrypted))
}

/// Encryption, the inverse of the functions above, for test fixtures and
/// for the round trip in this module's tests.
#[doc(hidden)]
pub mod encrypt {
    use super::*;

    /// `ENC[...]` of `plain` with `key`, `iv` and `aad`.
    pub fn value(plain: &str, kind: &str, key: &[u8], iv: &[u8; 32], aad: &str) -> String {
        let cipher = Cipher::new_from_slice(key).expect("a 32-byte key");
        let payload = Payload {
            msg: plain.as_bytes(),
            aad: aad.as_bytes(),
        };
        let sealed = cipher
            .encrypt(&Nonce::<U32>::from(*iv), payload)
            .expect("AES-GCM encryption");
        let (data, tag) = sealed.split_at(sealed.len() - 16);
        format!(
            "ENC[AES256_GCM,data:{},iv:{},tag:{},type:{kind}]",
            STANDARD.encode(data),
            STANDARD.encode(iv),
            STANDARD.encode(tag)
        )
    }

    /// `document` with every leaf encrypted as SOPS does, and the encrypted
    /// MAC for `last_modified`. `iv` is varied per value from `seed`.
    pub fn document(document: &Value, key: &[u8], last_modified: &str) -> (Value, String) {
        let mut hash = Sha512::new();
        let mut counter = 0u8;
        let encrypted = walk(document, key, &mut Vec::new(), &mut hash, &mut counter);
        let mac: String = hash.finalize().iter().map(|b| format!("{b:02X}")).collect();
        let mac = value(&mac, "str", key, &[0xAA; 32], last_modified);
        (encrypted, mac)
    }

    fn walk(
        v: &Value,
        key: &[u8],
        path: &mut Vec<String>,
        hash: &mut Sha512,
        counter: &mut u8,
    ) -> Value {
        match v {
            Value::Object(map) => Value::Object(
                map.iter()
                    .map(|(k, item)| {
                        path.push(k.clone());
                        let out = walk(item, key, path, hash, counter);
                        path.pop();
                        (k.clone(), out)
                    })
                    .collect(),
            ),
            Value::Array(items) => Value::Array(
                items
                    .iter()
                    .map(|i| walk(i, key, path, hash, counter))
                    .collect(),
            ),
            leaf => {
                let kind = match leaf {
                    Value::Bool(_) => "bool",
                    Value::Number(n) if n.is_i64() => "int",
                    Value::Number(_) => "float",
                    _ => "str",
                };
                let plain = mac_bytes(leaf);
                hash.update(plain.as_bytes());
                *counter = counter.wrapping_add(1);
                let aad = format!("{}:", path.join(":"));
                Value::String(value(&plain, kind, key, &[*counter; 32], &aad))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    const KEY: [u8; 32] = [7; 32];
    const MODIFIED: &str = "2026-01-02T03:04:05Z";

    fn sealed(doc: &Value) -> (Map<String, Value>, Value) {
        let (encrypted, mac) = encrypt::document(doc, &KEY, MODIFIED);
        let sops = json!({"mac": mac, "lastmodified": MODIFIED});
        (encrypted.as_object().unwrap().clone(), sops)
    }

    #[test]
    fn documents_round_trip_with_types() {
        let doc = json!({"db": {"password": "p@ss", "port": 5432, "ratio": 0.5, "tls": true},
                         "hosts": ["a", "b"]});
        let (encrypted, sops) = sealed(&doc);
        assert!(
            encrypted["db"]["password"]
                .as_str()
                .unwrap()
                .starts_with("ENC[AES256_GCM,")
        );
        assert_eq!(decrypt_document(&encrypted, &sops, &KEY).unwrap(), doc);
    }

    #[test]
    fn tampering_and_wrong_keys_are_detected() {
        let doc = json!({"a": "1", "b": "2"});
        let (mut encrypted, sops) = sealed(&doc);
        assert_eq!(
            decrypt_document(&encrypted, &sops, &[8; 32]),
            Err(TreeError::Decrypt)
        );
        // Swapping two values breaks their additional data.
        let a = encrypted["a"].clone();
        encrypted.insert("a".into(), encrypted["b"].clone());
        encrypted.insert("b".into(), a);
        assert_eq!(
            decrypt_document(&encrypted, &sops, &KEY),
            Err(TreeError::Decrypt)
        );
        // A removed value changes the MAC.
        let (mut encrypted, sops) = sealed(&doc);
        encrypted.remove("b");
        assert_eq!(
            decrypt_document(&encrypted, &sops, &KEY),
            Err(TreeError::MacMismatch)
        );
        // A plain value added later changes it too.
        let (mut encrypted, sops) = sealed(&doc);
        encrypted.insert("c_unencrypted".into(), json!("x"));
        assert_eq!(
            decrypt_document(&encrypted, &sops, &KEY),
            Err(TreeError::MacMismatch)
        );
    }
}
