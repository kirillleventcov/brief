//! Project-wide compile state: root discovery, the compile context every
//! tool shares, and the cross-document reference index.
//!
//! `Project::discover` finds the `brief.toml` that governs a file and loads
//! its config and shortcode registry; `Project::check` runs the one
//! lex → parse → resolve → validate front-end. The CLI, `brief watch`, the
//! language server and brief-web all go through these two calls, so the same
//! file gets the same verdict everywhere. `build_index` walks the project
//! tree and collects the heading anchors that `@ref` is allowed to point at.

use crate::ast::{Block, Document, Inline};
use crate::config::{self, Config};
use crate::diag::{Code, Diagnostic, Severity, render_all};
use crate::resolve::{self, ResolveProject};
use crate::shortcode::Registry;
use crate::span::{SourceMap, Span};
use crate::validate::{self, ValidateOptions};
use crate::{lexer, parser, slug};
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io;
use std::path::{self, Path, PathBuf};

/// Read a Brief source file the way every tool does: UTF-8, with a leading
/// byte-order mark stripped.
///
/// # Errors
///
/// Returns the I/O error when the file cannot be read or is not UTF-8.
pub fn read_source(path: &Path) -> io::Result<String> {
    let raw = fs::read_to_string(path)?;
    Ok(match raw.strip_prefix('\u{feff}') {
        Some(rest) => rest.to_string(),
        None => raw,
    })
}

/// The compile context for a file: its project root, the config that
/// applies to it, and the shortcode registry built from that config.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct Project {
    /// Canonical directory holding `brief.toml`, or `None` for a file that
    /// is not inside any project.
    pub root: Option<PathBuf>,
    /// The config file that was loaded: the explicit override if one was
    /// given, else `<root>/brief.toml`. `None` means built-in defaults.
    pub config_path: Option<PathBuf>,
    /// The config that applies to the file.
    pub config: Config,
    /// Built-in shortcodes plus those `config` declares.
    pub registry: Registry,
}

/// What `Project::check` produced for one source.
#[derive(Debug)]
#[non_exhaustive]
pub struct Checked {
    /// `None` when the source failed to lex; there is nothing to emit.
    pub doc: Option<Document>,
    /// Every diagnostic the front-end reported, errors and warnings alike.
    pub diagnostics: Vec<Diagnostic>,
}

impl Checked {
    /// Whether any diagnostic is an error, i.e. the source must not be
    /// emitted.
    #[must_use]
    pub fn has_errors(&self) -> bool {
        self.diagnostics
            .iter()
            .any(|d| d.severity == Severity::Error)
    }

    /// Project-relative paths of the broken files this source `@ref`s (the
    /// targets of its B0605 diagnostics), in source order, deduplicated.
    #[must_use]
    pub fn broken_targets(&self) -> Vec<String> {
        let spans: BTreeSet<Span> = self
            .diagnostics
            .iter()
            .filter(|d| d.code == Code::RefTargetBroken)
            .map(|d| d.span)
            .collect();
        let Some(doc) = self.doc.as_ref().filter(|_| !spans.is_empty()) else {
            return Vec::new();
        };
        let mut out: Vec<String> = Vec::new();
        for site in collect_refs(doc) {
            if !spans.contains(&site.span) {
                continue;
            }
            let path = site.target.trim();
            let path = path.split_once('#').map_or(path, |(p, _)| p);
            if !out.iter().any(|p| p == path) {
                out.push(path.to_string());
            }
        }
        out
    }

    /// Render the diagnostics against `src`, then those of every broken
    /// file this source `@ref`s, each against its own source, so a B0605
    /// arrives with the reason the target is broken. `index_diags` is the
    /// `IndexBuild::diagnostics` of `Project::index`; diagnostics of files
    /// this source does not reference are never shown.
    #[must_use]
    pub fn render(&self, src: &SourceMap, index_diags: &[FileDiagnostics]) -> String {
        let mut parts = vec![render_all(&self.diagnostics, src)];
        for target in self.broken_targets() {
            if let Some(fd) = index_diags.iter().find(|fd| fd.rel == target) {
                parts.push(render_all(&fd.diagnostics, &fd.source));
            }
        }
        parts.retain(|p| !p.is_empty());
        parts.join("\n")
    }
}

