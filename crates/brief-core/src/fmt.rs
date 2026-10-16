//! `brief fmt` — gofmt-doctrine formatter for Brief sources.
//!
//! Operates on the raw source as a sequence of lines. Which lines are
//! verbatim (frontmatter, fenced and `@code` bodies) and which are table
//! rows is read off the real lexer and parser rather than a second copy of
//! the grammar, so the formatter cannot disagree with the compiler about
//! where a region starts or ends. What it does to the remaining lines —
//! trailing whitespace, leading tabs, blank-line runs, table padding — is
//! invisible to the parser. Emphasis markers, shortcode argument order, and
//! inline content are preserved verbatim — the formatter never re-emits
//! parsed AST.

use crate::ast::Block;
use crate::lexer;
use crate::parser;
use crate::span::SourceMap;
use crate::token::{Token, TokenKind};
use std::collections::BTreeMap;
use std::ops::Range;

#[derive(Debug, Clone, Default)]
pub struct Opts {
    /// Sort top-level keys of frontmatter alphabetically. Off by default
    /// because re-emitting TOML loses comment positions and is therefore
    /// considered a controversial transform — opt in explicitly.
    pub sort_frontmatter: bool,
}

/// What the formatter may do to one source line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    /// Ordinary Brief: leading tabs become two spaces, trailing whitespace
    /// goes, and runs of blank lines collapse to one.
    Brief,
    /// Frontmatter or code-block body: emitted byte for byte.
    Verbatim,
    /// A `+++`, fence, `@code` or `@end` line delimiting a verbatim region:
    /// trailing whitespace goes, nothing else.
    Delimiter,
    /// Row `row` of table `table`, realigned with its siblings.
    TableRow { table: usize, row: usize },
}

pub fn format(source: &str, opts: &Opts) -> String {
    // Strip a leading UTF-8 BOM. fmt produces canonical sources; the BOM
    // is allowed by the lexer but adds no value.
    let source = source.strip_prefix('\u{feff}').unwrap_or(source);
    let lines = split_lines(source);
    if lines.is_empty() {
        return String::new();
    }

    let Some(layout) = Layout::scan(&lines) else {
        // Without tokens there is no telling verbatim lines from the rest,
        // so normalize line endings and touch nothing else.
        let mut out = lines.join("\n");
        out.push('\n');
        return out;
    };

    let tables: Vec<Vec<String>> = layout
        .tables
        .iter()
        .map(|rows| format_table(&rows.iter().map(|&k| lines[k]).collect::<Vec<_>>()))
        .collect();
    let sorted_frontmatter = match &layout.frontmatter {
        Some(body) if opts.sort_frontmatter && !body.is_empty() => {
            sort_frontmatter(&lines[body.clone()]).map(|sorted| (body.clone(), sorted))
        }
        _ => None,
    };

    // Each output line, paired with whether it is a blank Brief line —
    // the only kind blank-run collapsing and edge trimming may remove.
    let mut out: Vec<(String, bool)> = Vec::with_capacity(lines.len());
    let mut k = 0;
    while k < lines.len() {
        if let Some((body, sorted)) = &sorted_frontmatter
            && k == body.start
        {
            out.extend(sorted.iter().map(|l| (l.clone(), false)));
            k = body.end;
            continue;
        }
        let line = lines[k];
        out.push(match layout.kinds[k] {
            Kind::Brief => {
                let t = transform_brief_line(line);
                let blank = t.is_empty();
                (t, blank)
            }
            Kind::Verbatim => (line.to_string(), false),
            Kind::Delimiter => (strip_trailing_ws(line).to_string(), false),
            Kind::TableRow { table, row } => (tables[table][row].clone(), false),
        });
        k += 1;
    }
    finish(out)
}

/// The source's lines without their terminators. The lexer splits on `\n`
/// alone and drops trailing `\r`s, so CRLF becomes LF, but a bare `\r`
/// anywhere else is line content to the compiler and stays.
fn split_lines(source: &str) -> Vec<&str> {
    if source.is_empty() {
        return Vec::new();
    }
    let body = source.strip_suffix('\n').unwrap_or(source);
    body.split('\n').map(|l| l.trim_end_matches('\r')).collect()
}

