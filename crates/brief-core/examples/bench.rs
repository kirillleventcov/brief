//! Stage-level benchmark for the Brief pipeline.
//!
//! Usage: cargo run --release -p brief-core --example bench -- <file.brf> [iters]
//!
//! Times each compiler stage (lex, parse, resolve, validate, emit-html,
//! emit-llm, fmt) separately over `iters` iterations and prints the best
//! (minimum) per-iteration time plus throughput in MiB/s of source input.

#[global_allocator]
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;

use std::hint::black_box;
use std::time::Instant;

use brief::emit::{html, llm};
use brief::shortcode::Registry;
use brief::span::SourceMap;
use brief::validate::ValidateOpts;
use brief::{fmt, lexer, parser, resolve, validate};

fn main() {
    let mut args = std::env::args().skip(1);
    let path = args.next().expect("usage: bench <file.brf> [iters]");
    let iters: usize = args.next().map(|s| s.parse().unwrap()).unwrap_or(10);

    let raw = std::fs::read_to_string(&path).expect("read input");
    let bytes = raw.len();
    let src = SourceMap::new(path.clone(), raw.clone());
    let registry = Registry::default();
    let vopts = ValidateOpts::default();
    let lopts = llm::Opts::default();

    let mib = bytes as f64 / (1024.0 * 1024.0);
    println!(
        "input: {} ({} bytes, {:.2} MiB), {} iters",
        path, bytes, mib, iters
    );

    let report = |name: &str, best: f64| {
        println!(
            "{:<10} {:>10.3} ms   {:>8.1} MiB/s",
            name,
            best * 1e3,
            mib / best
        );
    };

    // lex
    let mut best = f64::MAX;
    for _ in 0..iters {
        let t = Instant::now();
        let tokens = lexer::lex(black_box(&src)).expect("lex ok");
        best = best.min(t.elapsed().as_secs_f64());
        black_box(tokens);
    }
    report("lex", best);

    // parse (includes a fresh lex per iter, subtracted via separate timing)
    let tokens = lexer::lex(&src).expect("lex ok");
    let mut best = f64::MAX;
    for _ in 0..iters {
        let toks = tokens.clone();
        let t = Instant::now();
        let (doc, diags) = parser::parse(black_box(toks), &src);
        best = best.min(t.elapsed().as_secs_f64());
        black_box((doc, diags));
    }
    report("parse", best);

    // resolve + validate
    let (doc0, _) = parser::parse(tokens.clone(), &src);
    let mut best = f64::MAX;
    for _ in 0..iters {
        let mut doc = doc0.clone();
        let t = Instant::now();
        let d1 = resolve::resolve(black_box(&mut doc), &registry);
        let d2 = validate::validate(&doc, &vopts, &src);
        best = best.min(t.elapsed().as_secs_f64());
        black_box((d1, d2));
    }
    report("res+val", best);

    // fully resolved doc for emitters
    let mut doc = doc0.clone();
    let _ = resolve::resolve(&mut doc, &registry);

    let mut best = f64::MAX;
    for _ in 0..iters {
        let t = Instant::now();
        let out = html::render(black_box(&doc), &registry);
        best = best.min(t.elapsed().as_secs_f64());
        black_box(out);
    }
    report("emit-html", best);

    let mut best = f64::MAX;
    for _ in 0..iters {
        let t = Instant::now();
        let out = llm::render(black_box(&doc), &registry, &lopts);
        best = best.min(t.elapsed().as_secs_f64());
        black_box(out);
    }
    report("emit-llm", best);

    let fopts = fmt::Opts::default();
    let mut best = f64::MAX;
    for _ in 0..iters {
        let t = Instant::now();
        let out = fmt::format(black_box(&raw), &fopts);
        best = best.min(t.elapsed().as_secs_f64());
        black_box(out);
    }
    report("fmt", best);
}
