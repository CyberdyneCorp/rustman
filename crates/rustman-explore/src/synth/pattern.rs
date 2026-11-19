//! Strings for a `pattern`: the plain sample when it already matches,
//! otherwise one generated from the regular expression (with proptest's
//! regex generator and a ChaCha stream from the seed), checked against the
//! pattern and the length bounds. JSON Schema patterns are ECMA-262, so
//! `\d`, `\w`, `\s` and `.` are generated from their ASCII meaning.

use proptest::strategy::{Strategy, ValueTree};
use proptest::test_runner::{Config, RngAlgorithm, TestRng, TestRunner};
use regex::Regex;

/// Generated candidates tried before giving up on a pattern.
const ATTEMPTS: usize = 64;

/// How often an open `+` or `*` repeats in the first, short attempts.
const SHORT: &str = "4";

/// What `\d`, `\w` and `\s` stand for inside a character class.
const CLASS_ESCAPES: [(char, &str); 3] = [('d', "0-9"), ('w', "A-Za-z0-9_"), ('s', " ")];

/// A string matching `pattern` with between `min` and `max` characters:
/// `sample` when it does, else a generated one; `sample` when the pattern
/// is not supported or nothing generated fits.
pub fn fit(pattern: &str, sample: String, min: usize, max: Option<usize>, seed: u64) -> String {
    let Ok(regex) = Regex::new(pattern) else {
        return sample;
    };
    let fits = |s: &str| {
        let n = s.chars().count();
        n >= min && max.is_none_or(|m| n <= m) && regex.is_match(s)
    };
    if fits(&sample) {
        return sample;
    }
    let mut runner = runner(seed);
    [true, false]
        .into_iter()
        .find_map(|short| generate(&generator(pattern, short), &mut runner, &fits))
        .unwrap_or(sample)
}

/// The first of [`ATTEMPTS`] strings generated from `regex` that `fits`.
fn generate(regex: &str, runner: &mut TestRunner, fits: &dyn Fn(&str) -> bool) -> Option<String> {
    let strategy = proptest::string::string_regex(regex).ok()?;
    (0..ATTEMPTS)
        .filter_map(|_| strategy.new_tree(runner).ok())
        .map(|tree| tree.current())
        .find(|s| fits(s))
}

/// The regex to generate from: `pattern` without its anchors, with ASCII
/// classes, and with `+` and `*` repeating at most [`SHORT`] times when
/// `short` (so strings stay small and fit a `maxLength`).
fn generator(pattern: &str, short: bool) -> String {
    let ascii = ascii_classes(pattern, short);
    ascii
        .trim_start_matches('^')
        .trim_end_matches('$')
        .to_owned()
}

fn runner(seed: u64) -> TestRunner {
    let mut bytes = [0u8; 32];
    bytes[..8].copy_from_slice(&seed.to_le_bytes());
    let rng = TestRng::from_seed(RngAlgorithm::ChaCha, &bytes);
    TestRunner::new_with_rng(Config::default(), rng)
}

/// `pattern` with `\d`, `\w`, `\s` and `.` spelled as ASCII classes, so the
/// generator does not pick characters only Unicode-aware engines accept;
/// with `short`, open `+` and `*` are bounded to [`SHORT`] repeats.
fn ascii_classes(pattern: &str, short: bool) -> String {
    let mut out = String::with_capacity(pattern.len());
    let mut in_class = false;
    let mut chars = pattern.chars();
    while let Some(c) = chars.next() {
        match c {
            '\\' => escape(chars.next(), in_class, &mut out),
            '[' if !in_class => {
                in_class = true;
                out.push(c);
            }
            ']' if in_class => {
                in_class = false;
                out.push(c);
            }
            '.' if !in_class => out.push_str("[ -~]"),
            '+' if short && !in_class => out.push_str(&format!("{{1,{SHORT}}}")),
            '*' if short && !in_class => out.push_str(&format!("{{0,{SHORT}}}")),
            _ => out.push(c),
        }
    }
    out
}

fn escape(next: Option<char>, in_class: bool, out: &mut String) {
    let Some(next) = next else {
        out.push('\\');
        return;
    };
    match CLASS_ESCAPES.iter().find(|(e, _)| *e == next) {
        Some((_, class)) if in_class => out.push_str(class),
        Some((_, class)) => out.push_str(&format!("[{class}]")),
        None => {
            out.push('\\');
            out.push(next);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_matching_sample_is_kept() {
        assert_eq!(fit("^[a-z]+$", "string".into(), 0, None, 0), "string");
    }

    #[test]
    fn generated_strings_match_the_pattern_and_bounds() {
        let codes = Regex::new("^[A-Z]{3}-[0-9]{4}$").unwrap();
        for seed in 0..8 {
            let code = fit("^[A-Z]{3}-[0-9]{4}$", "string".into(), 0, None, seed);
            assert!(codes.is_match(&code), "{code}");
            let nested = fit("^(a+)+$", "string".into(), 2, Some(5), seed);
            assert!(nested.chars().all(|c| c == 'a'), "{nested}");
            assert!((2..=5).contains(&nested.len()), "{nested}");
            let digits = fit(r"^\d{3}\.\w$", "string".into(), 0, None, seed);
            assert!(digits.is_ascii() && digits.len() == 5, "{digits}");
        }
    }

    #[test]
    fn the_same_seed_gives_the_same_string() {
        let a = fit("^[a-f0-9]{12}$", String::new(), 0, None, 7);
        assert_eq!(a, fit("^[a-f0-9]{12}$", String::new(), 0, None, 7));
    }

    #[test]
    fn unsupported_or_impossible_patterns_keep_the_sample() {
        assert_eq!(fit("(?<=a)b", "string".into(), 0, None, 0), "string");
        assert_eq!(fit("^a{10}$", "string".into(), 0, Some(3), 0), "string");
    }

    #[test]
    fn classes_are_spelled_in_ascii() {
        assert_eq!(
            ascii_classes(r"\d.[\w.+]\\d", false),
            r"[0-9][ -~][A-Za-z0-9_.+]\\d"
        );
        assert_eq!(ascii_classes(r"a+b*\+", true), r"a{1,4}b{0,4}\+");
    }
}
