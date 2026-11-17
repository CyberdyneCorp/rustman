//! ULIDs (design D5): 48 bits of milliseconds since the Unix epoch, then 80
//! random bits, in Crockford base32. They sort by time as text, so listing
//! history is a sorted directory scan.

use std::sync::{Mutex, PoisonError};
use std::time::{SystemTime, UNIX_EPOCH};

const ALPHABET: &[u8; 32] = b"0123456789ABCDEFGHJKMNPQRSTVWXYZ";
const LEN: usize = 26;
const RANDOM_BITS: u32 = 80;

/// A new ULID. Ids made by one process strictly increase, even within one
/// millisecond.
pub fn new_ulid() -> String {
    static LAST: Mutex<u128> = Mutex::new(0);
    let millis = u128::from(now_millis()) & ((1 << 48) - 1);
    let random = rand::random::<u128>() & ((1 << RANDOM_BITS) - 1);
    let mut value = (millis << RANDOM_BITS) | random;
    let mut last = LAST.lock().unwrap_or_else(PoisonError::into_inner);
    if value <= *last {
        value = *last + 1;
    }
    *last = value;
    encode(value)
}

/// The milliseconds since the Unix epoch encoded in `id`, if it is a ULID.
pub fn timestamp_millis(id: &str) -> Option<u64> {
    let value = decode(id)?;
    u64::try_from(value >> RANDOM_BITS).ok()
}

/// Whether `id` is a well-formed ULID.
pub fn is_ulid(id: &str) -> bool {
    decode(id).is_some()
}

/// Milliseconds since the Unix epoch.
pub fn now_millis() -> u64 {
    let elapsed = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX)
}

fn encode(value: u128) -> String {
    (0..LEN)
        .rev()
        .map(|i| char::from(ALPHABET[((value >> (i * 5)) & 31) as usize]))
        .collect()
}

/// The 128-bit value of `id`. 26 base32 digits hold 130 bits, so the first
/// digit of a ULID is at most `7`; a larger one is not a ULID.
fn decode(id: &str) -> Option<u128> {
    if id.len() != LEN || !matches!(id.as_bytes()[0], b'0'..=b'7') {
        return None;
    }
    id.bytes().try_fold(0u128, |acc, byte| {
        let digit = ALPHABET
            .iter()
            .position(|&c| c == byte.to_ascii_uppercase())?;
        Some((acc << 5) | digit as u128)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ulids_increase_and_round_trip_their_time() {
        let before = now_millis();
        let ids: Vec<String> = (0..1000).map(|_| new_ulid()).collect();
        let after = now_millis();
        assert!(ids.windows(2).all(|w| w[0] < w[1]));
        for id in &ids {
            assert_eq!(id.len(), 26);
            assert!(is_ulid(id));
            let millis = timestamp_millis(id).unwrap();
            assert!((before..=after).contains(&millis), "{id}");
        }
    }

    #[test]
    fn malformed_ids_are_rejected() {
        assert!(!is_ulid("short"));
        assert!(!is_ulid("01ARZ3NDEKTSV4RRFFQ69G5FAU"));
        assert!(is_ulid("01ARZ3NDEKTSV4RRFFQ69G5FAV"));
        assert!(is_ulid("7ZZZZZZZZZZZZZZZZZZZZZZZZZ"));
        for overflowing in ["8ZZZZZZZZZZZZZZZZZZZZZZZZZ", "Z1ARZ3NDEKTSV4RRFFQ69G5FAV"] {
            assert!(!is_ulid(overflowing), "{overflowing}");
            assert_eq!(timestamp_millis(overflowing), None);
        }
        assert_eq!(
            timestamp_millis("01ARZ3NDEKTSV4RRFFQ69G5FAV"),
            Some(1_469_922_850_259)
        );
    }
}
