//! Project-aware analysis shared by every LSP handler.
//!
//! Buffers go through the same `Project::check` front-end as `brief
//! compile`, in the same project `Project::discover` finds for the CLI, so
//! in-editor diagnostics never diverge from what the compiler reports. The
//! one addition is the dirty-buffer overlay: open buffers stand in for their
//! files in the `@ref` index. Everything here is pure with respect to the
//! LSP transport (no `Client`), which is what makes it testable.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use brief::{
    ast::{Block, Document, Inline},
    config::Config,
    diag::{Diagnostic, Severity},
    lexer, parser,
    project::{self, Project, ProjectIndex},
    span::{SourceMap, Span},
};

pub use brief::project::{RefSite, collect_refs};

// ---------------------------------------------------------------------------
// Project snapshot
// ---------------------------------------------------------------------------

/// Cached per-root compile context: the project (config and shortcode
/// registry) and its disk-built `@ref` index. Rebuilt on file open and
/// save, reused across keystrokes.
#[derive(Debug)]
pub struct ProjectSnapshot {
    /// The directory containing `brief.toml`.
    pub root: PathBuf,
    pub project: Project,
    pub index: ProjectIndex,
    /// Set when `<root>/brief.toml` fails to load. Analysis falls back to
    /// the default config so diagnostics keep flowing; `brief compile`
    /// would exit 2 with this same message, so the backend surfaces it as
    /// an error diagnostic on every document under the root.
    pub config_error: Option<String>,
}

/// Build a snapshot for a known project root (a directory that contains
/// `brief.toml`). Index diagnostics from other files are dropped here: the
/// editor shows those when their own file is open, and a `@ref` into a
/// broken file reports B0605 through the index's `broken` set.
pub fn load_snapshot(root: &Path) -> ProjectSnapshot {
    let (project, config_error) = match Project::discover(root, None) {
        Ok(p) => (p, None),
        Err(e) => (
            Project::from_config(Some(root.to_path_buf()), None, Config::default()),
            Some(e),
        ),
    };
    let index = project::build_index(root).0;
    ProjectSnapshot {
        root: root.to_path_buf(),
        project,
        index,
        config_error,
    }
}

/// Project-relative forward-slash path for `doc_path` under `root`, in the
/// same form `build_index` uses as keys. `None` when the file is outside
/// the root.
pub fn rel_path(root: &Path, doc_path: &Path) -> Option<String> {
    let canon = doc_path
        .canonicalize()
        .unwrap_or_else(|_| doc_path.to_path_buf());
    canon.strip_prefix(root).ok().map(|p| {
        p.components()
            .map(|c| c.as_os_str().to_string_lossy().into_owned())
            .collect::<Vec<_>>()
            .join("/")
    })
}

// ---------------------------------------------------------------------------
// Buffer analysis
// ---------------------------------------------------------------------------

/// What an open buffer contributes to the `@ref` index in place of its
/// copy on disk, judged exactly as `build_index` judges a file.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct BufferEntry {
    /// Heading anchors declared by the buffer.
    pub anchors: BTreeSet<String>,
    /// The buffer fails to lex or parse, so a `@ref` into it is B0605.
    pub broken: bool,
}

impl BufferEntry {
    fn of(src: &SourceMap) -> BufferEntry {
        match lexer::lex(src) {
            Err(_) => BufferEntry {
                anchors: BTreeSet::new(),
                broken: true,
            },
            Ok(tokens) => {
                let (doc, diags) = parser::parse(tokens, src);
                BufferEntry {
                    anchors: project::collect_anchors(&doc),
                    broken: diags.iter().any(|d| d.severity == Severity::Error),
                }
            }
        }
    }
}

/// Result of running the full pipeline over one buffer.
pub struct Analysis {
    /// `None` when the buffer failed to lex.
    pub doc: Option<Document>,
    pub diagnostics: Vec<Diagnostic>,
    /// This buffer's own index entry. The backend caches it per open
    /// document to overlay unsaved edits over the disk index.
    pub entry: BufferEntry,
}

