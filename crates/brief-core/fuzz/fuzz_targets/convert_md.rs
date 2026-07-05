#![no_main]

use brief::convert::convert;
use brief::emit::{html, llm};
use brief::lexer;
use brief::parser;
use brief::resolve;
use brief::shortcode::Registry;
use brief::span::SourceMap;
use brief::validate::{self, ValidateOpts};
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let Ok(input) = std::str::from_utf8(data) else {
        return;
    };
    if input.len() > 64 * 1024 {
        return;
    }

    let result = convert(input, "fuzz.md");

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
    let _ = validate::validate(&doc, &ValidateOpts::default(), &src);
    let _ = html::render(&doc, &registry);
    let _ = llm::render(
        &doc,
        &registry,
        &llm::Opts::default(),
    );
});
