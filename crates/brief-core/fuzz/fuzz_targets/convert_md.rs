#![no_main]

use brief::convert;
use brief::emit::{html, llm};
use brief::shortcode::Registry;
use brief::span::SourceMap;
use brief::validate::{self, ValidateOptions};
use brief::{lexer, parser, resolve};
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let Ok(input) = std::str::from_utf8(data) else {
        return;
    };
    if input.len() > 64 * 1024 {
        return;
    }

    let result = convert::convert(input, "fuzz.md");

    // Round-trip: brief output of the converter must itself be parseable
    // without crashing — the converter is not allowed to emit pathological
    // brief that the rest of the pipeline panics on.
    let src = SourceMap::new("converted.brf", &result.brief_source);
    let Ok(tokens) = lexer::lex(&src) else {
        return;
    };
    let (mut doc, _) = parser::parse(tokens, &src);
    let registry = Registry::with_builtins();
    let _ = resolve::resolve(&mut doc, &registry);
    let _ = validate::validate(&doc, &ValidateOptions::default(), &src);
    let _ = html::render(&doc, &registry);
    let _ = llm::render(&doc, &registry, &llm::Options::default());
});
