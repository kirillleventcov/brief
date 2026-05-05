//! `brief fmt` — gofmt-doctrine formatter for Brief sources.
//!
//! Operates on the raw source as a sequence of lines. A region scanner
//! identifies inviolate regions (frontmatter, code fences, block comments,
//! tables) so per-line transforms never touch their internals. Emphasis
//! markers, shortcode argument order, and inline content are preserved
//! verbatim — the formatter never re-emits parsed AST.

use std::collections::BTreeMap;

#[derive(Debug, Clone, Default)]
pub struct Opts {
    /// Sort top-level keys of frontmatter alphabetically. Off by default
    /// because re-emitting TOML loses comment positions and is therefore
    /// considered a controversial transform — opt in explicitly.
    pub sort_frontmatter: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Region {
    Default,
    FrontmatterOpen,
    FrontmatterBody,
    FrontmatterClose,
    CodeFenceOpen,
    CodeFenceBody,
    CodeFenceClose,
    BlockComment,
    TableDirective,
    TableRow,
}

impl Region {
    /// True if blank lines in this region count as collapsible whitespace
    /// for the global blank-collapse pass. Blanks inside structural regions
    /// (code fences, comments, frontmatter) are part of the body and stay.
    fn collapsible(self) -> bool {
        matches!(self, Region::Default)
    }
}

pub fn format(source: &str, opts: &Opts) -> String {
    // Strip a leading UTF-8 BOM. fmt produces canonical sources; the BOM
    // is allowed by the lexer but adds no value.
    let source = source.strip_prefix('\u{feff}').unwrap_or(source);

    if source.is_empty() {
        return String::new();
    }

    // Normalize line endings: CRLF and bare CR → LF. Brief is LF-only.
    let normalized = normalize_line_endings(source);

    // Split into lines without trailing newline information; we re-emit
    // with a single trailing `\n`.
    let raw_lines: Vec<&str> = normalized.split('\n').collect();

    // The final trailing element of split('\n') is always "" when the input
    // ends with `\n`. Drop it so we don't carry a phantom blank line.
    let lines: Vec<&str> = if raw_lines.last() == Some(&"") {
        raw_lines[..raw_lines.len() - 1].to_vec()
    } else {
        raw_lines
    };

    if lines.is_empty() {
        return String::new();
    }

    let regions = scan_regions(&lines);
    debug_assert_eq!(regions.len(), lines.len());

    let transformed = apply_transforms(&lines, &regions, opts);
    let collapsed = collapse_blanks(&transformed.lines, &transformed.regions);
    let trimmed = trim_blank_edges(&collapsed.lines, &collapsed.regions);

    if trimmed.is_empty() {
        return String::new();
    }
    let mut out = trimmed.join("\n");
    out.push('\n');
    out
}

fn normalize_line_endings(s: &str) -> String {
    // Two-step: CRLF → LF, then bare CR → LF.
    let step1 = s.replace("\r\n", "\n");
    step1.replace('\r', "\n")
}

fn scan_regions(lines: &[&str]) -> Vec<Region> {
    let mut regions = vec![Region::Default; lines.len()];
    let mut i = 0;

    // Frontmatter is only legal at the very top of the document.
    if !lines.is_empty() && lines[0].trim_end() == "+++" {
        let mut close_idx = None;
        for j in 1..lines.len() {
            if lines[j].trim_end() == "+++" {
                close_idx = Some(j);
                break;
            }
        }
        if let Some(j) = close_idx {
            regions[0] = Region::FrontmatterOpen;
            for k in 1..j {
                regions[k] = Region::FrontmatterBody;
            }
            regions[j] = Region::FrontmatterClose;
            i = j + 1;
        }
        // If unterminated, leave as Default — the parser will raise B0313 on
        // the next compile and we shouldn't make it harder to spot.
    }

    while i < lines.len() {
        let line = lines[i];
        let (indent_len, _) = leading_indent(line);
        let trimmed = &line[indent_len..];
        let trimmed = trimmed.trim_end_matches(|c: char| c == ' ' || c == '\t');

        if trimmed.starts_with("```") {
            regions[i] = Region::CodeFenceOpen;
            i += 1;
            while i < lines.len() {
                let inner = lines[i];
                let (i_indent, _) = leading_indent(inner);
                let inner_trim =
                    inner[i_indent..].trim_end_matches(|c: char| c == ' ' || c == '\t');
                if inner_trim == "```" {
                    regions[i] = Region::CodeFenceClose;
                    i += 1;
                    break;
                }
                regions[i] = Region::CodeFenceBody;
                i += 1;
            }
            continue;
        }

        if trimmed.starts_with("/*") {
            regions[i] = Region::BlockComment;
            // Single-line `/* ... */` — closes on the same line.
            if trimmed.ends_with("*/") && trimmed.len() >= 4 {
                i += 1;
                continue;
            }
            i += 1;
            while i < lines.len() {
                regions[i] = Region::BlockComment;
                let inner = lines[i].trim_end_matches(|c: char| c == ' ' || c == '\t');
                if inner.ends_with("*/") {
                    i += 1;
                    break;
                }
                i += 1;
            }
            continue;
        }

        if is_table_directive(trimmed) {
            regions[i] = Region::TableDirective;
            i += 1;
            while i < lines.len() {
                let (j_indent, _) = leading_indent(lines[i]);
                let body = &lines[i][j_indent..];
                if body.starts_with('|') {
                    regions[i] = Region::TableRow;
                    i += 1;
                } else {
                    break;
                }
            }
            continue;
        }

        i += 1;
    }

    regions
}

fn is_table_directive(trimmed: &str) -> bool {
    trimmed == "@t" || trimmed.starts_with("@t ") || trimmed.starts_with("@t(")
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

struct PassResult {
    lines: Vec<String>,
    regions: Vec<Region>,
}

fn apply_transforms(lines: &[&str], regions: &[Region], opts: &Opts) -> PassResult {
    let mut out_lines: Vec<String> = Vec::with_capacity(lines.len());
    let mut out_regions: Vec<Region> = Vec::with_capacity(lines.len());

    let mut i = 0;
    while i < lines.len() {
        match regions[i] {
            Region::Default => {
                out_lines.push(transform_brief_line(lines[i]));
                out_regions.push(Region::Default);
                i += 1;
            }
            Region::FrontmatterOpen => {
                out_lines.push("+++".to_string());
                out_regions.push(Region::FrontmatterOpen);
                i += 1;
            }
            Region::FrontmatterClose => {
                out_lines.push("+++".to_string());
                out_regions.push(Region::FrontmatterClose);
                i += 1;
            }
            Region::FrontmatterBody => {
                let start = i;
                while i < lines.len() && regions[i] == Region::FrontmatterBody {
                    i += 1;
                }
                let body_slice = &lines[start..i];
                let processed = process_frontmatter_body(body_slice, opts);
                for l in processed {
                    out_lines.push(l);
                    out_regions.push(Region::FrontmatterBody);
                }
            }
            Region::CodeFenceOpen => {
                // Strip trailing whitespace from the fence line; never touch
                // the language tag or attributes.
                out_lines.push(strip_trailing_ws(lines[i]).to_string());
                out_regions.push(Region::CodeFenceOpen);
                i += 1;
            }
            Region::CodeFenceClose => {
                out_lines.push(strip_trailing_ws(lines[i]).to_string());
                out_regions.push(Region::CodeFenceClose);
                i += 1;
            }
            Region::CodeFenceBody => {
                // Verbatim: never alter code-block contents. The lexer
                // strips trailing whitespace before tokenizing so we don't
                // need to either.
                out_lines.push(lines[i].to_string());
                out_regions.push(Region::CodeFenceBody);
                i += 1;
            }
            Region::BlockComment => {
                // Strip trailing whitespace; otherwise verbatim.
                out_lines.push(strip_trailing_ws(lines[i]).to_string());
                out_regions.push(Region::BlockComment);
                i += 1;
            }
            Region::TableDirective => {
                out_lines.push(transform_brief_line(lines[i]));
                out_regions.push(Region::TableDirective);
                i += 1;
            }
            Region::TableRow => {
                let start = i;
                while i < lines.len() && regions[i] == Region::TableRow {
                    i += 1;
                }
                let formatted = format_table(&lines[start..i]);
                for l in formatted {
                    out_lines.push(l);
                    out_regions.push(Region::TableRow);
                }
            }
        }
    }

    PassResult {
        lines: out_lines,
        regions: out_regions,
    }
}

/// Transform a Brief region line: tabs in the leading indent become two
/// spaces each, then trailing whitespace is stripped.
fn transform_brief_line(line: &str) -> String {
    let stripped = strip_trailing_ws(line);
    detab_indent(stripped)
}

fn strip_trailing_ws(line: &str) -> &str {
    line.trim_end_matches(|c: char| c == ' ' || c == '\t' || c == '\r')
}

/// Replace every leading tab with two spaces. Tabs after the first
/// non-whitespace character are left alone — they are content the user
/// presumably typed deliberately, and brief's lexer will surface them as
/// B0102 errors so the user can address them directly.
fn detab_indent(line: &str) -> String {
    let (n, _) = leading_indent(line);
    if n == 0 {
        return line.to_string();
    }
    let prefix = &line[..n];
    let rest = &line[n..];
    let mut out = String::with_capacity(line.len() + 4);
    for c in prefix.chars() {
        if c == '\t' {
            out.push(' ');
            out.push(' ');
        } else {
            out.push(c);
        }
    }
    out.push_str(rest);
    out
}

fn process_frontmatter_body(body_lines: &[&str], opts: &Opts) -> Vec<String> {
    if !opts.sort_frontmatter {
        return body_lines
            .iter()
            .map(|l| strip_trailing_ws(l).to_string())
            .collect();
    }

    let body: String = body_lines.join("\n");
    let parsed: Result<toml::Table, _> = toml::from_str(&body);
    let table = match parsed {
        Ok(t) => t,
        Err(_) => {
            // Bad TOML: don't risk dropping data. Leave the body alone (the
            // compiler will surface B0314 on next compile).
            return body_lines
                .iter()
                .map(|l| strip_trailing_ws(l).to_string())
                .collect();
        }
    };

    // Convert through a BTreeMap to guarantee alphabetical ordering of
    // top-level keys. Subkeys within nested tables are not reordered here
    // beyond what the toml serializer does — this keeps the controversial
    // transform shallow.
    let sorted: BTreeMap<String, toml::Value> = table.into_iter().collect();
    let mut wrap: toml::Table = toml::Table::new();
    for (k, v) in sorted {
        wrap.insert(k, v);
    }
    let serialized = match toml::to_string(&wrap) {
        Ok(s) => s,
        Err(_) => {
            return body_lines
                .iter()
                .map(|l| strip_trailing_ws(l).to_string())
                .collect();
        }
    };

    // Strip the trailing newline `to_string` always appends.
    let trimmed = serialized.trim_end_matches('\n');
    trimmed.split('\n').map(|s| s.to_string()).collect()
}

fn format_table(rows: &[&str]) -> Vec<String> {
    if rows.is_empty() {
        return Vec::new();
    }

    struct Parsed {
        indent: String,
        cells: Vec<String>,
    }

    let parsed: Vec<Parsed> = rows
        .iter()
        .map(|line| {
            let stripped = strip_trailing_ws(line);
            let (n, _) = leading_indent(stripped);
            let indent_raw = &stripped[..n];
            // Tabs in the indent of a table row → two spaces, same rule as
            // the rest of brief.
            let indent: String = indent_raw
                .chars()
                .flat_map(|c| {
                    if c == '\t' {
                        vec![' ', ' '].into_iter()
                    } else {
                        vec![c].into_iter()
                    }
                })
                .collect();
            let body = &stripped[n..];
            let cells = parse_table_cells(body);
            Parsed { indent, cells }
        })
        .collect();

    let max_cols = parsed.iter().map(|p| p.cells.len()).max().unwrap_or(0);
    if max_cols == 0 {
        return parsed.iter().map(|p| format!("{}|", p.indent)).collect();
    }

    let mut widths = vec![0usize; max_cols];
    for p in &parsed {
        for (i, cell) in p.cells.iter().enumerate() {
            let w = cell.chars().count();
            if w > widths[i] {
                widths[i] = w;
            }
        }
    }

    parsed
        .iter()
        .map(|p| {
            let mut out = p.indent.clone();
            out.push('|');
            for (idx, cell) in p.cells.iter().enumerate() {
                out.push(' ');
                out.push_str(cell);
                let pad = widths[idx].saturating_sub(cell.chars().count());
                if idx + 1 < p.cells.len() {
                    for _ in 0..pad {
                        out.push(' ');
                    }
                    out.push(' ');
                    out.push('|');
                }
                // Last cell: no trailing pad and no trailing `|` — matches
                // brief table convention (cf. parser::split_cells, which
                // tolerates either form on input).
            }
            out
        })
        .collect()
}

fn parse_table_cells(body: &str) -> Vec<String> {
    // Mirrors parser::split_cells: tolerate optional trailing `|`, strip
    // leading `|`, split on `|`, trim each cell.
    let trimmed = body.trim_end_matches(|c: char| c == ' ' || c == '\t');
    let trimmed = trimmed.trim_end_matches('|');
    let inner = if let Some(rest) = trimmed.strip_prefix('|') {
        rest
    } else {
        trimmed
    };
    inner.split('|').map(|s| s.trim().to_string()).collect()
}

fn collapse_blanks(lines: &[String], regions: &[Region]) -> PassResult {
    let mut out_lines = Vec::with_capacity(lines.len());
    let mut out_regions = Vec::with_capacity(lines.len());
    let mut prev_was_blank = false;
    for (line, region) in lines.iter().zip(regions.iter()) {
        let is_blank = line.trim().is_empty();
        if is_blank && region.collapsible() {
            if prev_was_blank {
                continue;
            }
            prev_was_blank = true;
            // Canonical blank line: literally empty.
            out_lines.push(String::new());
            out_regions.push(*region);
        } else {
            prev_was_blank = false;
            out_lines.push(line.clone());
            out_regions.push(*region);
        }
    }
    PassResult {
        lines: out_lines,
        regions: out_regions,
    }
}

fn trim_blank_edges(lines: &[String], regions: &[Region]) -> Vec<String> {
    let mut start = 0;
    while start < lines.len() && lines[start].trim().is_empty() && regions[start].collapsible() {
        start += 1;
    }
    let mut end = lines.len();
    while end > start && lines[end - 1].trim().is_empty() && regions[end - 1].collapsible() {
        end -= 1;
    }
    lines[start..end].to_vec()
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
    fn normalizes_bare_cr_to_lf() {
        assert_eq!(fmt("a\rb\r"), "a\nb\n");
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
        let src = "@t\n|\n";
        let out = fmt(src);
        // Single empty cell: `|` followed by ` ` then nothing (last cell),
        // so we render `| ` — but since the cell is empty and is the last
        // column, no trailing pad. `|` alone with one space.
        assert!(out.starts_with("@t\n|"));
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
}
