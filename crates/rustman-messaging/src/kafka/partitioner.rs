//! Partition choice for Kafka produce (design D1): an explicit
//! `partition`, else murmur2 of the key exactly as the Java client's
//! default partitioner computes it, else round robin for keyless
//! messages.

/// The Java client's `Utils.murmur2`.
pub fn murmur2(data: &[u8]) -> i32 {
    const SEED: u32 = 0x9747_b28c;
    const M: u32 = 0x5bd1_e995;
    const R: u32 = 24;
    let length = data.len();
    // The Java client hashes `int` lengths; keys never approach 2 GiB.
    let mut h = SEED ^ (length as u32);
    let mut chunks = data.chunks_exact(4);
    for chunk in &mut chunks {
        let mut k = u32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]);
        k = k.wrapping_mul(M);
        k ^= k >> R;
        k = k.wrapping_mul(M);
        h = h.wrapping_mul(M);
        h ^= k;
    }
    let tail = chunks.remainder();
    if !tail.is_empty() {
        for (i, byte) in tail.iter().enumerate().rev() {
            h ^= u32::from(*byte) << (8 * i);
        }
        h = h.wrapping_mul(M);
    }
    h ^= h >> 13;
    h = h.wrapping_mul(M);
    h ^= h >> 15;
    h as i32
}

/// The partition (an index into `0..partitions`) the Java client's
/// default partitioner picks for `key`.
pub fn partition_for_key(key: &[u8], partitions: usize) -> usize {
    let positive = (murmur2(key) & 0x7fff_ffff) as usize;
    positive % partitions.max(1)
}

/// Picks a partition per message.
#[derive(Debug)]
pub struct Partitioner {
    /// The topic's partition ids, ascending.
    partitions: Vec<i32>,
    explicit: Option<i32>,
    next: usize,
}

impl Partitioner {
    pub fn new(mut partitions: Vec<i32>, explicit: Option<i32>) -> Self {
        partitions.sort_unstable();
        Self {
            partitions,
            explicit,
            next: 0,
        }
    }

    /// The partition for a message with `key`.
    pub fn choose(&mut self, key: Option<&[u8]>) -> i32 {
        if let Some(partition) = self.explicit {
            return partition;
        }
        let count = self.partitions.len().max(1);
        let index = match key {
            Some(key) => partition_for_key(key, count),
            None => {
                let index = self.next % count;
                self.next = self.next.wrapping_add(1);
                index
            }
        };
        self.partitions.get(index).copied().unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `UtilsTest.testMurmur2` of the Apache Kafka Java client.
    #[test]
    fn murmur2_matches_the_java_client_reference_vectors() {
        let cases: [(&[u8], i32); 6] = [
            (b"21", -973_932_308),
            (b"foobar", -790_332_482),
            (b"a-little-bit-long-string", -985_981_536),
            (b"a-little-bit-longer-string", -1_486_304_829),
            (
                b"lkjh234lh9fiuh90y23oiuhsafujhadof229phr9h19h89h8",
                -58_897_971,
            ),
            (b"abc", 479_470_107),
        ];
        for (key, expected) in cases {
            assert_eq!(murmur2(key), expected, "{}", String::from_utf8_lossy(key));
        }
    }

    #[test]
    fn keyed_messages_land_where_the_java_client_puts_them() {
        // toPositive(murmur2("foobar")) % 3 = (-790332482 & 0x7fffffff) % 3.
        let expected = ((-790_332_482i32 & 0x7fff_ffff) as usize) % 3;
        assert_eq!(partition_for_key(b"foobar", 3), expected);
        let mut partitioner = Partitioner::new(vec![2, 0, 1], None);
        assert_eq!(partitioner.choose(Some(b"foobar")), expected as i32);
        assert_eq!(partitioner.choose(Some(b"foobar")), expected as i32);
    }

    #[test]
    fn keyless_messages_go_round_robin_and_explicit_partitions_win() {
        let mut partitioner = Partitioner::new(vec![0, 1, 2], None);
        let chosen: Vec<i32> = (0..4).map(|_| partitioner.choose(None)).collect();
        assert_eq!(chosen, [0, 1, 2, 0]);
        let mut explicit = Partitioner::new(vec![0, 1, 2], Some(1));
        assert_eq!(explicit.choose(Some(b"foobar")), 1);
        assert_eq!(explicit.choose(None), 1);
    }
}