impl Project {
    /// Find the project that governs `input` (a file or directory) by
    /// walking up to the nearest `brief.toml`. The working directory plays
    /// no part. `config_override` (a `--config` flag) replaces the config
    /// file but not the root, so `@ref` still resolves against the tree the
    /// file lives in.
    ///
    /// # Errors
    ///
    /// Returns a message naming the config file when it fails to load.
    pub fn discover(input: &Path, config_override: Option<&Path>) -> Result<Project, String> {
        let abs = input
            .canonicalize()
            .or_else(|_| path::absolute(input))
            .unwrap_or_else(|_| input.to_path_buf());
        let root = discover_root(&abs);
        let config_path = config_override
            .map(Path::to_path_buf)
            .or_else(|| root.as_ref().map(|r| r.join("brief.toml")));
        let config = match &config_path {
            Some(p) => config::load(p).map_err(|e| format!("bad config {}: {}", p.display(), e))?,
            None => Config::default(),
        };
        Ok(Project::from_config(root, config_path, config))
    }

    /// A project with an explicit root and config, for tools that locate
    /// their config some other way (brief-web's `book.toml`, the LSP's
    /// fallback after a broken `brief.toml`).
    #[must_use]
    pub fn from_config(
        root: Option<PathBuf>,
        config_path: Option<PathBuf>,
        config: Config,
    ) -> Project {
        let registry = config::registry_from(&config);
        Project {
            root,
            config_path,
            config,
            registry,
        }
    }

    /// No root, default config: built-in shortcodes only, `@ref` is B0604.
    #[must_use]
    pub fn standalone() -> Project {
        Project::from_config(None, None, Config::default())
    }

    /// The validator options this project's `[compile]` table selects.
    #[must_use]
    pub fn validate_options(&self) -> ValidateOptions {
        ValidateOptions {
            strict_heading_levels: self.config.compile.strict_heading_levels,
        }
    }

    /// Index the project tree for `@ref`. `None` outside a project. Files
    /// that fail to lex or parse are indexed leniently and listed in
    /// `ProjectIndex::broken`; their diagnostics are returned for callers
    /// that report on the whole project (`brief check`, a site build).
    #[must_use]
    pub fn index(&self) -> Option<IndexBuild> {
        self.root.as_deref().map(build_index)
    }

    /// Project-relative, forward-slash path of `file`, in the form
    /// `ProjectIndex` keys use. `None` outside the root.
    #[must_use]
    pub fn rel_path(&self, file: &Path) -> Option<String> {
        let root = self.root.as_deref()?;
        let abs = file.canonicalize().unwrap_or_else(|_| file.to_path_buf());
        abs.strip_prefix(root).ok().map(to_forward_slash)
    }

    /// The compile front-end: lex → parse → resolve → validate `src` with
    /// this project's registry and options. `index` and `rel` (the file's
    /// project-relative path) enable `@ref` resolution; without an index,
    /// `@ref` reports B0604 exactly as it does outside a project.
    #[must_use]
    pub fn check(
        &self,
        src: &SourceMap,
        index: Option<&ProjectIndex>,
        rel: Option<&str>,
    ) -> Checked {
        let tokens = match lexer::lex(src) {
            Ok(t) => t,
            Err(diagnostics) => {
                return Checked {
                    doc: None,
                    diagnostics,
                };
            }
        };
        let (mut doc, mut diagnostics) = parser::parse(tokens, src);
        let current = PathBuf::from(rel.unwrap_or_default());
        let resolve_project = index.map(|index| ResolveProject {
            index,
            current: &current,
        });
        diagnostics.extend(resolve::resolve_with_project(
            &mut doc,
            &self.registry,
            resolve_project.as_ref(),
        ));
        diagnostics.extend(validate::validate(&doc, &self.validate_options(), src));
        Checked {
            doc: Some(doc),
            diagnostics,
        }
    }
}

/// Walk up from `start`, returning the first ancestor directory that
/// contains `brief.toml`. Returns `None` if no such ancestor exists.
///
/// `start` may be a file or directory. If a file, its parent is the
/// first directory considered. The search includes `start` itself if
/// `start` is a directory.
#[must_use]
pub fn discover_root(start: &Path) -> Option<PathBuf> {
    let mut cur: PathBuf = if start.is_file() {
        start.parent()?.to_path_buf()
    } else {
        start.to_path_buf()
    };
    loop {
        if cur.join("brief.toml").is_file() {
            return Some(cur);
        }
        if !cur.pop() {
            return None;
        }
    }
}