/// Run the `brief compile` front-end over an in-memory buffer.
///
/// `overlay` maps project-relative paths of *other* open buffers to their
/// current index entries; they take precedence over the disk-built index
/// (anchors and broken state both) so unsaved edits are seen immediately.
/// The current buffer's own entry is always overlaid last, so
/// self-references resolve against what is on screen, not what is on disk.
pub fn analyze(
    doc_path: &Path,
    text: &str,
    snapshot: Option<&ProjectSnapshot>,
    overlay: &BTreeMap<String, BufferEntry>,
) -> Analysis {
    let src = SourceMap::new(doc_path.to_string_lossy(), text.to_string());
    let entry = BufferEntry::of(&src);

    let checked = match snapshot {
        Some(s) => {
            let rel = rel_path(&s.root, doc_path);
            let mut index = s.index.clone();
            let own = rel.as_ref().map(|r| (r, &entry));
            for (p, e) in overlay.iter().chain(own) {
                index.anchors.insert(p.clone(), e.anchors.clone());
                if e.broken {
                    index.broken.insert(p.clone());
                } else {
                    index.broken.remove(p);
                }
            }
            s.project.check(&src, Some(&index), rel.as_deref())
        }
        // No `brief.toml` above the file: same as `brief compile` outside
        // a project — builtins only, `@ref` errors with B0604.
        None => Project::standalone().check(&src, None, None),
    };

    Analysis {
        doc: checked.doc,
        diagnostics: checked.diagnostics,
        entry,
    }
}

// ---------------------------------------------------------------------------
// AST walks: headings and @ref sites
// ---------------------------------------------------------------------------

/// A heading found in document order.
pub struct HeadingInfo {
    pub level: u8,
    pub text: String,
    pub anchor: Option<String>,
    pub span: Span,
}

pub fn collect_headings(doc: &Document) -> Vec<HeadingInfo> {
    let mut out = Vec::new();
    for b in &doc.blocks {
        collect_headings_block(b, &mut out);
    }
    out
}

fn collect_headings_block(b: &Block, out: &mut Vec<HeadingInfo>) {
    match b {
        Block::Heading {
            level,
            content,
            anchor,
            span,
        } => {
            out.push(HeadingInfo {
                level: *level,
                text: inline_text(content),
                anchor: anchor.clone(),
                span: *span,
            });
        }
        Block::Blockquote { children, .. } | Block::BlockShortcode { children, .. } => {
            for c in children {
                collect_headings_block(c, out);
            }
        }
        Block::List { items, .. } => {
            for it in items {
                for c in &it.children {
                    collect_headings_block(c, out);
                }
            }
        }
        _ => {}
    }
}

/// Flatten inline content to plain text (for symbol names).
pub fn inline_text(nodes: &[Inline]) -> String {
    let mut out = String::new();
    push_inline_text(nodes, &mut out);
    out
}

fn push_inline_text(nodes: &[Inline], out: &mut String) {
    for n in nodes {
        match n {
            Inline::Text { value, .. } | Inline::InlineCode { value, .. } => out.push_str(value),
            Inline::Bold { content, .. }
            | Inline::Italic { content, .. }
            | Inline::Underline { content, .. }
            | Inline::Strike { content, .. } => push_inline_text(content, out),
            Inline::HardBreak { .. } => out.push(' '),
            Inline::Shortcode { content, .. } => {
                if let Some(c) = content {
                    push_inline_text(c, out);
                }
            }
        }
    }
}

/// The `@ref` whose span contains `byte`, if any.
pub fn ref_at(doc: &Document, byte: usize) -> Option<RefSite> {
    collect_refs(doc)
        .into_iter()
        .find(|r| span_contains(r.span, byte))
}

