# Brief

A strict markup language for documents written by humans and consumed by humans _and_ LLMs.

**Docs: [docs.brief.kirillleventcov.com](https://docs.brief.kirillleventcov.com/)**

> [!WARNING]
> This is a personal project. It's **nowhere near production grade**, nor contains best quality code. It exists because it solves a set of very specific problems I have with Markdown.
> It may improve over time, or it may not.

## Why it exists

Markdown is forgiving — it silently accepts ambiguous input. That's fine for a blog post and bad for documents you feed to an LLM, where every token costs money. Furthermore, building any Markdown central product becomes difficult when each other product has their own version of Markdown and quirks. [Why are we using Markdown](https://bgslabs.org/blog/why-are-we-using-markdown/) is a great read on this topic.

Brief, on the other hand follows the following conventions:

- **One way to do it.** Every construct has a single canonical spelling. `*bold*`, not `**bold**`. `-` for bullets, never `*` or `+`. Sequential ordered lists or it's an error.
- **Compiler time errors.** Ambiguous input is a compile error with a source span and an error code, in the style of `rustc`. There are no warnings — just success or failure.
- **Token-economic LLM target.** `brief compile doc.brf --target=llm` produces a deterministic, token-minimized rendering for LLM prompts, with `--report-tokens` for budgeting.
- **No inline HTML.** The single extension point is registered shortcodes.

## Learn the syntax

Read [`LearnXinYminutes.brf`](./LearnXinYminutes.brf) — it's the whole language, top to bottom, in one file.

Compile it for your AI Agent: `brief compile LearnXinYminutes.brf --target=llm`

## Build

```bash
cargo build --release
brief compile doc.brf --target=html              # writes to stdout
brief compile doc.brf --target=html -w           # writes doc.html next to source
brief compile doc.brf --target=llm -w            # writes doc.txt next to source
brief compile doc.brf --target=html -o out.html  # writes to an explicit path
brief compile doc.brf --target=llm --report-tokens
brief explain <CODE> # Explain compilation error
```

The repo is a Cargo workspace. The crates are:

- [`crates/brief-core`](./crates/brief-core) — the parser, AST, validators, HTML / LLM emitters, formatter, and Markdown→Brief converter, exposed as a library.
- [`crates/brief-cli`](./crates/brief-cli) — the `brief` command-line binary used in the examples above.
- [`crates/brief-web`](./crates/brief-web) — `brief-web`, a static-site generator and dev server for `.brf` documents. See below.

## Web rendering (`brief-web`)

`brief-web` turns a directory of `.brf` files into an HTML site with a
sidebar, themes, and live-reload during development. It is to Brief what
`mdBook` is to Markdown: a separate binary that depends on the Brief
compiler as a library.

```
cargo build -p brief-web --release
./target/release/brief-web serve examples/learn-x-in-y-minutes --no-watch
# open http://127.0.0.1:3000
```

That example hosts `LearnXinYminutes.brf` as a rendered, browseable single
page. Editing the source rebuilds and reloads the browser automatically.

Operational docs (project layout, `book.toml`, `SUMMARY.brf`, the `@page`
cross-link shortcode, themes) are in
[`crates/brief-web/README.md`](./crates/brief-web/README.md).

## Code-block minification (LLM mode)

In `--target=llm` mode, fenced code blocks are minified for languages
where minification is meaning-preserving for an LLM consumer. Single best improvement in terms of LLM tokens comes from JSON code blocks or heavily nested code.

Comments are stripped by default; whitespace collapses to the minimum required to preserve token boundaries. JS / TS and Go preserve newlines (their automatic semicolon insertion makes whitespace-stripping ambiguous without a real parser). C / C++ preserve newlines around `#`-preprocessor lines. All other languages (Rust, Java, SQL) collapse to a single line.

### Three states per block

State 1 — full minification (default):

    ```rust
    fn add(a: i32, b: i32) -> i32 {
        // Adds two numbers.
        a + b
    }
    ```

→ `fn add(a:i32,b:i32)->i32{a+b}`

State 2 — verbatim:

    ```rust @nominify
    fn add(a: i32, b: i32) -> i32 { /* preserved */ }
    ```

State 3 — minified whitespace, comments preserved:

    ```rust @minify-keep-comments
    fn add(a: i32, b: i32) -> i32 {
        // Adds two numbers.
        a + b
    }
    ```

→ `fn add(a:i32,b:i32)->i32{/* Adds two numbers.*/a+b}` (and emits a
`B0703` warning per `//`-to-`/* */` conversion).

### Per-block opt-in

`@minify` overrides `compile.llm.minify_code_blocks = false` and the
document-level `minify_code = false` in frontmatter:

    ```json @minify
    { ... }
    ```

### Refused languages

Python, YAML, and Makefile have **significant whitespace**, so they
cannot be safely minified. If a `python` / `py` / `yaml` / `yml` /
`makefile` / `make` / `mk` block is forced to minify (via `@minify` or
by adding the tag to `minify_languages`), the compiler emits a `B0704`
error to stderr and falls back to verbatim emission.

## Convert from Markdown

> For best results it's better to hand re-write Markdown to Brief. For quick wins Markdown converter might be sufficient.

```
brief convert notes.md                      # writes notes.brf next to input
brief convert notes.md -o renamed.brf       # rename single output
brief convert notes.md --stdout             # pipe to stdout
brief convert *.md                          # batch; per-file failures don't abort
```

The converter is lossy by design: every Markdown construct without a clean
Brief equivalent is rewritten and reported on stderr as a "design hole".
Hole markers are also injected into the output as `// TODO[B-hole:...]`
comments where applicable (inline HTML, HTML blocks, frontmatter), so they
remain greppable in the converted corpus.

## Fuzzing

The parser is fuzzed with [cargo-fuzz][]. Four targets cover the lex/parse
pipeline, the inline parser, the shortcode argument grammar, and the
markdown→brief converter (round-trip). See [`crates/brief-core/fuzz/README.md`](./crates/brief-core/fuzz/README.md)
for setup and run instructions.

[cargo-fuzz]: https://rust-fuzz.github.io/book/cargo-fuzz.html

## Status

Pre v1.0. Expect rough edges.
c
