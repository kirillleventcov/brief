# brief-core

The reference compiler for the [Brief](https://docs.brief.kirillleventcov.com)
markup language: lexer, parser, AST, HTML and LLM emitters, formatter, and a
Markdown-to-Brief converter — all as a single library crate.

Brief is a writer-first markup format designed to be both human-pleasant and
LLM-friendly. The format spec doubles as a quickstart and lives in
[`LearnXinYminutes.brf`](https://github.com/kirillleventcov/brief/blob/main/LearnXinYminutes.brf).

## Use it from Rust

```rust
use brief::emit::html;
use brief::lexer;
use brief::parser;
use brief::resolve;
use brief::shortcode::Registry;
use brief::span::SourceMap;
use brief::validate::{self, ValidateOptions};

let src = SourceMap::new("hello.brf", "# Hello\nworld\n");
let tokens = lexer::lex(&src).expect("lex");
let (mut doc, mut diagnostics) = parser::parse(tokens, &src);
let registry = Registry::with_builtins();
diagnostics.extend(resolve::resolve(&mut doc, &registry));
diagnostics.extend(validate::validate(&doc, &ValidateOptions::default(), &src));
let rendered = html::render(&doc, &registry);
println!("{rendered}");
```

Convert Markdown to Brief:

```rust
use brief::convert;

let result = convert::convert("# Hello\n\nworld\n", "hello.md");
println!("{}", result.brief_source);
```

## What's in the box

Public modules: `ast`, `config`, `convert`, `diag`, `emit`, `fmt`, `inline`,
`lexer`, `minify`, `parser`, `project`, `resolve`, `shortcode`, `span`,
`token`, `validate`, `watch`.

## CLI tooling

The `brief` CLI, the `brief-web` static-site generator, and the `brief-lsp`
language server live in [sibling crates](https://github.com/kirillleventcov/brief/tree/main/crates)
in the upstream repository. They are not yet published to crates.io.

## License

MIT. See the [LICENSE](https://github.com/kirillleventcov/brief/blob/main/LICENSE)
file in the upstream repository.
