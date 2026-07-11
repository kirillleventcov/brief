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
    NestingTooDeep = 318,

    MinifyFailed = 701,
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
    UnknownArg = 409,

    OrderedListSequence = 501,
    TableColumnMismatch = 502,
    HeadingMonotonic = 503,
    AlignArrayLength = 504,
    BadDefinitionList = 505,
    DuplicateHeadingAnchor = 506,

    RefMissingFile = 601,
    RefMissingAnchor = 602,
    RefBadTarget = 603,
    RefNoProject = 604,
}

impl Code {
    /// Every diagnostic code the compiler can emit. `brief explain` coverage
    /// is tested against this list — extend it when adding a variant.
    pub const ALL: &'static [Code] = &[
        Code::InvalidUtf8,
        Code::TabCharacter,
        Code::BomNotAtStart,
        Code::UnexpectedChar,
        Code::EmphasisSameMarker,
        Code::EmphasisCrossLine,
        Code::DoubledEmphasis,
        Code::UnterminatedEmph,
        Code::UnterminatedCode,
        Code::HeadingTooDeep,
        Code::HeadingNoSpace,
        Code::BadIndent,
        Code::BadHorizontalRule,
        Code::UnterminatedFence,
        Code::UnterminatedBlock,
        Code::InlineBlockComment,
        Code::BadListMarker,
        Code::EmptyDocument,
        Code::BadBlockquote,
        Code::StrayEnd,
        Code::StrayContent,
        Code::UnterminatedFrontmatter,
        Code::FrontmatterToml,
        Code::UnknownCodeAttribute,
        Code::ConflictingCodeAttributes,
        Code::BadHeadingAnchor,
        Code::NestingTooDeep,
        Code::UnknownShortcode,
        Code::ArgTypeMismatch,
        Code::MissingArg,
        Code::BadEnumValue,
        Code::FormMismatch,
        Code::BadArgSyntax,
        Code::DuplicateKwarg,
        Code::DeprecatedCalloutKind,
        Code::UnknownArg,
        Code::OrderedListSequence,
        Code::TableColumnMismatch,
        Code::HeadingMonotonic,
        Code::AlignArrayLength,
        Code::BadDefinitionList,
        Code::DuplicateHeadingAnchor,
        Code::RefMissingFile,
        Code::RefMissingAnchor,
        Code::RefBadTarget,
        Code::RefNoProject,
        Code::MinifyFailed,
        Code::CodeBlockLineCount,
        Code::LineCommentConverted,
        Code::RefusedLanguage,
    ];

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
            NestingTooDeep => "blocks are nested too deeply",
            BadDefinitionList => "malformed definition list",
            MinifyFailed => "code block did not parse in its tagged language; emitted verbatim",
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
            UnknownArg => "shortcode does not declare this argument",
            OrderedListSequence => "ordered list numbering must be sequential starting from 1",
            TableColumnMismatch => "table row column count does not match header",
            HeadingMonotonic => "heading levels must increase by at most one",
            AlignArrayLength => "alignment array length must equal the column count",
            DuplicateHeadingAnchor => "heading anchor must be unique within a document",
            RefMissingFile => "cross-document reference target file does not exist in project",
            RefMissingAnchor => {
                "cross-document reference target anchor does not exist in target file"
            }
            RefBadTarget => "malformed cross-document reference target",
            RefNoProject => "`@ref` requires a `brief.toml`-rooted project; none found",
        }
    }

    /// Long-form explanation shown by `brief explain <CODE>` and in-editor
    /// hovers. Exhaustive: a new variant will not compile until it has one.
    pub fn explain(self) -> &'static str {
        use Code::*;
        match self {
            InvalidUtf8 => "Brief sources must be valid UTF-8. Re-encode the file (e.g. `iconv -t UTF-8`) or find and fix the corrupt bytes at the reported offset.",
            TabCharacter => "Tabs are forbidden in Brief sources. Configure your editor to insert two spaces.",
            BomNotAtStart => "A UTF-8 byte-order mark is tolerated only as the very first bytes of the file. A BOM anywhere else is usually the result of concatenating files; delete it.",
            UnexpectedChar => "The lexer found a character that cannot begin or continue any Brief construct at this position. Usually a control character or stray escape; delete or replace it.",
            EmphasisSameMarker => "`*outer *inner* outer*` is ambiguous. Use a different marker for the inner span: `*outer _inner_ outer*`.",
            EmphasisCrossLine => "Emphasis spans must open and close on the same source line. Close the span before the line break, or join the lines into one.",
            DoubledEmphasis => "`**bold**`-style doubled markers are Markdown, not Brief. Brief uses single markers: `*bold*`, `_italic_`, `+underline+`, `~strike~`.",
            UnterminatedEmph => "An emphasis marker opened a span that never closes on the same line. Close it, or escape the marker (`\\*`) if it's literal text.",
            UnterminatedCode => "A backtick opened an inline code span that never closes on the same line. Close it, or use a double-backtick span (``` ``code with ` inside`` ```) when the code contains a backtick.",
            HeadingTooDeep => "Brief supports six heading levels. `#######` and deeper are errors. Restructure the document or split it.",
            HeadingNoSpace => "A heading marker is the `#`s followed by exactly one space: `# Title`. No space (or several) is an error so `#hashtag`-style text is never silently promoted to a heading.",
            BadIndent => "Brief indentation is strict: two spaces per nesting level. An odd number of leading spaces (or a level skipped) cannot be interpreted unambiguously; re-indent to multiples of two.",
            BadHorizontalRule => "A horizontal rule is exactly three dashes on their own line: `---`. Two dashes is too few; four or more is too many. This is strict so a typoed rule never silently becomes paragraph text.",
            UnterminatedFence => "A ``` code fence was opened but never closed before end of file. Add the closing ``` line. To show fence syntax inside a code block, use the `@code ... @end` shortcode instead of nesting fences.",
            UnterminatedBlock => "A block shortcode (`@details`, `@dl`, `@code`, a custom block) was never closed. Add `@end` at the same indentation as the opening line.",
            InlineBlockComment => "`/* ... */` comments are block-level in Brief: they must start at the beginning of a line. For a comment after content on the same line there is no inline form — move it to its own line.",
            BadListMarker => "Unordered list items are `- ` (dash, one space); ordered items are `1. ` (number, dot, one space). Markdown's `*` and `+` markers are not list markers in Brief.",
            EmptyDocument => "The source contains no blocks — only whitespace or comments. Brief treats a document with nothing to render as an error rather than emitting an empty output file.",
            BadBlockquote => "Blockquote markers are `>` (or `>>`, `>>>` for nesting) followed by exactly one space. `>text` without the space is rejected so accidental `>` characters are caught.",
            StrayEnd => "`@end` closes a block shortcode, but no block shortcode is open here. Remove it, or check the indentation of the opening line — `@end` must sit at the same indent.",
            StrayContent => "Content appears where the current construct does not allow it — most commonly a `|` row outside a `@t` table. Wrap table rows in `@t ... `(rows end at the first non-`|` line).",
            UnterminatedFrontmatter => "A frontmatter block opened with `+++` was never closed. Add a closing `+++` line, or remove the opening if the document has no metadata.",
            FrontmatterToml => "Frontmatter content must be valid TOML. Brief deliberately uses TOML (not YAML) to match `brief.toml`. Fix the TOML syntax in the `+++ ... +++` block.",
            UnknownCodeAttribute => "Code-fence attributes are `@`-prefixed identifiers after the language tag (e.g. ```json @nominify). v0.4 recognizes `@nominify`, `@minify`, and `@minify-keep-comments`. Anything else is a compile error so typos are caught early.",
            ConflictingCodeAttributes => "`@nominify` and `@minify` (or `@minify-keep-comments`) are mutually exclusive: one says \"never minify this block\" and the other says \"always minify this block.\" Drop one.",
            BadHeadingAnchor => "A heading anchor is a trailing `{#name}` where name matches `[a-z0-9-]+`: `## Title {#title}`. Fix the anchor syntax or remove the braces.",
            NestingTooDeep => "Blocks (lists, blockquotes, block shortcodes) nest at most 64 levels deep. Deeper nesting is almost always generated or accidental input; restructure the document.",
            UnknownShortcode => "Shortcodes must be registered in `brief.toml` under `[shortcodes.<name>]` (or be a built-in: link, image, kbd, sub, sup, details, t, code, callout, math, footnote, ref). Note: `@br` is intentionally not a shortcode — use `\\` at end of line for a hard break.",
            ArgTypeMismatch => "A shortcode argument has the wrong type — e.g. a bare identifier where a quoted string is declared, or an int where an array is expected. The declared type is in `brief.toml` under `[shortcodes.<name>.arguments]` (built-ins are documented in the reference).",
            MissingArg => "A required shortcode argument was not supplied. Add it as `@name(arg: value)` or positionally if the argument declares a `position`.",
            BadEnumValue => "The argument only accepts a fixed set of values (its `oneof` list). For example `@t(align: [...])` entries must be `left`, `right`, or `center`. Use one of the allowed values.",
            FormMismatch => "The shortcode was used in the wrong form: a block shortcode invoked inline, or an inline shortcode used as a block with `@end`. Check the shortcode's declared `kind`.",
            BadArgSyntax => "The argument list does not parse: unbalanced parentheses or brackets, a missing `:` after a keyword, an unterminated string, or an unclosed `(url)` in link sugar (percent-encode unmatched parens as %28/%29).",
            DuplicateKwarg => "The same keyword argument is given more than once (possibly once positionally and once by name). Remove the duplicate.",
            DeprecatedCalloutKind => "This callout kind is a deprecated alias. Use the GFM set: note, tip, important, warning, caution. This is the compiler's only deprecation warning; it compiles with exit 0.",
            UnknownArg => "The shortcode does not declare an argument with this name. Check the spelling against the declared arguments (listed in the diagnostic's help text), or declare it in `brief.toml`.",
            OrderedListSequence => "Ordered lists must number 1, 2, 3, ... renumbering by the renderer is forbidden. Either fix the source or convert to an unordered list.",
            TableColumnMismatch => "Every row in a `@t` table must have the same number of cells as the header row. Add or remove cells until they match.",
            HeadingMonotonic => "With `compile.strict_heading_levels = true`, heading levels may increase by at most one per step (`#` to `##`, never `#` to `###`). Restructure the outline or disable the option.",
            AlignArrayLength => "`@t(align: [...])` must list exactly one alignment per table column. Add or remove entries until the array length matches the header's cell count.",
            BadDefinitionList => "A `@dl` body alternates term lines and `: definition` lines — colon, exactly one space, definition. Terms without definitions, definitions without terms, multiple definitions per term, and extra or missing spaces after `:` are all rejected.",
            DuplicateHeadingAnchor => "Two headings in this document declare the same `{#anchor}`. Anchors are `@ref` targets and must be unique within a file; rename one.",
            RefMissingFile => "Brief verifies cross-document references at compile time. The file referenced by `@ref[path.brf]` was not found anywhere under the project root (the directory containing `brief.toml`). Either fix the path, create the missing file, or move the file into the project tree.",
            RefMissingAnchor => "Brief verifies that the `#anchor` portion of `@ref[file.brf#anchor]` matches a heading anchor declared in the target file (e.g. `## Title {#anchor}`). The anchor was not found. The diagnostic's help text lists the anchors that *do* exist in the target file.",
            RefBadTarget => "`@ref` targets are project-relative `.brf` paths, optionally suffixed with `#anchor`. Leading `/`, `..` segments, backslashes, missing `.brf` extension, and anchors not matching `[a-z0-9-]+` are rejected. Restate the target in canonical form.",
            RefNoProject => "`@ref` only works inside a project rooted by a `brief.toml` file. The compiler walks up from the source file looking for one. Create a `brief.toml` (an empty file is fine) at the desired root, or remove the `@ref` invocation.",
            MinifyFailed => "A code block is tagged with a minifiable language but does not lex in that language (e.g. an unterminated string, or pseudo-code tagged `json`). The block is emitted verbatim and compilation continues; fix the code or the language tag. Compiles with exit 0.",
            CodeBlockLineCount => "A code block is being minified to a single (or near-single) line, but the original spanned more than 50 lines. After minification the LLM consumer cannot reference the original line numbers. Either accept this (silence with `@nominify`) or split the block into smaller pieces.",
            LineCommentConverted => "`@minify-keep-comments` converts `//` line comments into `/* */` block form so they can survive on a single minified line. Any `*/` inside the comment text is rewritten to `* /` so the block cannot close early. Use `@nominify` to keep the source verbatim instead.",
            RefusedLanguage => "Python, YAML, and Makefile use significant whitespace; minification cannot be performed safely without parsing the language. Such blocks are emitted verbatim and the LLM consumer pays full cost. Drop the `@minify` attribute or remove the language from `compile.llm.minify_languages`.",
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

    #[test]
    fn ref_codes_render_with_correct_prefix() {
        use Code::*;
        assert_eq!(RefMissingFile.as_str(), "B0601");
        assert_eq!(RefMissingAnchor.as_str(), "B0602");
        assert_eq!(RefBadTarget.as_str(), "B0603");
        assert_eq!(RefNoProject.as_str(), "B0604");
        assert!(RefMissingFile.message().contains("file"));
        assert!(RefMissingAnchor.message().contains("anchor"));
        assert!(RefBadTarget.message().contains("target"));
        assert!(RefNoProject.message().contains("brief.toml"));
    }

    #[test]
    fn bad_definition_list_code_renders() {
        use Code::*;
        assert_eq!(BadDefinitionList.as_str(), "B0505");
        assert!(BadDefinitionList.message().contains("definition list"));
    }
}
