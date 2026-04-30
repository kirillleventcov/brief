use crate::span::{SourceMap, Span};
use std::fmt::Write;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Severity {
    Error,
    Warning,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Code {
    InvalidUtf8 = 101,
    TabCharacter = 102,
    BomNotAtStart = 103,
    UnexpectedChar = 104,

    EmphasisSameMarker = 204,
    EmphasisCrossLine = 205,
    DoubledEmphasis = 206,
    UnterminatedEmph = 207,
    UnterminatedCode = 208,

    HeadingTooDeep = 301,
    HeadingNoSpace = 302,
    BadIndent = 303,
    BadHorizontalRule = 304,
    UnterminatedFence = 305,
    UnterminatedBlock = 306,
    InlineBlockComment = 307,
    BadListMarker = 308,
    EmptyDocument = 309,
    BadBlockquote = 310,
    StrayEnd = 311,
    StrayContent = 312,
    UnterminatedFrontmatter = 313,
    FrontmatterToml = 314,
    UnknownCodeAttribute = 315,
    ConflictingCodeAttributes = 316,
    BadHeadingAnchor = 317,

    CodeBlockLineCount = 702,
    LineCommentConverted = 703,
    RefusedLanguage = 704,

    UnknownShortcode = 401,
    ArgTypeMismatch = 402,
    MissingArg = 403,
    BadEnumValue = 404,
    FormMismatch = 405,
    BadArgSyntax = 406,
    DuplicateKwarg = 407,
    DeprecatedCalloutKind = 408,

    OrderedListSequence = 501,
    TableColumnMismatch = 502,
    HeadingMonotonic = 503,
    AlignArrayLength = 504,
    DuplicateHeadingAnchor = 506,
}

impl Code {
    pub fn as_str(self) -> String {
        format!("B{:04}", self as u32)
    }

    pub fn message(self) -> &'static str {
        use Code::*;
        match self {
            InvalidUtf8 => "invalid UTF-8 in source",
            TabCharacter => "tab character is not allowed; use two spaces",
            BomNotAtStart => "byte-order mark must only appear at start of file",
            UnexpectedChar => "unexpected character",
            EmphasisSameMarker => "emphasis cannot nest with the same marker",
            EmphasisCrossLine => "emphasis must open and close on the same line",
            DoubledEmphasis => "doubled emphasis markers are not valid; use a single marker",
            UnterminatedEmph => "unterminated emphasis",
            UnterminatedCode => "unterminated inline code span",
            HeadingTooDeep => "heading level exceeds maximum of 6",
            HeadingNoSpace => "heading marker must be followed by exactly one space",
            BadIndent => "indentation must be in multiples of two spaces",
            BadHorizontalRule => "horizontal rule must be exactly three dashes",
            UnterminatedFence => "unterminated code fence",
            UnterminatedBlock => "unterminated block shortcode",
            InlineBlockComment => "block comments must start at the beginning of a line",
            BadListMarker => "invalid list marker",
            EmptyDocument => "document is empty",
            BadBlockquote => "blockquote marker must be followed by a space",
            StrayEnd => "`@end` without a matching block shortcode",
            StrayContent => "unexpected content after directive",
            UnterminatedFrontmatter => "frontmatter `+++` block is never closed",
            FrontmatterToml => "frontmatter is not valid TOML",
            UnknownCodeAttribute => "unknown code-fence attribute",
            ConflictingCodeAttributes => "conflicting code-fence attributes",
            BadHeadingAnchor => "invalid heading anchor",
            CodeBlockLineCount => {
                "minified code block was originally many lines; LLM consumers cannot reference specific lines"
            }
            LineCommentConverted => "line comment converted to block-comment form for minification",
            RefusedLanguage => "language uses significant whitespace and cannot be safely minified",
            UnknownShortcode => "shortcode is not registered",
            ArgTypeMismatch => "shortcode argument has wrong type",
            MissingArg => "missing required shortcode argument",
            BadEnumValue => "argument value is not in the allowed set",
            FormMismatch => "shortcode used in the wrong form (block vs. inline)",
            BadArgSyntax => "malformed shortcode argument syntax",
            DuplicateKwarg => "keyword argument given more than once",
            DeprecatedCalloutKind => "callout kind is deprecated; use the GFM equivalent",
            OrderedListSequence => "ordered list numbering must be sequential starting from 1",
            TableColumnMismatch => "table row column count does not match header",
            HeadingMonotonic => "heading levels must increase by at most one",
            AlignArrayLength => "alignment array length must equal the column count",
            DuplicateHeadingAnchor => "heading anchor must be unique within a document",
        }
    }
}

#[derive(Clone, Debug)]
pub struct Diagnostic {
    pub code: Code,
    pub span: Span,
    pub label: Option<String>,
    pub help: Option<String>,
    pub severity: Severity,
}

impl Diagnostic {
    pub fn new(code: Code, span: Span) -> Self {
        Diagnostic {
            code,
            span,
            label: None,
            help: None,
            severity: Severity::Error,
        }
    }
    pub fn warning(code: Code, span: Span) -> Self {
        Diagnostic {
            code,
            span,
            label: None,
            help: None,
            severity: Severity::Warning,
        }
    }
    pub fn label(mut self, s: impl Into<String>) -> Self {
        self.label = Some(s.into());
        self
    }
    pub fn help(mut self, s: impl Into<String>) -> Self {
        self.help = Some(s.into());
        self
    }
}

pub fn render(diag: &Diagnostic, src: &SourceMap) -> String {
    let mut out = String::new();
    let (line, col) = src.line_col(diag.span.start);
    let prefix = match diag.severity {
        Severity::Error => "error",
        Severity::Warning => "warning",
    };
    let _ = writeln!(
        out,
        "{}[{}]: {}",
        prefix,
        diag.code.as_str(),
        diag.code.message()
    );
    let _ = writeln!(out, "  --> {}:{}:{}", src.path, line, col);
    let _ = writeln!(out, "   |");
    let line_text = src.line_text(line);
    let _ = writeln!(out, "{:>3} | {}", line, line_text);
    let pad: String = std::iter::repeat(' ').take(col.saturating_sub(1)).collect();
    let line_remaining = line_text
        .chars()
        .count()
        .saturating_sub(col.saturating_sub(1));
    let caret_len = (diag.span.len as usize).max(1).min(line_remaining.max(1));
    let carets: String = std::iter::repeat('^').take(caret_len).collect();
    let label = diag.label.as_deref().unwrap_or("");
    let _ = writeln!(out, "   | {}{} {}", pad, carets, label);
    if let Some(help) = &diag.help {
        let _ = writeln!(out, "   |");
        let _ = writeln!(out, "   = help: {}", help);
    }
    out
}

pub fn render_all(diags: &[Diagnostic], src: &SourceMap) -> String {
    diags
        .iter()
        .map(|d| render(d, src))
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_with_caret() {
        let src = SourceMap::new("doc.brf", "abc\nhello world\n");
        let span = Span::new(4, 5);
        let d = Diagnostic::new(Code::UnexpectedChar, span).label("here");
        let out = render(&d, &src);
        assert!(out.contains("error[B0104]"));
        assert!(out.contains("doc.brf:2:1"));
        assert!(out.contains("hello world"));
        assert!(out.contains("^^^^^"));
    }
}
