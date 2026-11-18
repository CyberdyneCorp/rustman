#![no_main]

use libfuzzer_sys::fuzz_target;
use rustman_viz_protocol::{LineDecoder, decode_line, encode_line};

// Decoding never panics, whatever the bytes; anything that decodes to a
// known message encodes again and decodes to the same message.
fuzz_target!(|data: &[u8]| {
    let mut decoder = LineDecoder::new();
    for message in decoder.push(data).into_iter().flatten() {
        if let Ok(line) = encode_line(&message) {
            assert_eq!(decode_line(&line).ok(), Some(message));
        }
    }
    if let Ok(text) = std::str::from_utf8(data) {
        let _ = decode_line(text);
    }
});