/// Collapse runs of blank lines, drop blanks from both ends, and join with
/// a single trailing newline.
fn finish(lines: Vec<(String, bool)>) -> String {
    let mut kept: Vec<(String, bool)> = Vec::with_capacity(lines.len());
    for (text, blank) in lines {
        if blank && kept.last().is_some_and(|(_, prev_blank)| *prev_blank) {
            continue;
        }
        kept.push((text, blank));
    }
    let Some(mut start) = kept.iter().position(|(_, blank)| !blank) else {
        return String::new();
    };
    let end = kept
        .iter()
        .rposition(|(_, blank)| !blank)
        .map_or(0, |i| i + 1);
    // Frontmatter must open at byte 0. A `+++` line behind a leading blank
    // is ordinary text; trimming that blank would turn it into frontmatter.
    if start > 0 && kept[start].0 == "+++" {
        start -= 1;
    }
    let mut out = String::with_capacity(kept[start..end].iter().map(|(t, _)| t.len() + 1).sum());
    for (text, _) in &kept[start..end] {
        out.push_str(text);
        out.push('\n');
    }
    out
}

/// The line structure the parser sees in a source.
struct Layout {
    kinds: Vec<Kind>,
    /// Body lines of a closed frontmatter block.
    frontmatter: Option<Range<usize>>,
    /// Line indices of each table's rows, header first.
    tables: Vec<Vec<usize>>,
}

/// A lexed copy of the source, for mapping parser spans back to lines.
struct Scan<'a> {
    src: &'a SourceMap,
    toks: &'a [Token],
    line_starts: &'a [usize],
}

impl Scan<'_> {
    fn line_of(&self, offset: usize) -> usize {
        self.line_starts.partition_point(|&s| s <= offset) - 1
    }

    /// Text and indent of line `k` as the lexer tokenized it. The lexer
    /// emits exactly one token per line, so token `k` is line `k`.
    fn line(&self, k: usize) -> (&str, u16) {
        let tok = &self.toks[k];
        debug_assert_eq!(
            tok.span.start as usize, self.line_starts[k],
            "the lexer no longer emits one token per line"
        );
        (&self.src.source[tok.span.range()], tok.indent)
    }

    /// Whether line `last`, the end of a code block's span, is the line
    /// that closed the block rather than body running to end of file.
    /// These are the parser's own close conditions, so a body line that
    /// met them would have closed the block there.
    fn closes(&self, open: usize, last: usize) -> bool {
        let (open_text, open_indent) = self.line(open);
        let (text, indent) = self.line(last);
        if open_text[open_indent as usize..].starts_with("```") {
            text.trim() == "```"
        } else {
            text.trim() == "@end" && indent == open_indent
        }
    }
}

impl Layout {
    /// Lex and parse `lines` and record what the parser made of each one.
    /// `None` if the lexer refuses the source even with tabs and stray BOMs
    /// blanked out.
    fn scan(lines: &[&str]) -> Option<Layout> {
        // A tab or a BOM past byte 0 is a lex error that leaves no tokens at
        // all, and repairing tab indentation is part of fmt's job. Blank
        // each to spaces of the same byte length so offsets, line numbers
        // and region boundaries all stay where they were.
        let mut text = String::with_capacity(lines.iter().map(|l| l.len() + 1).sum());
        let mut line_starts = Vec::with_capacity(lines.len());
        for line in lines {
            line_starts.push(text.len());
            if line.contains(['\t', '\u{feff}']) {
                for c in line.chars() {
                    match c {
                        '\t' => text.push(' '),
                        '\u{feff}' => text.push_str("   "),
                        c => text.push(c),
                    }
                }
            } else {
                text.push_str(line);
            }
            text.push('\n');
        }
        let src = SourceMap::new("<fmt>", text);
        let toks = lexer::lex(&src).ok()?;
        let scan = Scan {
            src: &src,
            toks: &toks,
            line_starts: &line_starts,
        };

        let mut layout = Layout {
            kinds: vec![Kind::Brief; lines.len()],
            frontmatter: None,
            tables: Vec::new(),
        };
        layout.mark_frontmatter(&scan);
        let (doc, _) = parser::parse(toks.clone(), &src);
        layout.mark_blocks(&doc.blocks, &scan);
        Some(layout)
    }