/// Diagnostics emitted while indexing one project file, paired with the
/// source so the consumer can render them at correct positions.
#[derive(Debug)]
#[non_exhaustive]
pub struct FileDiagnostics {
    /// Project-relative path, the same key `ProjectIndex::anchors` uses.
    pub rel: String,
    /// The file's source, to render `diagnostics` against.
    pub source: SourceMap,
    /// What lexing or parsing the file reported.
    pub diagnostics: Vec<Diagnostic>,
}

/// Snapshot of the project's `@ref` targets. Built once per compilation
/// (or watch tick). Read-only after construction.
#[derive(Clone, Debug, Default)]
pub struct ProjectIndex {
    /// Absolute, canonicalized project root (the directory holding `brief.toml`).
    pub root: PathBuf,
    /// Project-relative `.brf` path (forward-slash, ASCII) → ids of the
    /// headings in it, explicit and automatic alike.
    pub anchors: BTreeMap<String, BTreeSet<String>>,
    /// Project-relative paths of files that failed to lex or parse. They
    /// stay in `anchors` (with whatever headings parsed), but a `@ref` into
    /// one reports B0605 instead of trusting those anchors. An unrelated
    /// broken file never blocks another file's compile.
    pub broken: BTreeSet<String>,
}

/// What `build_index` produced: the index plus the diagnostics of every
/// file that did not lex or parse cleanly.
#[derive(Debug, Default)]
#[non_exhaustive]
pub struct IndexBuild {
    /// The project's `@ref` targets.
    pub index: ProjectIndex,
    /// Per-file diagnostics, each paired with the source that produced it.
    pub diagnostics: Vec<FileDiagnostics>,
}

/// Walk the project tree and collect heading anchors per file.
///
/// Skips:
/// - hidden directories (name starts with `.`),
/// - the `target/`, `dist/`, `node_modules/` directories.
///
/// Files that fail to lex or parse are still indexed (with the anchors that
/// parsed, or none after a lex error) and are listed in `broken`, so a
/// `@ref` into one reports B0605. Returns the index along with per-file
/// diagnostics, each paired with the `SourceMap` of the file that produced
/// them so callers can render diagnostics at the correct positions. Callers
/// must not treat these as errors in the file they are compiling: they
/// belong to other files.
#[must_use]
pub fn build_index(root: &Path) -> IndexBuild {
    let mut idx = ProjectIndex {
        root: root.to_path_buf(),
        ..Default::default()
    };
    let mut all_diags: Vec<FileDiagnostics> = Vec::new();

    walk_brf_files(root, &mut |abs_path: &Path| {
        let rel: String = match abs_path.strip_prefix(root) {
            Ok(r) => to_forward_slash(r),
            Err(_) => return,
        };
        let raw = match read_source(abs_path) {
            Ok(s) => s,
            Err(_) => {
                idx.anchors.insert(rel, BTreeSet::new());
                return;
            }
        };
        let src = SourceMap::new(abs_path.to_string_lossy(), raw);
        let tokens = match lexer::lex(&src) {
            Ok(t) => t,
            Err(d) => {
                idx.broken.insert(rel.clone());
                idx.anchors.insert(rel.clone(), BTreeSet::new());
                all_diags.push(FileDiagnostics {
                    rel,
                    source: src,
                    diagnostics: d,
                });
                return;
            }
        };
        let (doc, parse_diags) = parser::parse(tokens, &src);
        if !parse_diags.is_empty() {
            if parse_diags.iter().any(|d| d.severity == Severity::Error) {
                idx.broken.insert(rel.clone());
            }
            all_diags.push(FileDiagnostics {
                rel: rel.clone(),
                source: src,
                diagnostics: parse_diags,
            });
        }
        let anchors = collect_anchors(&doc);
        idx.anchors.insert(rel, anchors);
    });

    IndexBuild {
        index: idx,
        diagnostics: all_diags,
    }
}

/// Every `.brf` file under `dir`, sorted, skipping the directories
/// `build_index` skips. Each path is `dir` joined with the file's relative
/// path, so a relative `dir` yields relative paths.
#[must_use]
pub fn brf_files(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    walk_brf_files(dir, &mut |p: &Path| out.push(p.to_path_buf()));
    out.sort();
    out
}

