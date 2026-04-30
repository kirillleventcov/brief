use crate::shortcode::ArgValue;
use crate::span::Span;
use std::collections::BTreeMap;

#[derive(Clone, Debug)]
pub struct Document {
    pub blocks: Vec<Block>,
    pub metadata: Option<toml::Table>,
}

#[derive(Clone, Debug)]
pub enum Block {
    Heading {
        level: u8,
        content: Vec<Inline>,
        anchor: Option<String>,
        span: Span,
    },
    Paragraph {
        content: Vec<Inline>,
        span: Span,
    },
    List {
        ordered: bool,
        items: Vec<ListItem>,
        span: Span,
    },
    Blockquote {
        children: Vec<Block>,
        span: Span,
    },
    CodeBlock {
        lang: Option<String>,
        body: String,
        attrs: CodeAttrs,
        span: Span,
    },
    Table {
        args: ShortArgs,
        header: Row,
        rows: Vec<Row>,
        span: Span,
    },
    BlockShortcode {
        name: String,
        args: ShortArgs,
        children: Vec<Block>,
        span: Span,
    },
    HorizontalRule {
        span: Span,
    },
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CodeAttrs {
    pub nominify: bool,
    pub minify: bool,
    /// `@minify-keep-comments` — State 3. Implies minification but the
    /// minifier preserves comments (converting `//` to `/* */` where
    /// necessary, with a B0703 warning per conversion).
    pub keep_comments: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TaskState {
    Done,
    Todo,
}

#[derive(Clone, Debug)]
pub struct ListItem {
    pub content: Vec<Inline>,
    pub children: Vec<Block>,
    /// Set when the item begins with the literal `[x] ` (Done) or `[ ] `
    /// (Todo) marker. Marker bytes are consumed by the parser; `content`
    /// holds the rest. Only unordered list items carry this.
    pub task: Option<TaskState>,
    pub span: Span,
}

#[derive(Clone, Debug)]
pub struct Row {
    pub cells: Vec<Vec<Inline>>,
    pub span: Span,
}

#[derive(Clone, Debug)]
pub enum Inline {
    Text {
        value: String,
        span: Span,
    },
    Bold {
        content: Vec<Inline>,
        span: Span,
    },
    Italic {
        content: Vec<Inline>,
        span: Span,
    },
    Underline {
        content: Vec<Inline>,
        span: Span,
    },
    Strike {
        content: Vec<Inline>,
        span: Span,
    },
    InlineCode {
        value: String,
        span: Span,
    },
    HardBreak {
        span: Span,
    },
    Shortcode {
        name: String,
        args: ShortArgs,
        content: Option<Vec<Inline>>,
        span: Span,
    },
}

#[derive(Clone, Debug, Default)]
pub struct ShortArgs {
    pub positional: Vec<ArgValue>,
    pub keyword: BTreeMap<String, ArgValue>,
}

impl ShortArgs {
    pub fn is_empty(&self) -> bool {
        self.positional.is_empty() && self.keyword.is_empty()
    }
}