    /// Frontmatter is consumed before the AST is built, so it is found by
    /// the token test `parser::parse` applies: a `+++` line at byte 0,
    /// closed by the next unindented `+++`. Left open, it runs to the end
    /// of the file.
    fn mark_frontmatter(&mut self, scan: &Scan) {
        let is_fence = |t: &Token| {
            t.kind == TokenKind::Line && t.indent == 0 && &scan.src.source[t.span.range()] == "+++"
        };
        if !scan
            .toks
            .first()
            .is_some_and(|t| t.span.start == 0 && is_fence(t))
        {
            return;
        }
        self.kinds[0] = Kind::Delimiter;
        let close = scan.toks[1..].iter().position(is_fence).map(|i| i + 1);
        let body_end = close.unwrap_or(self.kinds.len());
        for kind in &mut self.kinds[1..body_end] {
            *kind = Kind::Verbatim;
        }
        if let Some(close) = close {
            self.kinds[close] = Kind::Delimiter;
            self.frontmatter = Some(1..close);
        }
    }

    fn mark_blocks(&mut self, blocks: &[Block], scan: &Scan) {
        for block in blocks {
            match block {
                Block::CodeBlock { span, .. } => {
                    let open = scan.line_of(span.start as usize);
                    let last = scan.line_of(span.end());
                    self.kinds[open] = Kind::Delimiter;
                    for kind in &mut self.kinds[open + 1..=last] {
                        *kind = Kind::Verbatim;
                    }
                    if last > open && scan.closes(open, last) {
                        self.kinds[last] = Kind::Delimiter;
                    }
                }
                Block::Table { header, rows, .. } => {
                    let table = self.tables.len();
                    let lines: Vec<usize> = std::iter::once(header)
                        .chain(rows)
                        .map(|r| scan.line_of(r.span.start as usize))
                        .collect();
                    for (row, &k) in lines.iter().enumerate() {
                        self.kinds[k] = Kind::TableRow { table, row };
                    }
                    self.tables.push(lines);
                }
                Block::List { items, .. } => {
                    for item in items {
                        self.mark_blocks(&item.children, scan);
                    }
                }
                Block::Blockquote { children, .. } | Block::BlockShortcode { children, .. } => {
                    self.mark_blocks(children, scan);
                }
                Block::Heading { .. }
                | Block::Paragraph { .. }
                | Block::DefinitionList { .. }
                | Block::HorizontalRule { .. } => {}
            }
        }
    }
}

/// Returns (byte length of leading-whitespace prefix, the prefix as a
/// borrowed slice).
fn leading_indent(line: &str) -> (usize, &str) {
    let n = line
        .bytes()
        .take_while(|b| *b == b' ' || *b == b'\t')
        .count();
    (n, &line[..n])
}

/// Transform a Brief line: tabs in the leading indent become two spaces
/// each, then trailing whitespace is stripped. A whitespace-only line
/// becomes empty; the lexer reads both as blank.
fn transform_brief_line(line: &str) -> String {
    if line.trim().is_empty() {
        return String::new();
    }
    detab_indent(strip_trailing_ws(line))
}

fn strip_trailing_ws(line: &str) -> &str {
    line.trim_end_matches([' ', '\t', '\r'])
}

/// Replace every leading tab with two spaces. Tabs after the first
/// non-whitespace character are left alone — they are content the user
/// presumably typed deliberately, and brief's lexer will surface them as
/// B0102 errors so the user can address them directly.
fn detab_indent(line: &str) -> String {
    let (n, prefix) = leading_indent(line);
    if !prefix.contains('\t') {
        return line.to_string();
    }
    let mut out = prefix.replace('\t', "  ");
    out.push_str(&line[n..]);
    out
}

/// Sort the top-level keys of a frontmatter body. `None` when the body is
/// not valid TOML: the compiler reports that (B0314), and re-emitting it
/// could drop data.
fn sort_frontmatter(body_lines: &[&str]) -> Option<Vec<String>> {
    let body: String = body_lines.join("\n");
    let table: toml::Table = toml::from_str(&body).ok()?;

    // Convert through a BTreeMap to guarantee alphabetical ordering of
    // top-level keys. Subkeys within nested tables are not reordered here
    // beyond what the toml serializer does — this keeps the controversial
    // transform shallow.
    let sorted: BTreeMap<String, toml::Value> = table.into_iter().collect();
    let mut wrap: toml::Table = toml::Table::new();
    for (k, v) in sorted {
        wrap.insert(k, v);
    }
    let serialized = toml::to_string(&wrap).ok()?;

    // Strip the trailing newline `to_string` always appends.
    let trimmed = serialized.trim_end_matches('\n');
    Some(trimmed.split('\n').map(|s| s.to_string()).collect())
}

