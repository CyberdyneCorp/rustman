//! Trace and span ids (W3C Trace Context, design D3).

use std::fmt;

use rand::Rng;

/// A 16-byte trace id, never all zeros.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct TraceId([u8; 16]);

/// An 8-byte span id, never all zeros.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct SpanId([u8; 8]);

/// The ids an exchange carries, and whether it is sampled.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TraceContext {
    pub trace_id: TraceId,
    pub span_id: SpanId,
    pub sampled: bool,
}

impl TraceId {
    /// A random id from `rng`, drawn again in the all-zeros case.
    pub fn random(rng: &mut impl Rng) -> Self {
        Self(random_non_zero(rng))
    }

    /// Parse 32 lowercase hex digits; all zeros is invalid.
    pub fn from_hex(hex: &str) -> Option<Self> {
        decode_non_zero(hex).map(Self)
    }

    /// Parse a B3 trace id: 32 or 16 hex digits (a 64-bit id is padded on
    /// the left with zeros). Upper case is accepted, as B3 tracers vary.
    pub fn from_b3(hex: &str) -> Option<Self> {
        let lower = hex.to_ascii_lowercase();
        match lower.len() {
            16 => Self::from_hex(&format!("{:0>32}", lower)),
            _ => Self::from_hex(&lower),
        }
    }

    pub fn bytes(&self) -> [u8; 16] {
        self.0
    }

    /// The lower 64 bits, as Datadog shows a trace id (`{trace_id_dd}`).
    pub fn low_u64(&self) -> u64 {
        let mut low = [0u8; 8];
        low.copy_from_slice(&self.0[8..]);
        u64::from_be_bytes(low)
    }
}

impl SpanId {
    /// A random id from `rng`, drawn again in the all-zeros case.
    pub fn random(rng: &mut impl Rng) -> Self {
        Self(random_non_zero(rng))
    }

    /// Parse 16 lowercase hex digits; all zeros is invalid.
    pub fn from_hex(hex: &str) -> Option<Self> {
        decode_non_zero(hex).map(Self)
    }

    pub fn bytes(&self) -> [u8; 8] {
        self.0
    }
}

impl TraceContext {
    /// New random ids under `trace_id` (a run-scoped trace), else under a
    /// new random trace id.
    pub fn generate(rng: &mut impl Rng, trace_id: Option<TraceId>, sampled: bool) -> Self {
        let trace_id = trace_id.unwrap_or_else(|| TraceId::random(rng));
        Self {
            trace_id,
            span_id: SpanId::random(rng),
            sampled,
        }
    }
}

fn random_non_zero<const N: usize>(rng: &mut impl Rng) -> [u8; N] {
    loop {
        let mut bytes = [0u8; N];
        rng.fill_bytes(&mut bytes);
        if bytes.iter().any(|b| *b != 0) {
            return bytes;
        }
    }
}

fn decode_non_zero<const N: usize>(hex: &str) -> Option<[u8; N]> {
    if hex.len() != N * 2 || !is_lower_hex(hex) {
        return None;
    }
    let mut bytes = [0u8; N];
    for (i, byte) in bytes.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&hex[i * 2..i * 2 + 2], 16).ok()?;
    }
    bytes.iter().any(|b| *b != 0).then_some(bytes)
}

pub(super) fn is_lower_hex(text: &str) -> bool {
    text.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

fn write_hex(f: &mut fmt::Formatter<'_>, bytes: &[u8]) -> fmt::Result {
    bytes.iter().try_for_each(|b| write!(f, "{b:02x}"))
}

impl fmt::Display for TraceId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write_hex(f, &self.0)
    }
}

impl fmt::Debug for TraceId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "TraceId({self})")
    }
}

impl fmt::Display for SpanId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write_hex(f, &self.0)
    }
}

impl fmt::Debug for SpanId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "SpanId({self})")
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use std::convert::Infallible;

    use rand::rngs::StdRng;
    use rand::{SeedableRng, TryRng};

    use super::*;

    /// Yields `zeros` all-zero bytes, then 0xab forever.
    pub(crate) struct ZerosFirst {
        pub zeros: usize,
    }

    impl TryRng for ZerosFirst {
        type Error = Infallible;

        fn try_next_u32(&mut self) -> Result<u32, Infallible> {
            let mut b = [0u8; 4];
            self.try_fill_bytes(&mut b)?;
            Ok(u32::from_le_bytes(b))
        }

        fn try_next_u64(&mut self) -> Result<u64, Infallible> {
            let mut b = [0u8; 8];
            self.try_fill_bytes(&mut b)?;
            Ok(u64::from_le_bytes(b))
        }

        fn try_fill_bytes(&mut self, dst: &mut [u8]) -> Result<(), Infallible> {
            for byte in dst {
                *byte = if self.zeros > 0 { 0 } else { 0xab };
                self.zeros = self.zeros.saturating_sub(1);
            }
            Ok(())
        }
    }

    #[test]
    fn ids_are_never_all_zeros() {
        let mut rng = ZerosFirst { zeros: 16 };
        let trace = TraceId::random(&mut rng);
        assert_eq!(trace.to_string(), "ab".repeat(16));
        let mut rng = ZerosFirst { zeros: 8 };
        assert_eq!(SpanId::random(&mut rng).to_string(), "ab".repeat(8));
        let mut seeded = StdRng::seed_from_u64(1);
        for _ in 0..1000 {
            let ctx = TraceContext::generate(&mut seeded, None, true);
            assert_ne!(ctx.trace_id.bytes(), [0; 16]);
            assert_ne!(ctx.span_id.bytes(), [0; 8]);
        }
    }

    #[test]
    fn a_seeded_generator_is_reproducible() {
        let a = TraceContext::generate(&mut StdRng::seed_from_u64(7), None, true);
        let b = TraceContext::generate(&mut StdRng::seed_from_u64(7), None, true);
        assert_eq!(a, b);
    }

    #[test]
    fn a_run_trace_id_is_kept_with_a_new_span() {
        let mut rng = StdRng::seed_from_u64(3);
        let run = TraceId::random(&mut rng);
        let a = TraceContext::generate(&mut rng, Some(run), false);
        let b = TraceContext::generate(&mut rng, Some(run), false);
        assert_eq!((a.trace_id, b.trace_id), (run, run));
        assert_ne!(a.span_id, b.span_id);
        assert!(!a.sampled);
    }

    #[test]
    fn hex_ids_round_trip_and_reject_zeros_and_case() {
        let id = "4bf92f3577b34da6a3ce929d0e0e4736";
        assert_eq!(TraceId::from_hex(id).unwrap().to_string(), id);
        assert!(TraceId::from_hex(&"0".repeat(32)).is_none());
        assert!(TraceId::from_hex(&id.to_uppercase()).is_none());
        assert!(TraceId::from_hex(&id[1..]).is_none());
        assert!(SpanId::from_hex("00f067aa0ba902b7").is_some());
        assert!(SpanId::from_hex(&"0".repeat(16)).is_none());
        assert!(SpanId::from_hex("00f067aa0ba902bz").is_none());
    }

    #[test]
    fn b3_trace_ids_may_be_64_bit_or_upper_case() {
        let short = TraceId::from_b3("A3CE929D0E0E4736").unwrap();
        assert_eq!(short.to_string(), "0000000000000000a3ce929d0e0e4736");
        assert!(TraceId::from_b3("4bf92f3577b34da6a3ce929d0e0e4736").is_some());
        assert!(TraceId::from_b3("123").is_none());
    }
}
