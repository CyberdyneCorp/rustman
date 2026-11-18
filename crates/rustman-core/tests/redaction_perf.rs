//! Redaction cost stays bounded (benchmark-style, with generous bounds for
//! unoptimized test builds): many long secrets, a run that shares its
//! redactor across requests, a periodic body full of overlapping matches,
//! and a stream whose chunks split self-overlapping secrets.

use std::io::Read;
use std::time::{Duration, Instant};

use rustman_core::{Redactor, Secret};

/// The bound for each benchmark in a debug build; release builds run them
/// in a small fraction of it.
const BOUND: Duration = Duration::from_secs(30);

/// `n` secrets of `len` characters, with the characters every encoder
/// family escapes, so each has its full set of forms.
fn secrets(n: usize, len: usize) -> Vec<Secret<String>> {
    const CHARS: [char; 16] = [
        'a', 'b', '/', '&', 'é', '"', '<', ' ', '~', 'Q', '9', 'x', '.', '-', '=', '+',
    ];
    (0..n)
        .map(|j| {
            let value: String = (0..len)
                .map(|i| {
                    let x = ((i + j * 1_000_003) as u64)
                        .wrapping_mul(6_364_136_223_846_793_005)
                        .rotate_left(17);
                    CHARS[(x % 16) as usize]
                })
                .collect();
            Secret::new(format!("s{j}"), value)
        })
        .collect()
}

#[test]
fn a_run_with_fifty_long_secrets_stays_fast() {
    let started = Instant::now();
    let secrets = secrets(50, 1024);
    let mut run = Redactor::new();
    for secret in &secrets {
        run.add(secret);
    }
    let body: Vec<u8> = (0..1_000_000u32)
        .map(|i| b"abQ9x/&\" .-=+\n"[(i % 14) as usize])
        .collect();
    let mut echo = body.clone();
    echo.extend_from_slice(secrets[49].expose().as_bytes());
    for _ in 0..20 {
        // What each request does: start from the run's redactor, add the
        // secrets it resolved (already known), redact its exchange.
        let mut request = run.clone();
        for secret in &secrets {
            request.add(secret);
        }
        run.add(&secrets[0]);
        assert_eq!(request.bytes(&body).len(), body.len());
        assert!(
            !request
                .text(&String::from_utf8_lossy(&echo))
                .contains(secrets[49].expose())
        );
    }
    let elapsed = started.elapsed();
    assert!(elapsed < BOUND, "{elapsed:?}");
}

#[test]
fn nested_secrets_over_a_periodic_body_stay_fast() {
    let started = Instant::now();
    let mut nested = Redactor::new();
    for i in 1..=50 {
        nested.add(&Secret::new(format!("a{i}"), "a".repeat(i * 4)));
    }
    let body = vec![b'a'; 200_000];
    let out = nested.bytes(&body);
    assert_eq!(&out[..], "«redacted:a50»".as_bytes());
    let mut copied = Vec::new();
    nested.copy(&body[..], &mut copied).unwrap();
    assert_eq!(copied, out.into_owned());
    let elapsed = started.elapsed();
    assert!(elapsed < BOUND, "{elapsed:?}");
}

#[test]
fn a_long_periodic_secret_builds_fast() {
    let started = Instant::now();
    let value: String = (0..4096)
        .map(|i| ['a', '/', '&', 'é', '"', '<', ' ', '~'][i % 8])
        .collect();
    let r = Redactor::new().with(&Secret::new("big", value.clone()));
    assert!(r.reveals_secret(&value[..64]));
    assert!(r.finds_secret(&format!("x{value}x")));
    let elapsed = started.elapsed();
    assert!(elapsed < BOUND, "{elapsed:?}");
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

/// A self-overlapping secret repeated across a chunk boundary never leaves
/// a fragment (`«redacted:p»cabc`): the stream matches `bytes()`.
#[test]
fn self_overlapping_secrets_across_chunks_are_redacted_whole() {
    let r = Redactor::new().with(&Secret::new("p", "abcabcab".to_owned()));
    for off in (65_400..65_540).step_by(3) {
        for len in [3, 8, 9, 21, 40] {
            let mut data = vec![b'x'; off];
            data.extend("abc".repeat(len).as_bytes());
            data.extend(b"zzzz");
            for max in [65_536, 4_096, 7] {
                let mut out = Vec::new();
                r.copy(Short { data: &data, max }, &mut out).unwrap();
                assert_eq!(
                    out,
                    r.bytes(&data).into_owned(),
                    "off {off} len {len} max {max}"
                );
            }
        }
    }
}
