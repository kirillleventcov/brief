#![no_main]

use brief::inline::parse_args;
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let Ok(tail) = std::str::from_utf8(data) else {
        return;
    };
    if tail.len() > 16 * 1024 {
        return;
    }
    // parse_args expects the cursor to point at `(`. Wrap raw fuzzer bytes
    // so we exercise the argument grammar rather than the dispatcher.
    let mut s = String::with_capacity(tail.len() + 1);
    s.push('(');
    s.push_str(tail);
    let mut cursor = 0usize;
    let _ = parse_args(&s, &mut cursor);
});
