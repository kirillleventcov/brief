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
}

#[derive(Clone, Debug)]
pub struct ListItem {
    pub content: Vec<Inline>,
    pub children: Vec<Block>,
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