fn walk_brf_files(root: &Path, visit: &mut dyn FnMut(&Path)) {
    let rd = match fs::read_dir(root) {
        Ok(rd) => rd,
        Err(_) => return,
    };
    for entry in rd.flatten() {
        let path = entry.path();
        let name = path.file_name().and_then(|s| s.to_str()).unwrap_or("");
        if name.starts_with('.') {
            continue;
        }
        let ft = match entry.file_type() {
            Ok(ft) => ft,
            Err(_) => continue,
        };
        if ft.is_dir() {
            if matches!(name, "target" | "dist" | "node_modules") {
                continue;
            }
            walk_brf_files(&path, visit);
        } else if ft.is_file() && path.extension().and_then(|s| s.to_str()) == Some("brf") {
            visit(&path);
        }
    }
}

/// Collect the id of every heading in `doc` (explicit `{#id}` or automatic
/// slug, see `slug::headings`), including headings nested in blockquotes,
/// lists, and block shortcodes. This is the same set `build_index` records
/// per file; exposed so callers holding an in-memory document (e.g. an
/// editor buffer) can overlay a fresher anchor set over a disk-built index.
#[must_use]
pub fn collect_anchors(doc: &Document) -> BTreeSet<String> {
    slug::headings(doc).into_iter().map(|h| h.id).collect()
}

/// A `@ref` invocation: its source span and the raw target text
/// (`path.brf` or `path.brf#anchor`).
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct RefSite {
    /// Span of the whole `@ref` invocation.
    pub span: Span,
    /// The target text as written in the invocation body.
    pub target: String,
}

/// Every `@ref` in `doc` whose body is a plain target, in source order,
/// including those nested in lists, quotes, tables and shortcodes.
#[must_use]
pub fn collect_refs(doc: &Document) -> Vec<RefSite> {
    let mut out = Vec::new();
    for b in &doc.blocks {
        collect_refs_block(b, &mut out);
    }
    out
}

fn collect_refs_block(b: &Block, out: &mut Vec<RefSite>) {
    match b {
        Block::Heading { content, .. } | Block::Paragraph { content, .. } => {
            collect_refs_inlines(content, out);
        }
        Block::List { items, .. } => {
            for it in items {
                collect_refs_inlines(&it.content, out);
                for c in &it.children {
                    collect_refs_block(c, out);
                }
            }
        }
        Block::Blockquote { children, .. } | Block::BlockShortcode { children, .. } => {
            for c in children {
                collect_refs_block(c, out);
            }
        }
        Block::Table { header, rows, .. } => {
            for cell in &header.cells {
                collect_refs_inlines(cell, out);
            }
            for row in rows {
                for cell in &row.cells {
                    collect_refs_inlines(cell, out);
                }
            }
        }
        Block::DefinitionList { items, .. } => {
            for it in items {
                collect_refs_inlines(&it.term, out);
                collect_refs_inlines(&it.definition, out);
            }
        }
        Block::CodeBlock { .. } | Block::HorizontalRule { .. } => {}
    }
}

fn collect_refs_inlines(nodes: &[Inline], out: &mut Vec<RefSite>) {
    for n in nodes {
        match n {
            Inline::Bold { content, .. }
            | Inline::Italic { content, .. }
            | Inline::Underline { content, .. }
            | Inline::Strike { content, .. } => collect_refs_inlines(content, out),
            Inline::Shortcode {
                name,
                content,
                span,
                ..
            } => {
                if name == "ref" {
                    if let Some([Inline::Text { value, .. }]) = content.as_deref() {
                        out.push(RefSite {
                            span: *span,
                            target: value.clone(),
                        });
                    }
                } else if let Some(c) = content {
                    collect_refs_inlines(c, out);
                }
            }
            _ => {}
        }
    }
}

