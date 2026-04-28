# Brief

A strict markup language for documents written by humans and consumed by humans _and_ LLMs.

> [!WARNING]
> This is a personal project. It's **nowhere near production grade**, nor contains best quality code. It exists because it solves a set of very specific problem I have with Markdown.
> It may improve over time, or it may not.

## Why it exists

Markdown is forgiving — it guesses, renumbers, and silently accepts ambiguous input. That's fine for a blog post and bad for documents you feed to an LLM, where every token costs money and every ambiguity costs accuracy. Inspired by [Why are we using Markdown](https://bgslabs.org/blog/why-are-we-using-markdown/).

Brief, on the other hand follows the following conventions:

- **One way to do it.** Every construct has a single canonical spelling. `*bold*`, not `**bold**`. `-` for bullets, never `*` or `+`. Sequential ordered lists or it's an error.
- **Errors over guesses.** Ambiguous input is a compile error with a source span and an error code, in the style of `rustc`. There are no warnings — just success or failure.
- **Token-economic LLM target.** `brief compile doc.brf --target=llm` produces a deterministic, token-minimized rendering for LLM prompts, with `--report-tokens` for budgeting.
- **No inline HTML.** The single extension point is registered shortcodes.

## Learn the syntax

Read [`LearnXinYminutes.brf`](./LearnXinYminutes.brf) — it's the whole language, top to bottom, in one file.

Compile it for your AI Agent: `brief compile LearnXinYminutes.brf --target=llm`

## Build

```
cargo build --release
brief compile doc.brf --target=html
brief compile doc.brf --target=llm --report-tokens
```

## Convert from Markdown

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

## Status

v0.1. Expect rough edges.