/// Realign a table so its `|` separators line up. Cells come from
/// `parser::split_cells`, the splitter the parser itself uses, so an
/// escaped `\|` or a pipe inside a code span stays inside its cell.
fn format_table(rows: &[&str]) -> Vec<String> {
    let split: Vec<Option<(String, Vec<&str>)>> = rows
        .iter()
        .map(|line| {
            let line = strip_trailing_ws(line);
            let body = line.trim_start();
            let cells = parser::split_cells(body);
            if cells.unclosed_backtick_at.is_some() {
                return None;
            }
            // Tabs in the indent of a table row → two spaces, same rule as
            // the rest of brief.
            let indent = line[..line.len() - body.len()].replace('\t', "  ");
            Some((indent, cells.cells))
        })
        .collect();

    let mut widths: Vec<usize> = Vec::new();
    for (_, cells) in split.iter().flatten() {
        for (i, cell) in cells.iter().enumerate() {
            let w = cell.chars().count();
            match widths.get_mut(i) {
                Some(max) => *max = (*max).max(w),
                None => widths.push(w),
            }
        }
    }

    rows.iter()
        .zip(&split)
        .map(|(line, split)| match split {
            Some((indent, cells)) => render_row(indent, cells, &widths),
            // The parser drops a row with an unclosed code span (B0208);
            // leave it for the user to fix.
            None => transform_brief_line(line),
        })
        .collect()
}

