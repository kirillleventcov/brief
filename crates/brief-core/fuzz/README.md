# Fuzzing Brief

The Brief parser is hand-written recursive descent over line-tokens. That's
exactly the shape of code where fuzzing finds crashes, infinite loops, and
UTF-8 boundary panics fast. This directory holds the [cargo-fuzz][] harness.

[cargo-fuzz]: https://rust-fuzz.github.io/book/cargo-fuzz.html

## One-time setup

```sh
rustup toolchain install nightly --component rust-src
cargo install cargo-fuzz
```

cargo-fuzz requires a nightly toolchain because it depends on
`-Z sanitizer` instrumentation.

## Targets

| Target          | Surface                                              |
| --------------- | ---------------------------------------------------- |
| `parse_full`    | `lex` → `parse` → `resolve` → `validate` → emit HTML + LLM |
| `parse_inline`  | `inline::parse_inline` (emphasis, escapes, code, shortcodes) |
| `parse_args`    | `inline::parse_args` (shortcode argument grammar)    |
| `convert_md`    | `convert::convert` then re-parse the brief output (round-trip) |

`parse_full` and `convert_md` are the broadest. `parse_inline` is the
fastest per-iteration and the densest in interesting parser logic.

## Run a target

```sh
# Run forever (Ctrl-C to stop):
cargo +nightly fuzz run parse_full

# Time-bounded run (use this for a weekend soak):
cargo +nightly fuzz run parse_full -- -max_total_time=172800   # 48h

# Per-input timeout (kill anything that takes >10s — catches infinite loops):
cargo +nightly fuzz run parse_full -- -timeout=10

# Combine — typical "weekend" command:
cargo +nightly fuzz run parse_full -- -max_total_time=172800 -timeout=10
```

`max_total_time` is wall-clock seconds. `timeout` is per-input.

## When a crash is found

libFuzzer writes the failing input to `fuzz/artifacts/<target>/crash-<hash>`
and prints a `Reproduce with:` line. To re-run a specific crash:

```sh
cargo +nightly fuzz run parse_full fuzz/artifacts/parse_full/crash-<hash>
```

To shrink a crashing input to its minimal form:

```sh
cargo +nightly fuzz tmin parse_full fuzz/artifacts/parse_full/crash-<hash>
```

The minimized file lands in the same directory with a `minimized-from-…`
prefix. Add a regression test against the minimized input in the relevant
unit test module before fixing the bug.

## Corpus

Seed inputs live in `corpus/<target>/`. Add new ones whenever you find a
shape the existing seeds don't cover; libfuzzer mutates from these. Mutated
inputs that cover new edges are auto-saved to `corpus/<target>/` as well —
those are gitignored (they're hex-named).

## Coverage

```sh
cargo +nightly fuzz coverage parse_full
# Then use llvm-cov to render a report (see cargo-fuzz docs for the exact
# llvm-cov invocation; the binary lives under fuzz/target/...).
```

## Known limitations

- All targets bound input size (16–64 KB). Real Brief documents are well
  under that; the cap stops the fuzzer from wasting time stress-testing
  the allocator instead of the parser.
- Stack depth is not artificially bounded. Pathological inputs with deeply
  nested blockquotes or block shortcodes can stack-overflow before the
  parser produces a diagnostic. Fixing that is on the v0.2 list, not here.
- Fuzzing rejects non-UTF-8 input via `from_utf8`. The lexer never sees
  invalid UTF-8 in practice (files are read via `read_to_string`).
