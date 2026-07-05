//! Micro-benchmark for `parse_inline` on single-line inputs.
//! Usage: cargo run --release -p brief-core --example inlbench

use std::hint::black_box;
use std::time::Instant;

fn bench(name: &str, line: &str) {
    const N: usize = 200_000;
    // warmup
    for _ in 0..1000 {
        black_box(brief::inline::parse_inline(black_box(line), 0));
    }
    let t = Instant::now();
    for _ in 0..N {
        black_box(brief::inline::parse_inline(black_box(line), 0));
    }
    let per = t.elapsed().as_secs_f64() / N as f64;
    println!(
        "{:<24} {:>8.1} ns/line   {:>7.1} MiB/s  ({} bytes)",
        name,
        per * 1e9,
        line.len() as f64 / per / (1024.0 * 1024.0),
        line.len()
    );
}

fn main() {
    bench(
        "plain",
        "Paragraph with plain text only and no markup at all just words",
    );
    bench(
        "one-bold",
        "Paragraph with *bold text* and tail words to pad it out okay",
    );
    bench(
        "four-emph",
        "P with *bold text*, _italic text_, +underlined+, ~struck~ tail",
    );
    bench(
        "one-code",
        "Paragraph with `inline code` and tail words to pad it out ok",
    );
    bench(
        "one-link",
        "Paragraph with a @link[link text](https://example.com/x) tail",
    );
    bench(
        "escapes",
        "This costs \\*5\\* dollars and \\_that\\_ is fine by me okay",
    );
    bench(
        "nested-emph",
        "*bold with _italic inside_ and back to bold* plus tail text",
    );
}