fn render_row(indent: &str, cells: &[&str], widths: &[usize]) -> String {
    let mut out = String::from(indent);
    out.push('|');
    for (i, cell) in cells.iter().enumerate() {
        if i + 1 < cells.len() {
            out.push(' ');
            out.push_str(cell);
            let pad = widths[i].saturating_sub(cell.chars().count());
            out.extend(std::iter::repeat_n(' ', pad));
            out.push_str(" |");
        } else if !cell.is_empty() {
            // Last cell: no trailing pad and no trailing `|` — the brief
            // table convention.
            out.push(' ');
            out.push_str(cell);
        } else if i > 0 {
            // An empty last cell keeps its closing `|`: without one,
            // split_cells reads the row as a column short.
            out.push_str(" |");
        }
    }
    out
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CheckResult {
    Unchanged,
    WouldChange,
}

pub fn check(source: &str, opts: &Opts) -> CheckResult {
    if format(source, opts) == source {
        CheckResult::Unchanged
    } else {
        CheckResult::WouldChange
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fmt(s: &str) -> String {
        format(s, &Opts::default())
    }

    #[test]
    fn empty_input_stays_empty() {
        assert_eq!(fmt(""), "");
    }

    #[test]
    fn single_line_gains_trailing_newline() {
        assert_eq!(fmt("hello"), "hello\n");
    }

    #[test]
    fn strips_trailing_whitespace() {
        assert_eq!(fmt("hello   \nworld\t \n"), "hello\nworld\n");
    }

    #[test]
    fn collapses_runs_of_blank_lines() {
        assert_eq!(fmt("a\n\n\n\nb\n"), "a\n\nb\n");
    }

    #[test]
    fn trims_leading_and_trailing_blank_lines() {
        assert_eq!(fmt("\n\nhello\n\n\n"), "hello\n");
    }

    #[test]
    fn normalizes_crlf_to_lf() {
        assert_eq!(fmt("a\r\nb\r\n"), "a\nb\n");
    }

    #[test]
    fn bare_cr_is_content_not_a_line_break() {
        // The lexer splits lines on `\n` only, so `a\rb` is one line to the
        // compiler; turning the `\r` into `\n` would change the document.
        // A `\r` at the end of a line is CRLF residue and goes.
        assert_eq!(fmt("a\rb\r"), "a\rb\n");
        assert_eq!(fmt("# a\rb\r\r\nc\r\n"), "# a\rb\nc\n");
    }

    #[test]
    fn strips_leading_bom() {
        assert_eq!(fmt("\u{feff}hello\n"), "hello\n");
    }

    #[test]
    fn replaces_leading_tabs_with_two_spaces() {
        assert_eq!(fmt("\t- item\n"), "  - item\n");
        assert_eq!(fmt("\t\t- item\n"), "    - item\n");
    }

    #[test]
    fn preserves_emphasis_markers_verbatim() {
        let src = "*bold* and _underline_ and /italic/ and ~strike~\n";
        assert_eq!(fmt(src), src);
    }

    #[test]
    fn preserves_inline_shortcode_arg_order() {
        let src = "see @link(href: \"x\", title: \"Y\")\n";
        assert_eq!(fmt(src), src);
    }

    #[test]
    fn preserves_code_fence_body_verbatim() {
        let src = "```rust\n  fn  x  (  )  {  }   \n```\n";
        // Body line preserved including its weird interior spacing AND its
        // trailing whitespace (lexer strips trailing ws anyway, so this
        // stays as-is).
        let out = fmt(src);
        assert!(out.contains("  fn  x  (  )  {  }   "));
    }

    #[test]
    fn aligns_table_columns() {
        let src = "@t\n| Header | B\n| longcell | y\n| z | other\n";
        let out = fmt(src);
        let expected = "\
@t
| Header   | B
| longcell | y
| z        | other
";
        assert_eq!(out, expected);
    }

    #[test]
    fn table_alignment_handles_indent() {
        let src = "  @t\n  | A | B\n  | longer | y\n";
        let out = fmt(src);
        let expected = "  @t\n  | A      | B\n  | longer | y\n";
        assert_eq!(out, expected);
    }

    #[test]
    fn table_with_args() {
        let src = "@t(align: [\"left\", \"right\"])\n| A | B\n| 1 | 22\n";
        let out = fmt(src);
        let expected = "@t(align: [\"left\", \"right\"])\n| A | B\n| 1 | 22\n";
        assert_eq!(out, expected);
    }

    #[test]
    fn frontmatter_passthrough_by_default() {
        let src = "+++\nz = 1\na = 2\n+++\n# Doc\n";
        assert_eq!(fmt(src), src);
    }

    #[test]
    fn frontmatter_sort_when_opted_in() {
        let opts = Opts {
            sort_frontmatter: true,
        };
        let src = "+++\nz = 1\na = 2\n+++\n# Doc\n";
        let out = format(src, &opts);
        // Sorted: a then z.
        let header_end = out.find("+++\n").unwrap() + 4;
        let between = &out[header_end..];
        let close = between.find("+++").unwrap();
        let body = &between[..close];
        let a_pos = body.find("a = ").unwrap();
        let z_pos = body.find("z = ").unwrap();
        assert!(a_pos < z_pos, "frontmatter not sorted: {:?}", body);
    }

    #[test]
    fn frontmatter_with_invalid_toml_unchanged_body() {
        let opts = Opts {
            sort_frontmatter: true,
        };
        let src = "+++\nfoo === 1\n+++\n";
        let out = format(src, &opts);
        // Bad TOML: body preserved (don't drop data).
        assert!(out.contains("foo === 1"));
    }

    #[test]
    fn frontmatter_unterminated_left_alone() {
        // Without a closing +++, fmt treats the opening line as Default
        // content so the parse error is still visible at compile time.
        let src = "+++\nfoo = 1\n";
        let out = fmt(src);
        assert!(out.contains("+++"));
        assert!(out.contains("foo = 1"));
    }

    #[test]
    fn block_comment_body_preserved() {
        let src = "/*\n  multi\n  line\n*/\nbody\n";
        let out = fmt(src);
        assert!(out.contains("  multi"));
        assert!(out.contains("  line"));
    }

    #[test]
    fn does_not_collapse_blank_lines_inside_code_fence() {
        let src = "```rust\nfn x() {\n\n\n}\n```\n";
        let out = fmt(src);
        // Three newlines between `{` and `}` (i.e. two blank lines).
        assert!(out.contains("fn x() {\n\n\n}"));
    }

    #[test]
    fn does_not_reorder_paragraphs() {
        let src = "third\n\nfirst\n\nsecond\n";
        assert_eq!(fmt(src), src);
    }

    #[test]
    fn idempotent_on_already_formatted() {
        let src = "# Hello\n\nThis is a paragraph.\n\n@t\n| A | B\n| 1 | 2\n";
        let once = fmt(src);
        let twice = fmt(&once);
        assert_eq!(once, twice);
    }

    #[test]
    fn idempotent_on_unformatted() {
        let inputs = [
            "  # heading   \n\n\n\nbody\n",
            "@t\n| a | b\n| longercell | y\n",
            "+++\nz = 1\na = 2\n+++\n# x\n",
            "```rust\n  fn x() {}\n```\n",
            "/*\n comment\n*/\n# heading\n",
            "\thello\n\t\tworld\n",
            "",
        ];
        for src in &inputs {
            let once = fmt(src);
            let twice = fmt(&once);
            assert_eq!(once, twice, "not idempotent for: {:?}", src);
        }
    }

    #[test]
    fn idempotent_with_sort_frontmatter() {
        let opts = Opts {
            sort_frontmatter: true,
        };
        let src = "+++\nz = 1\na = 2\nm = \"x\"\n+++\nbody\n";
        let once = format(src, &opts);
        let twice = format(&once, &opts);
        assert_eq!(once, twice);
    }

    #[test]
    fn check_returns_unchanged_for_canonical_input() {
        let src = "hello\n";
        assert_eq!(check(src, &Opts::default()), CheckResult::Unchanged);
    }

    #[test]
    fn check_returns_would_change_for_dirty_input() {
        let src = "hello   \n";
        assert_eq!(check(src, &Opts::default()), CheckResult::WouldChange);
    }

    #[test]
    fn nested_list_indentation_preserved() {
        let src = "- top\n  - nested\n    - deeper\n";
        assert_eq!(fmt(src), src);
    }

    #[test]
    fn hard_break_backslash_preserved_after_trailing_ws_strip() {
        // Trailing spaces after the `\` are stripped; the `\` itself stays.
        let src = "line one \\   \nline two\n";
        let out = fmt(src);
        assert_eq!(out, "line one \\\nline two\n");
    }

    #[test]
    fn empty_table_row_handled() {
        // A single empty cell renders as a bare `|`, with no trailing space.
        assert_eq!(fmt("@t\n|  \n"), "@t\n|\n");
    }

    #[test]
    fn unicode_in_table_cells_aligns_by_codepoint() {
        // Codepoint-based alignment; users with East Asian wide chars will
        // still see slight visual misalignment in monospace fonts. Documented.
        let src = "@t\n| α | b\n| longer | y\n";
        let out = fmt(src);
        assert!(out.contains("| α      | b"));
        assert!(out.contains("| longer | y"));
    }

    #[test]
    fn comment_lines_stay_in_place() {
        let src = "// a comment\n# heading\n";
        // Single-line `//` comments are not classified as a region (they're
        // Default content); they pass through unchanged.
        assert_eq!(fmt(src), src);
    }

    #[test]
    fn block_shortcode_unchanged() {
        let src = "@callout(kind: warning)\nhello\n@end\n";
        assert_eq!(fmt(src), src);
    }

    #[test]
    fn fmt_idempotent_on_dl_example() {
        let src = "@dl\nTerm 1\n: Definition of term 1.\nTerm 2\n: Definition of term 2.\n@end\n";
        let once = fmt(src);
        let twice = fmt(&once);
        assert_eq!(once, twice, "fmt is not idempotent on @dl");
        // Canonical input shouldn't be rewritten.
        assert_eq!(once, src, "fmt rewrote canonical @dl source");
    }

    #[test]
    fn table_escaped_pipe_stays_inside_its_cell() {
        // FMT-01: splitting on every `|` turned `a \| b` into two cells and
        // the row into a B0502 column mismatch.
        let src = "@t\n| a \\| b | c\n| d | e\n";
        assert_eq!(fmt(src), "@t\n| a \\| b | c\n| d      | e\n");
    }

    #[test]
    fn table_pipe_inside_code_span_stays_inside_its_cell() {
        let src = "@t\n| `a|b` | c\n| d | e\n| ``x|y`` | f\n";
        let expected = "@t\n| `a|b`   | c\n| d       | e\n| ``x|y`` | f\n";
        assert_eq!(fmt(src), expected);
    }

    #[test]
    fn table_empty_last_cell_keeps_its_closing_pipe() {
        // `| a | |` is two cells; dropping the final `|` would leave one.
        let src = "@t\n| a |  |\n| bb | c\n";
        assert_eq!(fmt(src), "@t\n| a  | |\n| bb | c\n");
    }

    #[test]
    fn table_trailing_pipe_dropped_and_ragged_rows_padded() {
        let src = "@t\n| a | b |\n| ccc |\n";
        assert_eq!(fmt(src), "@t\n| a   | b\n| ccc\n");
    }

    #[test]
    fn table_row_with_unclosed_code_span_left_alone() {
        // The parser drops this row (B0208); it takes no part in alignment.
        let src = "@t\n| a | b\n| `x | y   \n| longer | c\n";
        assert_eq!(fmt(src), "@t\n| a      | b\n| `x | y\n| longer | c\n");
    }

    #[test]
    fn code_shortcode_body_is_verbatim() {
        // FMT-02: blank runs, tables and trailing whitespace inside `@code`
        // are code, not Brief.
        let src = "@code(brief)\n@t\n| a | b\n| longer | c\n\n\n\nx   \n@end\n";
        assert_eq!(fmt(src), src);
    }

    #[test]
    fn lone_fence_inside_code_shortcode_does_not_open_a_fence() {
        // A ``` line inside `@code` is body text. Reading it as a fence
        // opener used to flip every later region inside out.
        let src = "@code\n```\n@end\n\n\n\npara   \n\n```\nkeep   \n\n\n```\n";
        let expected = "@code\n```\n@end\n\npara\n\n```\nkeep   \n\n\n```\n";
        assert_eq!(fmt(src), expected);
    }

    #[test]
    fn code_shortcode_delimiters_lose_trailing_whitespace_only() {
        let src = "  @code(rust)   \n  let x = 1;  \n  @end  \n";
        assert_eq!(fmt(src), "  @code(rust)\n  let x = 1;  \n  @end\n");
    }

    #[test]
    fn code_shortcode_end_at_other_indent_is_body() {
        // `@end` closes `@code` only at the opener's indent.
        let src = "@code\n  @end   \n@end\n";
        assert_eq!(fmt(src), src);
    }

    #[test]
    fn tabs_inside_code_bodies_are_not_expanded() {
        let src = "```make\nall:\n\tcc -o x x.c\t\n```\n@code\n\tx\n@end\n";
        assert_eq!(fmt(src), src);
    }

    #[test]
    fn tab_indent_outside_code_is_expanded() {
        let src = "- a\n\t- b\t\n```\n\tbody\n```\n";
        assert_eq!(fmt(src), "- a\n  - b\n```\n\tbody\n```\n");
    }

    #[test]
    fn code_fence_inside_list_item_is_verbatim() {
        let src = "- item\n\n  ```\n  a   \n\n\n  ```\n";
        assert_eq!(fmt(src), src);
    }

    #[test]
    fn code_fence_delimiters_lose_trailing_whitespace() {
        assert_eq!(fmt("```rust   \nx\n```   \n"), "```rust\nx\n```\n");
    }

    #[test]
    fn unterminated_code_fence_runs_to_eof_untouched() {
        // The parser reads everything after an unclosed fence as its body,
        // trailing blank lines included.
        let src = "para  \n```\ncode   \n\n\n";
        assert_eq!(fmt(src), "para\n```\ncode   \n\n\n");
    }

    #[test]
    fn fence_lookalikes_inside_block_comment_do_not_open_regions() {
        let src = "/*\n```\n*/\npara   \n";
        assert_eq!(fmt(src), "/*\n```\n*/\npara\n");
    }

    #[test]
    fn frontmatter_body_is_verbatim() {
        let src = "+++   \ntitle = \"\"\"\nkeep   \n\n\n\"\"\"\n+++\nbody   \n";
        let expected = "+++\ntitle = \"\"\"\nkeep   \n\n\n\"\"\"\n+++\nbody\n";
        assert_eq!(fmt(src), expected);
    }

    #[test]
    fn plus_line_behind_leading_blank_is_not_promoted_to_frontmatter() {
        // Frontmatter must start at byte 0; trimming the blank would make
        // the parser read these lines as TOML.
        assert_eq!(fmt("\n\n+++\nx\n+++\n"), "\n+++\nx\n+++\n");
        assert_eq!(fmt("\n\nhello\n"), "hello\n");
    }

    #[test]
    fn idempotent_on_adversarial_inputs() {
        let inputs = [
            "@t\n| a \\| b | c\n| d | e\n",
            "@code(brief)\n@t\n| a | b\n\n\n@end\n",
            "@code\n```\n@end\n\n\n\npara   \n",
            "\n\n+++\nx\n+++\n",
            "```\ncode   \n\n\n",
            "@t\n| a |  |\n| bb | c\n",
            "\t```\n\tx\n\t```\n",
            "a\r\r\nb\r",
        ];
        for src in &inputs {
            let once = fmt(src);
            let twice = fmt(&once);
            assert_eq!(once, twice, "not idempotent for: {:?}", src);
        }
    }
}
