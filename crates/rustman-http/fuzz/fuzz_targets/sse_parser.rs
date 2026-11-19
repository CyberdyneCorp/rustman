#![no_main]

use libfuzzer_sys::fuzz_target;
use rustman_http::{SseParser, SseTooLarge};

const LIMIT: u64 = 4096;

/// Every event of `data` fed in pieces of at most `step` bytes, and the
/// parser's final state; `None` once the size limit is passed.
fn parse(data: &[u8], step: usize) -> Option<(Vec<rustman_http::SseEvent>, String, Option<u64>)> {
    let mut parser = SseParser::new(LIMIT);
    let mut events = Vec::new();
    for chunk in data.chunks(step.max(1)) {
        match parser.push(chunk) {
            Ok(more) => events.extend(more),
            Err(SseTooLarge { limit, size }) => {
                assert!(size > limit);
                return None;
            }
        }
    }
    Some((events, parser.last_event_id().to_owned(), parser.retry()))
}

// Parsing never panics, whatever the bytes, and how the stream is split
// into chunks never changes whether the size limit is passed, nor the
// events, the last event id or the retry.
fuzz_target!(|data: &[u8]| {
    let Some((&step, stream)) = data.split_first() else {
        return;
    };
    let whole = parse(stream, stream.len());
    let split = parse(stream, usize::from(step % 16) + 1);
    assert_eq!(whole, split);
});
