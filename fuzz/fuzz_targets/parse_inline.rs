#![no_main]

use brief::inline::parse_inline;
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let Ok(input) = std::str::from_utf8(data) else {
        return;
    };
    if input.len() > 16 * 1024 {
        return;
    }
    // The block parser feeds parse_inline a single trimmed line. Newlines
    // would never appear in a real call, but the fuzzer is allowed to try.
    let _ = parse_inline(input, 0);
});
