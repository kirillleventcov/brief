# brief-web

Static-site generator for [Brief][brief] documents. `brief-web` is to Brief
what `mdBook` is to Markdown: a separate executable and library that takes a
directory of `.brf` files and renders an HTML site, with a development server
and live reload.

## Install

From the workspace root:

```
cargo build -p brief-web --release
./target/release/brief-web --help
```

## Quick start

A new project:

```
brief-web init my-site            # single page
brief-web init my-site --multi    # multi-page (with SUMMARY.brf)
cd my-site
brief-web serve                   # http://127.0.0.1:3000, live reload
```

A one-shot build:

```
brief-web build my-site           # writes ./my-site/dist/
```

The repo ships an example: hosting `LearnXinYminutes.brf` as a single-page
site —

```
brief-web serve examples/learn-x-in-y-minutes
# open http://127.0.0.1:3000
```

## Project layout

```
my-site/
├── book.toml                     # site config
├── src/
│   ├── SUMMARY.brf               # optional; presence = multi-page mode
│   ├── index.brf
│   └── chapters/
│       └── intro.brf
├── theme/                        # optional override of the built-in theme
│   ├── page.html
│   └── style.css
└── dist/                         # build output (gitignored)
```

If `SUMMARY.brf` is absent, `brief-web` runs in single-page mode and renders
only `src/index.brf`.

## `book.toml`

```toml
[book]
title       = "My site"
description = "What this site is about."
authors     = ["You"]

[build]
src = "src"     # default
out = "dist"   # default

[site]
base_url = "/"
# theme  = "theme"    # uncomment to point at a custom theme directory

[server]
host = "127.0.0.1"
port = 3000

# Optional: register Brief shortcodes scoped to this site. These extend the
# built-in registry exactly the way `[shortcodes.*]` in `brief.toml` does.
# [shortcodes.tweet]
# kind = "inline"
# template_html = "..."
```

## `SUMMARY.brf`

A `SUMMARY.brf` _is_ the navigation tree:

```brief
# Summary

- @ref[introduction.brf](Introduction)
- @ref[getting-started.brf](Getting Started)
  - @ref[install.brf](Installation)
  - @ref[first-document.brf](Your First Document)
- @ref[reference/grammar.brf](Grammar Reference)
```

Headings (level 2+) become section dividers in the sidebar.

## Cross-page links: `@ref`

`brief-web` registers a `@ref` inline shortcode and resolves it at build
time:

```brief
See @ref[getting-started.brf](Getting Started) for the next step.
See @ref[reference/grammar.brf#headings](Headings) for the heading rules.
```

- `path` is project-relative (rooted at `brief.toml`).
- An `#anchor` suffix targets a heading anchor in that page; missing
  files / missing anchors are build warnings.

## Themes

`brief-web` ships an embedded default theme (light/dark via
`prefers-color-scheme`, sticky sidebar, mobile collapse).

To override, drop `theme/page.html` and/or `theme/style.css` into your project. The template is mustache-style with these placeholders:

| Placeholder           | Meaning                                         |
| --------------------- | ----------------------------------------------- |
| `{{ title }}`         | Page title (H1, falling back to SUMMARY title). |
| `{{ description }}`   | First paragraph, used for `<meta description>`. |
| `{{ site_title }}`    | `[book].title` from `book.toml`.                |
| `{{ content }}`       | Rendered HTML for the page body.                |
| `{{ sidebar }}`       | Rendered HTML for the navigation sidebar.       |
| `{{ base_url }}`      | `[site].base_url`.                              |
| `{{ stylesheet }}`    | URL of `style.css`.                             |
| `{{ reload_script }}` | Live-reload `<script>` (empty in `build`).      |

No conditionals or loops. Logic that needs them belongs in a Brief
shortcode, not in the template.

## Live reload

`brief-web serve` watches `src/`, `book.toml`, and `theme/` (debounced to
~150 ms), rebuilds, and pushes a `reload` event over a Server-Sent-Events
stream at `/__brief_web_events`. The default theme injects a tiny client
that reloads the page when it sees one. No WebSockets, no polling.

## CLI

```
brief-web init   [PATH]                 # scaffold (--multi, --force)
brief-web build  [PATH]                 # one-shot build into dist/
brief-web serve  [PATH] [--host H]      # build + live-reload server
                       [--port N]
                       [--no-watch]
```

`PATH` defaults to the current directory in every command.

## What's _not_ in here

Out of scope for v1: client-side search, WASM playground, print view, i18n,
RSS, plugin/preprocessor protocol.

[brief]: ../../README.md