/// The anchored heading whose span contains `byte`, if any.
pub fn heading_anchor_at(doc: &Document, byte: usize) -> Option<(Span, String)> {
    collect_headings(doc)
        .into_iter()
        .find(|h| h.anchor.is_some() && span_contains(h.span, byte))
        .map(|h| (h.span, h.anchor.unwrap()))
}

/// Span of the heading declaring `anchor`, if any.
pub fn heading_span_for_anchor(doc: &Document, anchor: &str) -> Option<Span> {
    collect_headings(doc)
        .into_iter()
        .find(|h| h.anchor.as_deref() == Some(anchor))
        .map(|h| h.span)
}

fn span_contains(span: Span, byte: usize) -> bool {
    let s = span.start as usize;
    let e = s + span.len as usize;
    byte >= s && byte <= e
}

/// Split a raw `@ref` target into (path, anchor). Best-effort — full
/// validation is the resolver's job; navigation just needs the parts.
pub fn split_target(target: &str) -> (String, Option<String>) {
    match target.trim().split_once('#') {
        Some((p, a)) => (p.to_string(), Some(a.to_string())),
        None => (target.trim().to_string(), None),
    }
}

// ---------------------------------------------------------------------------
// Document symbol nesting
// ---------------------------------------------------------------------------

/// Heading outline as a forest of indices into the `collect_headings`
/// slice. A heading owns every following heading of a deeper level.
pub struct SymbolNode {
    pub idx: usize,
    pub children: Vec<SymbolNode>,
}

pub fn nest_headings(headings: &[HeadingInfo]) -> Vec<SymbolNode> {
    let mut pos = 0;
    nest_from(headings, &mut pos, 0)
}

fn nest_from(headings: &[HeadingInfo], pos: &mut usize, min_level: u8) -> Vec<SymbolNode> {
    let mut out = Vec::new();
    while *pos < headings.len() {
        let level = headings[*pos].level;
        if level < min_level {
            break;
        }
        let idx = *pos;
        *pos += 1;
        let children = nest_from(headings, pos, level + 1);
        out.push(SymbolNode { idx, children });
    }
    out
}

// ---------------------------------------------------------------------------
// Completion context
// ---------------------------------------------------------------------------

/// What the cursor is in the middle of typing, derived from the current
/// line up to the cursor.
#[derive(Debug, PartialEq, Eq)]
pub enum CompletionCtx {
    /// `@` plus a partial name; `prefix` excludes the `@`.
    ShortcodeName { prefix: String },
    /// Inside an unclosed `@ref[`, before any `#`.
    RefPath { prefix: String },
    /// Inside `@ref[<path>#`; `prefix` is the partial anchor.
    RefAnchor { path: String, prefix: String },
}

