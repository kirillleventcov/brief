//! The Brief document tree produced by [`crate::parser`] and consumed by
//! the resolver, validators, and emitters.

use crate::shortcode::ArgValue;
use crate::span::Span;
use std::collections::BTreeMap;
use toml::Table;

/// A parsed Brief document: its block tree plus document-level data.
#[derive(Clone, Debug, Default)]
#[non_exhaustive]
pub struct Document {
    pub blocks: Vec<Block>,
    /// The `+++` TOML frontmatter table, if the document has one.
    pub metadata: Option<Table>,
    /// Validated `@ref` invocations, keyed by source span. Populated by
    /// `resolve_with_project`. Empty when the per-file resolver runs
    /// without project context.
    pub resolved_refs: BTreeMap<Span, ResolvedRef>,
}

/// A cross-file `@ref` whose target has been checked against the project.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct ResolvedRef {
    /// Project-relative target file as a forward-slash-separated string,
    /// validated by `resolve::parse_target`. Always ends in `.brf`.
    pub target_path: String,
    /// Anchor in the target file, or None for a whole-file reference.
    pub target_anchor: Option<String>,
    /// Display text (taken from the `title` positional arg).
    pub display: String,
    /// `target_path` relative to the directory of the file holding the
    /// `@ref`, still ending in `.brf`: `guide.brf` seen from `sub/a.brf` is
    /// `../guide.brf`. `None` when the target is that file itself.
    pub relative_path: Option<String>,
}

impl ResolvedRef {
    /// Link to the target from the referring page, for output where each
    /// `foo.brf` becomes a sibling `foo.<ext>`: `../guide.html#x`, `b.html`,
    /// or just `#x` for an anchor in the same file. Page-relative, so it
    /// stays correct wherever the output tree is deployed.
    #[must_use]
    pub fn href(&self, ext: &str) -> String {
        let mut out = String::new();
        let file = match &self.relative_path {
            Some(p) => Some(p.as_str()),
            None if self.target_anchor.is_some() => None,
            None => Some(self.target_path.rsplit('/').next().unwrap_or("")),
        };
        if let Some(f) = file {
            out.push_str(f.strip_suffix(".brf").unwrap_or(f));
            out.push('.');
            out.push_str(ext);
        }
        if let Some(a) = &self.target_anchor {
            out.push('#');
            out.push_str(a);
        }
        out
    }
}

/// A block-level node. Every variant carries the span of the source lines
/// it was parsed from.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub enum Block {
    /// `# text` through `###### text`, with an optional `{#anchor}`.
    Heading {
        /// Heading level, 1 through 6.
        level: u8,
        content: Vec<Inline>,
        /// Explicit anchor from a trailing `{#anchor}`, if given.
        anchor: Option<String>,
        span: Span,
    },
    /// Consecutive prose lines, joined with single spaces.
    Paragraph { content: Vec<Inline>, span: Span },
    /// A `- ` (unordered) or `1. ` (ordered) list.
    List {
        ordered: bool,
        items: Vec<ListItem>,
        span: Span,
    },
    /// `> ` lines; deeper `>>` nesting becomes child blockquotes.
    Blockquote { children: Vec<Block>, span: Span },
    /// A triple-backtick fence or `@code` block; `body` is verbatim.
    CodeBlock {
        lang: Option<String>,
        body: String,
        attrs: CodeAttrs,
        span: Span,
    },
    /// An `@t` table: a header row followed by body rows.
    Table {
        args: ShortcodeArgs,
        header: Row,
        rows: Vec<Row>,
        span: Span,
    },
    /// An `@dl` definition list of `term` / `: definition` pairs.
    DefinitionList {
        args: ShortcodeArgs,
        items: Vec<DefinitionItem>,
        span: Span,
    },
    /// An `@name(args) … @end` block shortcode wrapping child blocks.
    BlockShortcode {
        name: String,
        args: ShortcodeArgs,
        children: Vec<Block>,
        span: Span,
    },
    /// A `---` line.
    HorizontalRule { span: Span },
}

/// `@`-attributes given on a code fence's info line.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct CodeAttrs {
    /// `@nominify` — never minify this block.
    pub nominify: bool,
    /// `@minify` — minify this block.
    pub minify: bool,
    /// `@minify-keep-comments` — State 3. Implies minification but the
    /// minifier preserves comments (converting `//` to `/* */` where
    /// necessary, with a B0703 warning per conversion).
    pub keep_comments: bool,
}

/// Checkbox state of a task-list item.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TaskState {
    /// `[x]`.
    Done,
    /// `[ ]`.
    Todo,
}

/// One item of a [`Block::List`].
#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct ListItem {
    /// Inline content of the item's marker line.
    pub content: Vec<Inline>,
    /// Blocks indented under the item.
    pub children: Vec<Block>,
    /// Set when the item begins with the literal `[x] ` (Done) or `[ ] `
    /// (Todo) marker. Marker bytes are consumed by the parser; `content`
    /// holds the rest. Both unordered and ordered list items can carry this.
    pub task: Option<TaskState>,
    pub span: Span,
}

/// One `|`-separated row of a [`Block::Table`].
#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct Row {
    pub cells: Vec<Vec<Inline>>,
    pub span: Span,
}

/// A term and its definition in a [`Block::DefinitionList`].
#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct DefinitionItem {
    pub term: Vec<Inline>,
    pub definition: Vec<Inline>,
    /// Covers the term line through the last definition line.
    pub span: Span,
}

/// An inline node within a block's text.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub enum Inline {
    /// Plain text with escapes already resolved.
    Text { value: String, span: Span },
    /// `*bold*`.
    Bold { content: Vec<Inline>, span: Span },
    /// `_italic_`.
    Italic { content: Vec<Inline>, span: Span },
    /// `+underline+`.
    Underline { content: Vec<Inline>, span: Span },
    /// `~strike~`.
    Strike { content: Vec<Inline>, span: Span },
    /// `` `code` ``; `value` is verbatim.
    InlineCode { value: String, span: Span },
    /// A line ending in `\`.
    HardBreak { span: Span },
    /// `@name(args)` with optional `[content]`.
    Shortcode {
        name: String,
        args: ShortcodeArgs,
        content: Option<Vec<Inline>>,
        span: Span,
    },
}

/// Arguments of a shortcode invocation: `@name(pos1, pos2, key: value)`.
#[derive(Clone, Debug, Default)]
pub struct ShortcodeArgs {
    pub positional: Vec<ArgValue>,
    pub keyword: BTreeMap<String, ArgValue>,
}

impl ShortcodeArgs {
    /// True when the invocation has no arguments at all.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.positional.is_empty() && self.keyword.is_empty()
    }
}
