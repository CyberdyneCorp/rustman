//! `Redactor::copy` writes exactly what `Redactor::bytes` gives for the
//! whole input, whatever the chunking: a property test over random secrets
//! (nested, overlapping, self-overlapping), random bodies holding them and
//! random read sizes, from a fixed seed so every run checks the same cases.

use std::io::Read;

use rustman_core::{Redactor, Secret};

/// xorshift64: deterministic, no dependency.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }

    fn text(&mut self, len: usize) -> String {
        const ALPHABET: &[u8] = b"abcx";
        (0..len)
            .map(|_| char::from(ALPHABET[self.below(ALPHABET.len())]))
            .collect()
    }
}

/// Reads at most `max` bytes at a time.
struct Short<'a> {
    data: &'a [u8],
    max: usize,
}

impl Read for Short<'_> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let n = buf.len().min(self.max).min(self.data.len());
        buf[..n].copy_from_slice(&self.data[..n]);
        self.data = &self.data[n..];
        Ok(n)
    }
}

/// Up to four secrets; later ones often wrap or extend the first, so
/// secrets nest and overlap.
fn secrets(rng: &mut Rng) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for k in 0..1 + rng.below(4) {
        let len = 2 + rng.below(9);
        let own = rng.text(len);
        let value = match (k, rng.below(3)) {
            (0, _) | (_, 0) => own,
            (_, 1) => format!("q{}zz", out[0]),
            _ => format!("{}{own}", &out[0][out[0].len() / 2..]),
        };
        out.push(value);
    }
    out
}

/// Raw text outside every placeholder of `out`.
fn unredacted(out: &str) -> Vec<&str> {
    out.split('«')
        .map(|part| part.split_once('»').map_or(part, |(_, rest)| rest))
        .collect()
}

#[test]
fn copy_matches_bytes_for_random_secrets_bodies_and_chunks() {
    let mut rng = Rng(0x9e37_79b9_7f4a_7c15);
    for case in 0..20_000 {
        let values = secrets(&mut rng);
        let mut redactor = Redactor::new();
        for (k, value) in values.iter().enumerate() {
            redactor.add(&Secret::new(format!("s{k}"), value.clone()));
        }
        let len = 1 + rng.below(200);
        let mut body = rng.text(len).into_bytes();
        for value in &values {
            let at = rng.below(body.len() + 1);
            body.splice(at..at, value.bytes());
        }
        let max = 1 + rng.below(40);
        let mut out = Vec::new();
        redactor.copy(Short { data: &body, max }, &mut out).unwrap();
        let whole = redactor.bytes(&body).into_owned();
        let text = String::from_utf8_lossy(&out);
        assert_eq!(
            text,
            String::from_utf8_lossy(&whole),
            "case {case}: secrets {values:?}, reads of {max}, body {}",
            String::from_utf8_lossy(&body)
        );
        for segment in unredacted(&text) {
            assert!(
                values.iter().all(|v| !segment.contains(v.as_str())),
                "case {case}: a secret is visible in {text}"
            );
        }
    }
}

/// The end-to-end report: a secret nested in a longer one (a password in a
/// DSN) whose longer secret starts right before the held-back tail of a
/// 64 KiB chunk.
#[test]
fn a_nested_secret_near_a_chunk_end_does_not_expose_the_longer_one() {
    let redactor = Redactor::new()
        .with(&Secret::new("A", "xyz".to_owned()))
        .with(&Secret::new("B", "wxyz012345".to_owned()));
    for off in 65_500..65_540 {
        let mut body = vec![b'.'; off];
        body.extend(b"wxyz012345");
        body.extend([b'_'; 100]);
        let mut out = Vec::new();
        redactor
            .copy(
                Short {
                    data: &body,
                    max: 65_536,
                },
                &mut out,
            )
            .unwrap();
        assert_eq!(out, redactor.bytes(&body).into_owned(), "offset {off}");
    }
}