pub fn completion_context(line_prefix: &str) -> Option<CompletionCtx> {
    // Inside an unclosed `@ref[` target?
    if let Some(idx) = line_prefix.rfind("@ref[") {
        let after = &line_prefix[idx + 5..];
        if !after.contains(']') {
            return Some(match after.split_once('#') {
                Some((path, frag)) => CompletionCtx::RefAnchor {
                    path: path.to_string(),
                    prefix: frag.to_string(),
                },
                None => CompletionCtx::RefPath {
                    prefix: after.to_string(),
                },
            });
        }
    }
    // `@name` prefix: scan back over shortcode-name characters to an `@`
    // that starts a word (so `user@host` never completes).
    let bytes = line_prefix.as_bytes();
    let mut i = bytes.len();
    while i > 0 {
        let c = bytes[i - 1];
        if c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'-' || c == b'_' {
            i -= 1;
        } else {
            break;
        }
    }
    if i > 0 && bytes[i - 1] == b'@' {
        if i >= 2 && bytes[i - 2].is_ascii_alphanumeric() {
            return None;
        }
        return Some(CompletionCtx::ShortcodeName {
            prefix: line_prefix[i..].to_string(),
        });
    }
    None
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use brief::diag::{Code, Severity};
    use std::fs;
    use tempfile::TempDir;

    fn write(p: &Path, content: &str) {
        if let Some(parent) = p.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(p, content).unwrap();
    }

    /// Tempdir project with a brief.toml and files; returns (dir guard,
    /// canonical root).
    fn project(config: &str, files: &[(&str, &str)]) -> (TempDir, PathBuf) {
        let td = TempDir::new().unwrap();
        let root = td.path().canonicalize().unwrap();
        write(&root.join("brief.toml"), config);
        for (rel, content) in files {
            write(&root.join(rel), content);
        }
        (td, root)
    }

    fn parse_doc(src: &str) -> Document {
        let map = SourceMap::new("t.brf", src);
        let tokens = lexer::lex(&map).expect("lex");
        let (doc, _) = parser::parse(tokens, &map);
        doc
    }

    fn has(diags: &[Diagnostic], code: Code) -> bool {
        diags.iter().any(|d| d.code == code)
    }

    fn no_errors(diags: &[Diagnostic]) -> bool {
        diags.iter().all(|d| d.severity != Severity::Error)
    }

    fn empty_overlay() -> BTreeMap<String, BufferEntry> {
        BTreeMap::new()
    }

    // -- snapshot / config parity ------------------------------------------

    #[test]
    fn snapshot_loads_custom_shortcodes_from_brief_toml() {
        let (_td, root) = project(
            "[shortcodes.note]\nkind = \"block\"\ntemplate_html = \"<div>{{content}}</div>\"\n",
            &[],
        );
        let snap = load_snapshot(&root);
        assert!(snap.config_error.is_none());
        assert!(snap.project.registry.get("note").is_some());
        assert!(
            snap.project.registry.get("callout").is_some(),
            "builtins retained"
        );
    }

    #[test]
    fn custom_shortcode_accepted_unknown_rejected() {
        let (_td, root) = project("[shortcodes.note]\nkind = \"block\"\n", &[]);
        let snap = load_snapshot(&root);
        let path = root.join("doc.brf");

        let a = analyze(&path, "@note\nbody\n@end\n", Some(&snap), &empty_overlay());
        assert!(
            !has(&a.diagnostics, Code::UnknownShortcode),
            "{:?}",
            a.diagnostics
        );

        let b = analyze(&path, "@bogus\nbody\n@end\n", Some(&snap), &empty_overlay());
        assert!(
            has(&b.diagnostics, Code::UnknownShortcode),
            "{:?}",
            b.diagnostics
        );
    }

    #[test]
    fn unknown_shortcode_errors_without_project_too() {
        // Same behavior as `brief compile` outside a project: builtins only.
        let a = analyze(
            Path::new("/nonexistent/doc.brf"),
            "@bogus\nbody\n@end\n",
            None,
            &empty_overlay(),
        );
        assert!(has(&a.diagnostics, Code::UnknownShortcode));
    }

    #[test]
    fn strict_heading_levels_honored_from_config() {
        let (_td, root) = project("[compile]\nstrict_heading_levels = true\n", &[]);
        let snap = load_snapshot(&root);
        let path = root.join("doc.brf");
        let text = "# A\n\n### Skipped\n";

        let strict = analyze(&path, text, Some(&snap), &empty_overlay());
        assert!(
            has(&strict.diagnostics, Code::HeadingMonotonic),
            "{:?}",
            strict.diagnostics
        );

        let lax = analyze(&path, text, None, &empty_overlay());
        assert!(!has(&lax.diagnostics, Code::HeadingMonotonic));
    }

    #[test]
    fn bad_brief_toml_reports_error_and_falls_back() {
        let (_td, root) = project("this is [ not toml\n", &[]);
        let snap = load_snapshot(&root);
        assert!(snap.config_error.is_some());
        // Builtins still work so diagnostics keep flowing.
        let a = analyze(
            &root.join("doc.brf"),
            "*bold* text\n",
            Some(&snap),
            &empty_overlay(),
        );
        assert!(no_errors(&a.diagnostics), "{:?}", a.diagnostics);
    }

    // -- @ref parity --------------------------------------------------------

    #[test]
    fn ref_diagnostics_match_compiler() {
        let (_td, root) = project("", &[("other.brf", "# Other {#x}\n")]);
        let snap = load_snapshot(&root);
        let path = root.join("cur.brf");

        let ok = analyze(
            &path,
            "See @ref[other.brf#x](Other).\n",
            Some(&snap),
            &empty_overlay(),
        );
        assert!(no_errors(&ok.diagnostics), "{:?}", ok.diagnostics);

        let bad_anchor = analyze(
            &path,
            "See @ref[other.brf#nope](Other).\n",
            Some(&snap),
            &empty_overlay(),
        );
        assert!(has(&bad_anchor.diagnostics, Code::RefMissingAnchor));

        let bad_file = analyze(
            &path,
            "See @ref[missing.brf](Gone).\n",
            Some(&snap),
            &empty_overlay(),
        );
        assert!(has(&bad_file.diagnostics, Code::RefMissingFile));
    }

    #[test]
    fn ref_without_project_is_b0604() {
        let a = analyze(
            Path::new("/nonexistent/doc.brf"),
            "See @ref[other.brf](O).\n",
            None,
            &empty_overlay(),
        );
        assert!(has(&a.diagnostics, Code::RefNoProject));
    }

    #[test]
    fn dirty_buffer_overlay_beats_disk_index() {
        // Disk says other.brf declares {#old}; the open buffer renamed it
        // to {#new}. Diagnostics must follow the buffer.
        let (_td, root) = project("", &[("other.brf", "# Other {#old}\n")]);
        let snap = load_snapshot(&root);
        let path = root.join("cur.brf");
        let mut overlay = empty_overlay();
        overlay.insert(
            "other.brf".to_string(),
            BufferEntry {
                anchors: BTreeSet::from(["new".to_string()]),
                broken: false,
            },
        );

        let stale = analyze(
            &path,
            "See @ref[other.brf#old](O).\n",
            Some(&snap),
            &overlay,
        );
        assert!(
            has(&stale.diagnostics, Code::RefMissingAnchor),
            "{:?}",
            stale.diagnostics
        );

        let fresh = analyze(
            &path,
            "See @ref[other.brf#new](O).\n",
            Some(&snap),
            &overlay,
        );
        assert!(no_errors(&fresh.diagnostics), "{:?}", fresh.diagnostics);
    }

    #[test]
    fn bad_brief_toml_message_matches_the_cli() {
        let (_td, root) = project("[compile]\nnope = 1\n", &[]);
        let snap = load_snapshot(&root);
        let err = snap.config_error.expect("bad config");
        let prefix = format!("bad config {}: ", root.join("brief.toml").display());
        assert!(err.starts_with(&prefix), "{}", err);
    }

    #[test]
    fn ref_into_broken_file_is_b0605_not_b0602() {
        // The draft's heading parsed, but the file as a whole does not
        // compile. Before the shared front-end this reported "anchor not
        // found" (B0602), pointing the user at the wrong problem.
        let (_td, root) = project("", &[("draft.brf", "# D {#d}\n\n*unfinished\n")]);
        let snap = load_snapshot(&root);
        let a = analyze(
            &root.join("cur.brf"),
            "See @ref[draft.brf#d](the draft).\n",
            Some(&snap),
            &empty_overlay(),
        );
        let codes: Vec<Code> = a.diagnostics.iter().map(|d| d.code).collect();
        assert_eq!(codes, vec![Code::RefTargetBroken]);
    }

    #[test]
    fn unrelated_broken_file_reports_nothing() {
        let (_td, root) = project("", &[("draft.brf", "*unfinished\n")]);
        let snap = load_snapshot(&root);
        let a = analyze(
            &root.join("cur.brf"),
            "# Cur\n",
            Some(&snap),
            &empty_overlay(),
        );
        assert!(a.diagnostics.is_empty(), "{:?}", a.diagnostics);
    }

    #[test]
    fn overlay_broken_state_beats_disk_index() {
        let (_td, root) = project(
            "",
            &[
                ("draft.brf", "# D {#d}\n\n*unfinished\n"),
                ("clean.brf", "# C {#c}\n"),
            ],
        );
        let snap = load_snapshot(&root);
        let path = root.join("cur.brf");
        let text = "See @ref[draft.brf#d](D) and @ref[clean.brf#c](C).\n";

        // draft.brf is fixed in its (unsaved) buffer; clean.brf is broken
        // in its buffer. The overlay decides, not the disk.
        let mut overlay = empty_overlay();
        overlay.insert(
            "draft.brf".to_string(),
            BufferEntry {
                anchors: BTreeSet::from(["d".to_string()]),
                broken: false,
            },
        );
        overlay.insert(
            "clean.brf".to_string(),
            BufferEntry {
                anchors: BTreeSet::from(["c".to_string()]),
                broken: true,
            },
        );
        let a = analyze(&path, text, Some(&snap), &overlay);
        let broken: Vec<usize> = a
            .diagnostics
            .iter()
            .filter(|d| d.code == Code::RefTargetBroken)
            .map(|d| d.span.start as usize)
            .collect();
        assert_eq!(broken, vec![text.find("@ref[clean").unwrap()]);
        assert_eq!(a.diagnostics.len(), 1, "{:?}", a.diagnostics);
    }

    #[test]
    fn buffer_entry_judges_like_the_indexer() {
        let (_td, root) = project("", &[]);
        let snap = load_snapshot(&root);
        let path = root.join("cur.brf");
        let ok = analyze(&path, "# T {#t}\n", Some(&snap), &empty_overlay());
        assert_eq!(
            ok.entry,
            BufferEntry {
                anchors: BTreeSet::from(["t".to_string()]),
                broken: false,
            }
        );
        let parse_err = analyze(&path, "# T {#t}\n\n*open\n", Some(&snap), &empty_overlay());
        assert!(parse_err.entry.broken);
        assert!(parse_err.entry.anchors.contains("t"));
        let lex_err = analyze(&path, "a\tb\n", Some(&snap), &empty_overlay());
        assert_eq!(
            lex_err.entry,
            BufferEntry {
                anchors: BTreeSet::new(),
                broken: true,
            }
        );
    }

    #[test]
    fn self_ref_resolves_against_buffer_not_disk() {
        // cur.brf does not exist on disk at all; the buffer declares the
        // anchor it references.
        let (_td, root) = project("", &[]);
        let snap = load_snapshot(&root);
        let path = root.join("cur.brf");
        let a = analyze(
            &path,
            "# Top {#top}\n\nSee @ref[cur.brf#top](Top).\n",
            Some(&snap),
            &empty_overlay(),
        );
        assert!(no_errors(&a.diagnostics), "{:?}", a.diagnostics);
        assert!(a.entry.anchors.contains("top"));
    }

    // -- navigation helpers --------------------------------------------------

    #[test]
    fn ref_at_and_split_target() {
        let src = "Intro text.\n\nSee @ref[docs/other.brf#sec](Other).\n";
        let doc = parse_doc(src);
        let at = src.find("@ref").unwrap() + 2;
        let site = ref_at(&doc, at).expect("ref under cursor");
        assert_eq!(site.target, "docs/other.brf#sec");
        let (p, a) = split_target(&site.target);
        assert_eq!(p, "docs/other.brf");
        assert_eq!(a.as_deref(), Some("sec"));
        assert!(ref_at(&doc, 0).is_none());
    }

    #[test]
    fn collect_refs_reaches_nested_content() {
        let src = "\
- item with @ref[a.brf](A)

> quoted @ref[b.brf#x](B)

@t
| h1 | h2 |
| @ref[c.brf](C) | y |
";
        let doc = parse_doc(src);
        let targets: Vec<String> = collect_refs(&doc).into_iter().map(|r| r.target).collect();
        assert_eq!(targets, vec!["a.brf", "b.brf#x", "c.brf"]);
    }

    #[test]
    fn heading_lookup_by_position_and_anchor() {
        let src = "# Title {#top}\n\nBody.\n\n## Section {#sec}\n";
        let doc = parse_doc(src);
        let (_, anchor) = heading_anchor_at(&doc, 2).expect("cursor on heading");
        assert_eq!(anchor, "top");
        assert!(heading_anchor_at(&doc, src.find("Body").unwrap()).is_none());
        let span = heading_span_for_anchor(&doc, "sec").expect("anchor exists");
        assert_eq!(span.start as usize, src.find("## Section").unwrap());
        assert!(heading_span_for_anchor(&doc, "nope").is_none());
    }

    // -- symbols --------------------------------------------------------------

    #[test]
    fn heading_outline_nests_by_level() {
        let src = "# A\n\n## A1\n\n### A1a\n\n## A2\n\n# B\n";
        let doc = parse_doc(src);
        let headings = collect_headings(&doc);
        assert_eq!(headings.len(), 5);
        assert_eq!(headings[0].text, "A");
        let tree = nest_headings(&headings);
        assert_eq!(tree.len(), 2); // A, B
        assert_eq!(tree[0].children.len(), 2); // A1, A2
        assert_eq!(tree[0].children[0].children.len(), 1); // A1a
        assert!(tree[1].children.is_empty());
    }

    #[test]
    fn skipped_levels_still_nest() {
        let src = "## Deep Start\n\n# Top\n\n### Jump\n";
        let doc = parse_doc(src);
        let tree = nest_headings(&collect_headings(&doc));
        assert_eq!(tree.len(), 2); // "Deep Start" and "Top" are both roots
        assert_eq!(tree[1].children.len(), 1); // "Jump" nests under "Top"
    }

    #[test]
    fn heading_text_flattens_inline_markup() {
        let src = "# The *bold* `code` _title_\n";
        let doc = parse_doc(src);
        let headings = collect_headings(&doc);
        assert_eq!(headings[0].text, "The bold code title");
    }

    // -- completion context ----------------------------------------------------

    #[test]
    fn completion_context_detection() {
        use CompletionCtx::*;
        assert_eq!(
            completion_context("some text @"),
            Some(ShortcodeName {
                prefix: String::new()
            })
        );
        assert_eq!(
            completion_context("@cal"),
            Some(ShortcodeName {
                prefix: "cal".into()
            })
        );
        assert_eq!(
            completion_context("see @ref["),
            Some(RefPath {
                prefix: String::new()
            })
        );
        assert_eq!(
            completion_context("see @ref[docs/ot"),
            Some(RefPath {
                prefix: "docs/ot".into()
            })
        );
        assert_eq!(
            completion_context("see @ref[docs/other.brf#"),
            Some(RefAnchor {
                path: "docs/other.brf".into(),
                prefix: String::new()
            })
        );
        assert_eq!(
            completion_context("see @ref[docs/other.brf#se"),
            Some(RefAnchor {
                path: "docs/other.brf".into(),
                prefix: "se".into()
            })
        );
        // Closed ref target: no longer completing inside it.
        assert_eq!(completion_context("see @ref[done.brf]"), None);
        // Emails and mid-word @ must not trigger.
        assert_eq!(completion_context("mail me user@host"), None);
        assert_eq!(completion_context("plain text"), None);
    }

    // -- rel_path ---------------------------------------------------------------

    #[test]
    fn rel_path_forward_slashes_and_outside_root() {
        let (_td, root) = project("", &[("docs/deep.brf", "# D {#d}\n")]);
        assert_eq!(
            rel_path(&root, &root.join("docs/deep.brf")).as_deref(),
            Some("docs/deep.brf")
        );
        assert!(rel_path(&root, Path::new("/elsewhere/x.brf")).is_none());
    }
}
