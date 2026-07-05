#![no_main]

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
    // Bound input so the fuzzer doesn't waste time on large mutations that
    // only stress allocator throughput. Real Brief files are well under this.
    if input.len() > 64 * 1024 {
        return;
    }

    let src = SourceMap::new("fuzz.brf", input);
    let tokens = match lexer::lex(&src) {
        Ok(t) => t,
        Err(_) => return,
    };
    let (mut doc, _diags) = parser::parse(tokens, &src);
    let registry = Registry::with_builtins();
    let _ = resolve::resolve(&mut doc, &registry);
    let _ = validate::validate(&doc, &ValidateOpts::default(), &src);

    // Emitters must not panic on any well-typed AST the parser produces, even
    // for inputs that produced diagnostics — diagnostics are non-fatal.
    let _ = html::render(&doc, &registry);
    let _ = llm::render(
        &doc,
        &registry,
        &llm::Opts::default(),
    );
});
