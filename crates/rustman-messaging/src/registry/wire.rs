//! The Confluent wire format: magic byte 0, a big-endian 4-byte schema
//! id, then the payload (for Protobuf, preceded by the message indexes).

/// The schema id and payload of `bytes`, when they are in the wire format.
pub fn split(bytes: &[u8]) -> Option<(u32, &[u8])> {
    match bytes {
        [0, a, b, c, d, rest @ ..] => Some((u32::from_be_bytes([*a, *b, *c, *d]), rest)),
        _ => None,
    }
}

/// The wire-format header for schema `id`.
pub fn header(id: u32) -> Vec<u8> {
    let mut bytes = vec![0];
    bytes.extend_from_slice(&id.to_be_bytes());
    bytes
}

/// Read the Protobuf message-index path that follows the header: a
/// zig-zag varint count, then that many indexes; a count of 0 means the
/// first message (`[0]`). Returns the path and the remaining bytes.
pub fn message_indexes(bytes: &[u8]) -> Option<(Vec<usize>, &[u8])> {
    let (count, mut rest) = zigzag(bytes)?;
    if count == 0 {
        return Some((vec![0], rest));
    }
    let count = usize::try_from(count).ok()?;
    let mut path = Vec::with_capacity(count.min(16));
    for _ in 0..count {
        let (index, tail) = zigzag(rest)?;
        path.push(usize::try_from(index).ok()?);
        rest = tail;
    }
    Some((path, rest))
}

/// One zig-zag encoded varint.
fn zigzag(bytes: &[u8]) -> Option<(i64, &[u8])> {
    let mut value: u64 = 0;
    for (i, byte) in bytes.iter().enumerate().take(10) {
        value |= u64::from(byte & 0x7f) << (7 * i);
        if byte & 0x80 == 0 {
            let decoded = ((value >> 1) as i64) ^ -((value & 1) as i64);
            return Some((decoded, &bytes[i + 1..]));
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_header_carries_the_schema_id() {
        let mut bytes = header(7);
        assert_eq!(bytes, [0, 0, 0, 0, 7]);
        bytes.extend_from_slice(b"rest");
        assert_eq!(split(&bytes), Some((7, &b"rest"[..])));
        assert_eq!(split(b"{\"id\":1}"), None);
        assert_eq!(split(&[0, 0, 1]), None);
    }

    #[test]
    fn message_indexes_read_the_shorthand_and_full_paths() {
        assert_eq!(message_indexes(&[0, 9]), Some((vec![0], &[9u8][..])));
        // count 2 (zig-zag 4), indexes 1 (2) and 0 (0).
        assert_eq!(
            message_indexes(&[4, 2, 0, 9]),
            Some((vec![1, 0], &[9u8][..]))
        );
        assert_eq!(message_indexes(&[0x80]), None);
    }
}