fn to_forward_slash(p: &Path) -> String {
    let parts: Vec<_> = p
        .components()
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
        .collect();
    parts.join("/")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    fn touch(p: &Path) {
        if let Some(parent) = p.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(p, "").unwrap();
    }

    #[test]
    fn finds_brief_toml_in_same_directory() {
        let td = TempDir::new().unwrap();
        let root = td.path().canonicalize().unwrap();
        touch(&root.join("brief.toml"));
        assert_eq!(discover_root(&root).unwrap().canonicalize().unwrap(), root);
    }

    #[test]
    fn finds_brief_toml_from_subdirectory() {
        let td = TempDir::new().unwrap();
        let root = td.path().canonicalize().unwrap();
        touch(&root.join("brief.toml"));
        let sub = root.join("a/b/c");
        fs::create_dir_all(&sub).unwrap();
        assert_eq!(discover_root(&sub).unwrap().canonicalize().unwrap(), root);
    }

    #[test]
    fn finds_brief_toml_from_a_file() {
        let td = TempDir::new().unwrap();
        let root = td.path().canonicalize().unwrap();
        touch(&root.join("brief.toml"));
        let f = root.join("docs/intro.brf");
        touch(&f);
        assert_eq!(discover_root(&f).unwrap().canonicalize().unwrap(), root);
    }

    #[test]
    fn returns_none_when_no_brief_toml() {
        let td = TempDir::new().unwrap();
        assert!(discover_root(td.path()).is_none());
    }

    #[test]
    fn closest_brief_toml_wins() {
        let td = TempDir::new().unwrap();
        let outer = td.path().canonicalize().unwrap();
        let inner = outer.join("nested");
        touch(&outer.join("brief.toml"));
        touch(&inner.join("brief.toml"));
        let deepest = inner.join("docs");
        fs::create_dir_all(&deepest).unwrap();
        assert_eq!(
            discover_root(&deepest).unwrap().canonicalize().unwrap(),
            inner.canonicalize().unwrap()
        );
    }

    fn write(p: &Path, content: &str) {
        if let Some(parent) = p.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(p, content).unwrap();
    }

    #[test]
    fn build_index_collects_anchors_per_file() {
        let td = TempDir::new().unwrap();
        let root = td.path();
        touch(&root.join("brief.toml"));
        write(
            &root.join("a.brf"),
            "# Title {#top}\n\nSome text.\n\n## Subsection {#sub}\n",
        );
        write(&root.join("docs/b.brf"), "# Other Title {#other}\n");
        let IndexBuild {
            index: idx,
            diagnostics: diags,
        } = build_index(root);
        assert!(
            diags.iter().all(|fd| fd
                .diagnostics
                .iter()
                .all(|d| d.severity != crate::diag::Severity::Error)),
            "{:?}",
            diags,
        );
        assert_eq!(
            idx.root.canonicalize().unwrap(),
            root.canonicalize().unwrap()
        );
        assert_eq!(
            idx.anchors.get("a.brf").unwrap(),
            &BTreeSet::from(["top".into(), "sub".into()])
        );
        assert_eq!(
            idx.anchors.get("docs/b.brf").unwrap(),
            &BTreeSet::from(["other".into()])
        );
    }

    #[test]
    fn build_index_skips_target_dist_and_hidden_dirs() {
        let td = TempDir::new().unwrap();
        let root = td.path();
        touch(&root.join("brief.toml"));
        write(&root.join("good.brf"), "# G {#g}\n");
        write(&root.join("target/x.brf"), "# X {#x}\n");
        write(&root.join("dist/y.brf"), "# Y {#y}\n");
        write(&root.join(".hidden/z.brf"), "# Z {#z}\n");
        let IndexBuild {
            index: idx,
            diagnostics: diags,
        } = build_index(root);
        assert!(
            diags.iter().all(|fd| fd
                .diagnostics
                .iter()
                .all(|d| d.severity != crate::diag::Severity::Error)),
            "{:?}",
            diags,
        );
        assert!(idx.anchors.contains_key("good.brf"));
        assert!(!idx.anchors.contains_key("target/x.brf"));
        assert!(!idx.anchors.contains_key("dist/y.brf"));
        assert!(!idx.anchors.contains_key(".hidden/z.brf"));
    }

    #[test]
    fn discover_loads_the_files_own_brief_toml() {
        let td = TempDir::new().unwrap();
        let root = td.path().canonicalize().unwrap();
        write(
            &root.join("brief.toml"),
            "[shortcodes.lede]\nkind = \"inline\"\ntemplate_html = \"<b>{{content}}</b>\"\n",
        );
        let file = root.join("docs/a.brf");
        write(&file, "@lede[x]\n");
        // Discovery starts at the file, so the process cwd is irrelevant.
        let p = Project::discover(&file, None).unwrap();
        assert_eq!(p.root.as_deref(), Some(root.as_path()));
        assert_eq!(p.config_path, Some(root.join("brief.toml")));
        assert!(p.registry.get("lede").is_some());
        assert_eq!(p.rel_path(&file).as_deref(), Some("docs/a.brf"));
    }

    #[test]
    fn discover_outside_a_project_uses_defaults() {
        let td = TempDir::new().unwrap();
        let file = td.path().join("a.brf");
        write(&file, "x\n");
        let p = Project::discover(&file, None).unwrap();
        assert!(p.root.is_none());
        assert!(p.config_path.is_none());
        assert!(p.index().is_none());
    }

    #[test]
    fn discover_config_override_keeps_the_root() {
        let td = TempDir::new().unwrap();
        let root = td.path().canonicalize().unwrap();
        touch(&root.join("brief.toml"));
        let other = root.join("other.toml");
        write(&other, "[compile]\nstrict_heading_levels = true\n");
        let file = root.join("a.brf");
        write(&file, "x\n");
        let p = Project::discover(&file, Some(&other)).unwrap();
        assert_eq!(p.root.as_deref(), Some(root.as_path()));
        assert!(p.validate_options().strict_heading_levels);
    }

    #[test]
    fn discover_reports_a_bad_config() {
        let td = TempDir::new().unwrap();
        write(&td.path().join("brief.toml"), "[compile]\nnope = 1\n");
        let file = td.path().join("a.brf");
        write(&file, "x\n");
        let err = Project::discover(&file, None).unwrap_err();
        assert!(err.contains("bad config"), "{}", err);
    }

    #[test]
    fn broken_files_are_indexed_and_listed() {
        let td = TempDir::new().unwrap();
        let root = td.path();
        touch(&root.join("brief.toml"));
        write(&root.join("good.brf"), "# G {#g}\n");
        write(&root.join("draft.brf"), "# D {#d}\n\n*unfinished\n");
        write(&root.join("tabbed.brf"), "a\tb\n");
        let IndexBuild {
            index: idx,
            diagnostics: diags,
        } = build_index(root);
        assert_eq!(
            idx.broken,
            BTreeSet::from(["draft.brf".to_string(), "tabbed.brf".to_string()])
        );
        // Parse errors still leave the headings that parsed.
        assert!(idx.anchors["draft.brf"].contains("d"));
        assert!(idx.anchors.contains_key("tabbed.brf"));
        let rels: BTreeSet<&str> = diags.iter().map(|fd| fd.rel.as_str()).collect();
        assert!(rels.contains("draft.brf") && rels.contains("tabbed.brf"));
    }

    #[test]
    fn check_ignores_unrelated_broken_files() {
        let td = TempDir::new().unwrap();
        let root = td.path().canonicalize().unwrap();
        touch(&root.join("brief.toml"));
        write(&root.join("draft.brf"), "*unfinished\n");
        let guide = root.join("guide.brf");
        write(&guide, "# Guide\n\nText.\n");
        let p = Project::discover(&guide, None).unwrap();
        let idx = p.index().unwrap().index;
        let src = SourceMap::new("guide.brf", read_source(&guide).unwrap());
        let checked = p.check(&src, Some(&idx), p.rel_path(&guide).as_deref());
        assert!(!checked.has_errors(), "{:?}", checked.diagnostics);
    }

    #[test]
    fn check_reports_a_ref_into_a_broken_file() {
        let td = TempDir::new().unwrap();
        let root = td.path().canonicalize().unwrap();
        touch(&root.join("brief.toml"));
        write(&root.join("draft.brf"), "# D {#d}\n\n*unfinished\n");
        let guide = root.join("guide.brf");
        write(&guide, "See @ref[draft.brf#d](the draft).\n");
        let p = Project::discover(&guide, None).unwrap();
        let idx = p.index().unwrap().index;
        let src = SourceMap::new("guide.brf", read_source(&guide).unwrap());
        let checked = p.check(&src, Some(&idx), Some("guide.brf"));
        let codes: Vec<_> = checked.diagnostics.iter().map(|d| d.code).collect();
        assert_eq!(codes, vec![crate::diag::Code::RefTargetBroken]);
    }

    #[test]
    fn build_index_records_automatic_heading_ids() {
        let td = TempDir::new().unwrap();
        let root = td.path();
        touch(&root.join("brief.toml"));
        write(
            &root.join("a.brf"),
            "# Getting Started\n\n## Install Steps {#setup}\n\n## What's *new*?\n",
        );
        let idx = build_index(root).index;
        assert_eq!(
            idx.anchors["a.brf"],
            BTreeSet::from([
                "getting-started".to_string(),
                "setup".to_string(),
                "whats-new".to_string()
            ])
        );
    }

    #[test]
    fn check_resolves_a_ref_to_an_automatic_id_relative_to_the_file() {
        let td = TempDir::new().unwrap();
        let root = td.path().canonicalize().unwrap();
        touch(&root.join("brief.toml"));
        write(&root.join("guide.brf"), "# Guide\n\n## Install Steps\n");
        let page = root.join("sub/a.brf");
        write(&page, "See @ref[guide.brf#install-steps](the steps).\n");
        let p = Project::discover(&page, None).unwrap();
        let idx = p.index().unwrap().index;
        let src = SourceMap::new("a.brf", read_source(&page).unwrap());
        let checked = p.check(&src, Some(&idx), p.rel_path(&page).as_deref());
        assert!(checked.diagnostics.is_empty(), "{:?}", checked.diagnostics);
        let doc = checked.doc.unwrap();
        let html = crate::emit::html::render(&doc, &p.registry);
        assert_eq!(
            html,
            "<p>See <a href=\"../guide.html#install-steps\">the steps</a>.</p>\n"
        );
    }

    #[test]
    fn check_reports_duplicate_ids_and_dead_fragments() {
        let p = Project::standalone();
        let src = SourceMap::new(
            "a.brf",
            "## Setup\n\n## Setup\n\nSee @link[x](#setupp) and @link[y](javascript:x).\n",
        );
        let codes: Vec<_> = p
            .check(&src, None, None)
            .diagnostics
            .iter()
            .map(|d| d.code.to_string())
            .collect();
        assert_eq!(codes, ["B0410", "B0506", "B0606"]);
    }

    #[test]
    fn render_appends_only_referenced_broken_files() {
        let td = TempDir::new().unwrap();
        let root = td.path().canonicalize().unwrap();
        touch(&root.join("brief.toml"));
        write(&root.join("draft.brf"), "# D {#d}\n\n*unfinished\n");
        write(&root.join("other.brf"), "a\tb\n");
        let guide = root.join("guide.brf");
        write(
            &guide,
            "See @ref[draft.brf#d](one) and @ref[draft.brf](two).\n",
        );
        let p = Project::discover(&guide, None).unwrap();
        let IndexBuild {
            index: idx,
            diagnostics: index_diags,
        } = p.index().unwrap();
        let src = SourceMap::new("guide.brf", read_source(&guide).unwrap());
        let checked = p.check(&src, Some(&idx), Some("guide.brf"));
        assert_eq!(checked.broken_targets(), vec!["draft.brf".to_string()]);

        let out = checked.render(&src, &index_diags);
        let draft = root.join("draft.brf").display().to_string();
        let other = root.join("other.brf").display().to_string();
        assert!(out.starts_with("error[B0605]"), "{}", out);
        assert!(out.contains(&format!("--> {}:3:", draft)), "{}", out);
        assert!(!out.contains(&other), "{}", out);
    }

    #[test]
    fn collect_refs_reaches_nested_content() {
        let src = SourceMap::new(
            "t.brf",
            "- item with @ref[a.brf](A)\n\n> quoted @ref[b.brf#x](B)\n\n@t\n| h1 | h2 |\n| @ref[c.brf](C) | y |\n",
        );
        let tokens = crate::lexer::lex(&src).unwrap();
        let (doc, _) = crate::parser::parse(tokens, &src);
        let targets: Vec<String> = collect_refs(&doc).into_iter().map(|r| r.target).collect();
        assert_eq!(targets, vec!["a.brf", "b.brf#x", "c.brf"]);
    }

    #[test]
    fn read_source_strips_a_leading_bom() {
        let td = TempDir::new().unwrap();
        let f = td.path().join("a.brf");
        fs::write(&f, "\u{feff}# T\n").unwrap();
        assert_eq!(read_source(&f).unwrap(), "# T\n");
    }

    #[test]
    fn build_index_records_files_with_no_anchors_too() {
        let td = TempDir::new().unwrap();
        let root = td.path();
        touch(&root.join("brief.toml"));
        write(&root.join("plain.brf"), "Just a paragraph.\n");
        let IndexBuild {
            index: idx,
            diagnostics: diags,
        } = build_index(root);
        assert!(
            diags.iter().all(|fd| fd
                .diagnostics
                .iter()
                .all(|d| d.severity != crate::diag::Severity::Error)),
            "{:?}",
            diags,
        );
        let entry = idx.anchors.get("plain.brf").unwrap();
        assert!(entry.is_empty());
    }
}
