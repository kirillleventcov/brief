//! Build pipeline: check every site page with the shared Brief front-end,
//! and only when the whole site is clean, wrap each page in the theme
//! template and write `dist/`.

use brief::ast::{Block, Document};
use brief::diag::{Code, Diagnostic, Severity, render_all};
use brief::emit::html;
use brief::project::{Project, ProjectIndex, read_source};
use brief::span::{SourceMap, Span};

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::fs;
use std::path::{Component, Path, PathBuf};

use crate::config::BookConfig;
use crate::summary::{self, Entry, PageEntry};
use crate::theme::{self, PageContext, Theme};

/// Outcome of a build: how many pages produced, plus any warnings.
pub struct BuildOutcome {
    pub pages_built: usize,
    /// Rendered warnings, one per diagnostic, rustc-style with positions.
    pub warnings: Vec<String>,
    pub project_index: ProjectIndex,
}

/// Why a build stopped. Nothing under the output directory is touched
/// before every page has been checked, so a failed build leaves the last
/// good output in place.
#[derive(Debug)]
pub struct BuildError {
    /// Every diagnostic from every file, rendered rustc-style. Empty when the
    /// build stopped for a reason with no source position (I/O, layout).
    pub report: String,
    /// One-line summary.
    pub message: String,
}

impl From<String> for BuildError {
    fn from(message: String) -> Self {
        BuildError {
            report: String::new(),
            message,
        }
    }
}

impl From<&str> for BuildError {
    fn from(message: &str) -> Self {
        message.to_string().into()
    }
}

impl fmt::Display for BuildError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if !self.report.is_empty() {
            writeln!(f, "{}", self.report)?;
        }
        f.write_str(&self.message)
    }
}

#[derive(Clone)]
pub struct Builder {
    pub project_dir: PathBuf,
    pub config: BookConfig,
    /// When true, the page template embeds the live-reload SSE client.
    pub inject_reload: bool,
    /// When true, a warning fails the build the way an error does.
    pub deny_warnings: bool,
}

/// A checked site page. Rendered only once the whole site is clean.
struct CheckedPage<'a> {
    entry: &'a PageEntry,
    doc: Document,
    out_path: PathBuf,
}

impl Builder {
    pub fn new(project_dir: impl AsRef<Path>) -> Result<Self, String> {
        let project_dir = project_dir.as_ref().to_path_buf();
        let config = BookConfig::load_from_dir(&project_dir)?;
        Ok(Builder {
            project_dir,
            config,
            inject_reload: false,
            deny_warnings: false,
        })
    }

    pub fn from_config(project_dir: impl AsRef<Path>, config: BookConfig) -> Self {
        Builder {
            project_dir: project_dir.as_ref().to_path_buf(),
            config,
            inject_reload: false,
            deny_warnings: false,
        }
    }

    pub fn src_dir(&self) -> PathBuf {
        self.project_dir.join(&self.config.build.src)
    }
    pub fn out_dir(&self) -> PathBuf {
        self.project_dir.join(&self.config.build.out)
    }
    pub fn theme_dir(&self) -> Option<PathBuf> {
        self.config
            .site
            .theme
            .as_ref()
            .map(|p| self.project_dir.join(p))
            .or_else(|| {
                let p = self.project_dir.join("theme");
                if p.exists() { Some(p) } else { None }
            })
    }

    pub fn build(&self) -> Result<BuildOutcome, BuildError> {
        let src = self.src_dir();
        if !src.exists() {
            return Err(format!("source directory does not exist: {}", src.display()).into());
        }
        let out = self.out_dir();
        let theme = Theme::load(self.theme_dir().as_deref())?;

        // 1. The compile context: built-ins plus book.toml's shortcodes, with
        //    `@ref` rooted at the source directory.
        let mut brief_cfg = brief::config::Config::default();
        brief_cfg.shortcodes = self.config.shortcodes.clone();
        let project = Project::from_config(
            Some(src.clone()),
            Some(self.project_dir.join("book.toml")),
            brief_cfg,
        );
        // The index is lenient and its own diagnostics are dropped: a broken
        // draft no page links to must not fail the site, a `@ref` into one
        // is B0605 at the `@ref`, and a site page's errors are reported when
        // the page itself is checked below.
        let (project_index, _) = project.index().unwrap_or_default();
        let mut report = Report::default();

        // 2. Decide between single-page and multi-page mode.
        let summary_path = src.join("SUMMARY.brf");
        let multi_page = summary_path.exists();
        let entries: Vec<Entry> = if multi_page {
            let summary_src = load_source(&summary_path)?;
            let checked = project.check(&summary_src, Some(&project_index), Some("SUMMARY.brf"));
            // An entry whose page has errors gets B0605 here; those errors
            // are reported in full when the page is checked, so only the
            // page's own diagnostics are shown.
            let diags: Vec<Diagnostic> = checked
                .diagnostics
                .into_iter()
                .filter(|d| d.code != Code::RefTargetBroken)
                .collect();
            report.add("SUMMARY.brf", &summary_src, &diags);
            // A SUMMARY that failed to resolve has an unknown page set, and
            // checking pages against it would blame them for links into
            // entries that merely failed to parse.
            let doc = match checked.doc {
                Some(doc) if !report.has_errors() => doc,
                _ => return Err(report.into_error(&out)),
            };
            let entries = summary::extract(&doc)?;
            let mut seen = BTreeSet::new();
            for e in &entries {
                if let Entry::Page(p) = e {
                    for page in p.flatten() {
                        if !seen.insert(&page.path) {
                            let name = page.path.display();
                            report.add_site_error(
                                "SUMMARY.brf",
                                render_site_error(
                                    &summary_src,
                                    page.span,
                                    &format!("`{}` is listed in SUMMARY.brf more than once", name),
                                    "duplicate entry",
                                    "a page can appear in the navigation only once",
                                ),
                            );
                        }
                    }
                }
            }
            entries
        } else {
            // Single-page mode: synthesize an entry for index.brf.
            let index = src.join("index.brf");
            if !index.exists() {
                return Err(format!(
                    "neither SUMMARY.brf nor index.brf was found under {}",
                    src.display()
                )
                .into());
            }
            vec![Entry::Page(PageEntry {
                path: PathBuf::from("index.brf"),
                title: theme::site_title(&self.config).to_string(),
                span: Span::DUMMY,
                children: Vec::new(),
            })]
        };

        // 3. Collect every page entry (flattened, first listing wins).
        let mut flat: Vec<&PageEntry> = Vec::new();
        let mut site_pages: BTreeSet<String> = BTreeSet::new();
        for e in &entries {
            if let Entry::Page(p) = e {
                for page in p.flatten() {
                    if site_pages.insert(rel_key(&page.path)) {
                        flat.push(page);
                    }
                }
            }
        }
        if flat.is_empty() {
            return Err("SUMMARY.brf contains no @ref entries".into());
        }

        // 4. Check every page. Nothing is written until the whole site is
        //    clean, so `serve` keeps the last good output after a bad edit.
        let mut pages: Vec<CheckedPage> = Vec::with_capacity(flat.len());
        for entry in flat {
            let rel = rel_key(&entry.path);
            let out_path = output_path(&out, Path::new(&source_path_to_url(&entry.path)))?;
            let abs = src.join(&entry.path);
            let page_src = match read_source(&abs) {
                Ok(text) => SourceMap::new(abs.to_string_lossy(), text),
                Err(e) => {
                    report.add_site_error(
                        &rel,
                        format!("error: cannot read {}: {}\n", abs.display(), e),
                    );
                    continue;
                }
            };
            let checked = project.check(&page_src, Some(&project_index), Some(&rel));
            report.add(&rel, &page_src, &checked.diagnostics);
            let Some(doc) = checked.doc else { continue };
            // `@ref` resolves against every `.brf` under src/, but only site
            // pages are built: a link to any other file would be dead.
            for (span, r) in &doc.resolved_refs {
                if !site_pages.contains(&r.target_path) {
                    let help = if multi_page {
                        format!("add `{}` to SUMMARY.brf", r.target_path)
                    } else {
                        "without a SUMMARY.brf only index.brf is built; add one that lists every page"
                            .to_string()
                    };
                    report.add_site_error(
                        &rel,
                        render_site_error(
                            &page_src,
                            *span,
                            &format!("`{}` exists but is not part of the site", r.target_path),
                            "this link would be dead",
                            &help,
                        ),
                    );
                }
            }
            pages.push(CheckedPage {
                entry,
                doc,
                out_path,
            });
        }
        if report.has_errors() || (self.deny_warnings && report.warnings > 0) {
            return Err(report.into_error(&out));
        }

        // 5. Render. Display titles come from each page's H1 so the sidebar
        //    shows the page's actual title rather than the SUMMARY.brf text.
        let mut display_titles: BTreeMap<PathBuf, String> = BTreeMap::new();
        for page in &pages {
            display_titles.insert(
                page.entry.path.clone(),
                derive_page_title(&page.doc, &page.entry.title),
            );
        }
        let sidebar_html = render_sidebar(&entries, &display_titles);
        let stylesheet_url = theme::asset_url(&self.config.site.base_url, "style.css");
        let reload = if self.inject_reload {
            theme::RELOAD_CLIENT_JS
        } else {
            ""
        };
        let site_title = theme::site_title(&self.config).to_string();
        let mut rendered: Vec<(&Path, String)> = Vec::with_capacity(pages.len());
        for page in &pages {
            // Content hrefs are written as the emitter produced them: `@ref`
            // links are page-relative, so they hold under any deploy base.
            let body_html = html::render(&page.doc, &project.registry);
            let title = derive_page_title(&page.doc, &page.entry.title);
            let description = first_paragraph_text(&page.doc);
            let ctx = PageContext {
                title: &title,
                description: &description,
                site_title: &site_title,
                content_html: &body_html,
                sidebar_html: &sidebar_html,
                base_url: &self.config.site.base_url,
                stylesheet_path: &stylesheet_url,
                reload_script: reload,
            };
            rendered.push((
                page.out_path.as_path(),
                theme::render_page(&theme.page_template, &ctx),
            ));
        }

        // 6. Write: clear the previous output, then pages, stylesheet, assets.
        //    Pages were rendered first so the server's window onto a
        //    half-written dist/ stays short.
        clear_dir(&out)?;
        for (out_path, html) in &rendered {
            if let Some(p) = out_path.parent() {
                fs::create_dir_all(p)
                    .map_err(|e| format!("cannot create {}: {}", p.display(), e))?;
            }
            fs::write(out_path, html)
                .map_err(|e| format!("cannot write {}: {}", out_path.display(), e))?;
        }
        fs::write(out.join("style.css"), &theme.stylesheet)
            .map_err(|e| format!("cannot write style.css: {}", e))?;

        // 7. Asset copy: every non-.brf file under src/ is copied verbatim.
        copy_assets(&src, &out)?;

        Ok(BuildOutcome {
            pages_built: pages.len(),
            warnings: report.warnings_rendered,
            project_index,
        })
    }
}

/// Diagnostics gathered across the site, rendered as they are found so each
/// keeps its own file's positions.
#[derive(Default)]
struct Report {
    /// Every rendered diagnostic, in file order.
    rendered: Vec<String>,
    warnings_rendered: Vec<String>,
    errors: usize,
    warnings: usize,
    /// Source-relative paths of files with at least one error, and of
    /// files with at least one warning.
    failed: Vec<String>,
    warned: Vec<String>,
}

impl Report {
    fn add(&mut self, rel: &str, src: &SourceMap, diags: &[Diagnostic]) {
        for d in diags {
            let text = render_all(std::slice::from_ref(d), src);
            match d.severity {
                Severity::Error => self.note_error(rel),
                Severity::Warning => {
                    self.warnings += 1;
                    push_unique(&mut self.warned, rel);
                    self.warnings_rendered.push(text.clone());
                }
            }
            self.rendered.push(text);
        }
    }

    fn add_site_error(&mut self, rel: &str, text: String) {
        self.note_error(rel);
        self.rendered.push(text);
    }

    fn note_error(&mut self, rel: &str) {
        self.errors += 1;
        push_unique(&mut self.failed, rel);
    }

    fn has_errors(&self) -> bool {
        self.errors > 0
    }

    fn into_error(self, out: &Path) -> BuildError {
        let message = if self.errors > 0 {
            format!(
                "{} in {}; {} was left unchanged",
                plural(self.errors, "error"),
                self.failed.join(", "),
                out.display()
            )
        } else {
            format!(
                "{} in {} denied by --deny-warnings; {} was left unchanged",
                plural(self.warnings, "warning"),
                self.warned.join(", "),
                out.display()
            )
        };
        BuildError {
            report: self.rendered.join("\n"),
            message,
        }
    }
}

fn push_unique(list: &mut Vec<String>, rel: &str) {
    if !list.iter().any(|f| f == rel) {
        list.push(rel.to_string());
    }
}

fn plural(n: usize, noun: &str) -> String {
    if n == 1 {
        format!("1 {}", noun)
    } else {
        format!("{} {}s", n, noun)
    }
}

/// Render a site-level error rustc-style, the way `diag::render` renders a
/// compiler diagnostic. These are about how the site is assembled, not the
/// language, so they carry no `B` code.
fn render_site_error(
    src: &SourceMap,
    span: Span,
    message: &str,
    label: &str,
    help: &str,
) -> String {
    let (line, col) = src.line_col(span.start);
    let line_text = src.line_text(line);
    let pad = " ".repeat(col - 1);
    let remaining = line_text.chars().count().saturating_sub(col - 1).max(1);
    let carets = "^".repeat((span.len as usize).clamp(1, remaining));
    format!(
        "error: {message}\n  --> {path}:{line}:{col}\n   |\n{line:>3} | {line_text}\n   | {pad}{carets} {label}\n   |\n   = help: {help}\n",
        path = src.path,
    )
}

fn load_source(path: &Path) -> Result<SourceMap, String> {
    let text = read_source(path).map_err(|e| format!("cannot read {}: {}", path.display(), e))?;
    Ok(SourceMap::new(path.to_string_lossy(), text))
}

/// The forward-slash, source-relative key `ProjectIndex` and
/// `ResolvedRef::target_path` use for a page.
fn rel_key(path: &Path) -> String {
    path.components()
        .map(|c| c.as_os_str().to_string_lossy())
        .collect::<Vec<_>>()
        .join("/")
}

/// `rel` joined onto the output directory, refused unless it stays inside.
/// Page paths have already passed the resolver's target rules; this is the
/// backstop for anything that slips past them, and for asset paths.
fn output_path(out: &Path, rel: &Path) -> Result<PathBuf, String> {
    let mut parts = rel.components().peekable();
    if parts.peek().is_some() && parts.all(|c| matches!(c, Component::Normal(_))) {
        Ok(out.join(rel))
    } else {
        Err(format!(
            "refusing to write `{}`: it is not a path inside {}",
            rel.display(),
            out.display()
        ))
    }
}

/// Empty `out`, creating it if needed.
fn clear_dir(out: &Path) -> Result<(), String> {
    if !out.exists() {
        return fs::create_dir_all(out)
            .map_err(|e| format!("cannot create {}: {}", out.display(), e));
    }
    // Remove the *contents* rather than the directory itself — wiping `out`
    // would emit IN_DELETE/IN_CREATE on the project root on Linux, which
    // inotify can echo back into our file watcher (notify watches a file by
    // watching its parent dir) and trigger an infinite rebuild loop.
    for entry in fs::read_dir(out).map_err(|e| format!("cannot read {}: {}", out.display(), e))? {
        let entry = entry.map_err(|e| e.to_string())?;
        let path = entry.path();
        let ft = entry.file_type().map_err(|e| e.to_string())?;
        if ft.is_dir() {
            fs::remove_dir_all(&path)
                .map_err(|e| format!("cannot clear {}: {}", path.display(), e))?;
        } else {
            fs::remove_file(&path)
                .map_err(|e| format!("cannot clear {}: {}", path.display(), e))?;
        }
    }
    Ok(())
}

fn source_path_to_url(path: &Path) -> String {
    let stripped = path
        .to_string_lossy()
        .replace('\\', "/")
        .trim_end_matches(".brf")
        .to_string();
    if stripped == "index" {
        "index.html".to_string()
    } else if stripped.ends_with("/index") {
        format!("{}.html", stripped)
    } else {
        format!("{}.html", stripped)
    }
}

fn render_sidebar(entries: &[Entry], titles: &BTreeMap<PathBuf, String>) -> String {
    let mut out = String::new();
    out.push_str("<nav aria-label=\"site navigation\">\n");
    let mut in_list = false;
    for e in entries {
        match e {
            Entry::Section(name) => {
                if in_list {
                    out.push_str("</ul>\n");
                    in_list = false;
                }
                out.push_str("<h3>");
                out.push_str(&escape_html(name));
                out.push_str("</h3>\n");
            }
            Entry::Page(p) => {
                if !in_list {
                    out.push_str("<ul>\n");
                    in_list = true;
                }
                render_page_li(p, titles, &mut out);
            }
        }
    }
    if in_list {
        out.push_str("</ul>\n");
    }
    out.push_str("</nav>\n");
    out
}

fn render_page_li(p: &PageEntry, titles: &BTreeMap<PathBuf, String>, out: &mut String) {
    let title = titles
        .get(&p.path)
        .cloned()
        .unwrap_or_else(|| p.title.clone());
    out.push_str("<li>");
    out.push_str("<a href=\"");
    out.push_str(&escape_attr(&format!("/{}", source_path_to_url(&p.path))));
    out.push_str("\">");
    out.push_str(&escape_html(&title));
    out.push_str("</a>");
    if !p.children.is_empty() {
        out.push_str("<ul>");
        for c in &p.children {
            render_page_li(c, titles, out);
        }
        out.push_str("</ul>");
    }
    out.push_str("</li>\n");
}

fn copy_assets(src: &Path, out: &Path) -> Result<(), String> {
    fn walk(dir: &Path, src_root: &Path, out_root: &Path) -> Result<(), String> {
        let rd = fs::read_dir(dir).map_err(|e| format!("cannot read {}: {}", dir.display(), e))?;
        for entry in rd {
            let entry = entry.map_err(|e| e.to_string())?;
            let path = entry.path();
            let file_type = entry.file_type().map_err(|e| e.to_string())?;
            if file_type.is_dir() {
                walk(&path, src_root, out_root)?;
            } else {
                let name = path.file_name().and_then(|s| s.to_str()).unwrap_or("");
                if name.ends_with(".brf") || name == "SUMMARY.brf" {
                    continue;
                }
                let rel = path
                    .strip_prefix(src_root)
                    .map_err(|e| format!("strip_prefix: {}", e))?;
                let dest = output_path(out_root, rel)?;
                if let Some(p) = dest.parent() {
                    fs::create_dir_all(p)
                        .map_err(|e| format!("cannot create {}: {}", p.display(), e))?;
                }
                fs::copy(&path, &dest).map_err(|e| {
                    format!("cannot copy {} → {}: {}", path.display(), dest.display(), e)
                })?;
            }
        }
        Ok(())
    }
    walk(src, src, out)
}

fn derive_page_title(doc: &Document, fallback: &str) -> String {
    for b in &doc.blocks {
        if let Block::Heading { content, level, .. } = b {
            if *level == 1 {
                let t = summary::inline_text(content).trim().to_string();
                if !t.is_empty() {
                    return t;
                }
            }
        }
    }
    fallback.to_string()
}

fn first_paragraph_text(doc: &Document) -> String {
    for b in &doc.blocks {
        if let Block::Paragraph { content, .. } = b {
            let mut t = summary::inline_text(content).trim().to_string();
            // Trim to a reasonable meta-description length. Escaping happens
            // at the template boundary (theme::render_page), not here.
            if t.len() > 200 {
                let mut cut = 200;
                while !t.is_char_boundary(cut) {
                    cut -= 1;
                }
                t.truncate(cut);
                t.push('…');
            }
            if !t.is_empty() {
                return t;
            }
        }
    }
    String::new()
}

fn escape_html(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            _ => out.push(c),
        }
    }
    out
}

fn escape_attr(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            _ => out.push(c),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn write(p: &Path, content: &str) {
        if let Some(parent) = p.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(p, content).unwrap();
    }

    #[test]
    fn build_resolves_cross_page_ref_to_html() {
        let td = tempfile::TempDir::new().unwrap();
        let project = td.path();
        // book.toml
        write(
            &project.join("book.toml"),
            "[book]\ntitle = \"Test\"\n[build]\nsrc = \"src\"\nout = \"dist\"\n[site]\nbase_url = \"/\"\n",
        );
        // src/SUMMARY.brf
        write(
            &project.join("src/SUMMARY.brf"),
            "# Summary\n\n- @ref[index.brf](Home)\n- @ref[other.brf](Other)\n",
        );
        write(
            &project.join("src/index.brf"),
            "# Home\n\nSee @ref[other.brf#sec](other section).\n",
        );
        write(
            &project.join("src/other.brf"),
            "# Other Title\n\n## Section {#sec}\n\nbody.\n",
        );

        let builder = Builder::new(project).unwrap();
        let outcome = builder.build().unwrap();
        assert!(outcome.pages_built >= 2);
        assert!(
            outcome.warnings.is_empty(),
            "expected no warnings on happy path; got: {:?}",
            outcome.warnings,
        );
        let index_html = fs::read_to_string(project.join("dist/index.html")).unwrap();
        // Root-level pages link each other by bare file name; the href is
        // not rewritten with base_url.
        assert!(
            index_html.contains("href=\"other.html#sec\""),
            "expected a page-relative href; got: {}",
            index_html,
        );
        assert!(
            index_html.contains(">other section<"),
            "expected display text in anchor; got: {}",
            index_html
        );
    }

    const BOOK: &str = "[book]\ntitle = \"Test\"\n";

    /// A project with `book.toml` and the given files under it.
    fn site(files: &[(&str, &str)]) -> tempfile::TempDir {
        let td = tempfile::TempDir::new().unwrap();
        write(&td.path().join("book.toml"), BOOK);
        for (path, content) in files {
            write(&td.path().join(path), content);
        }
        td
    }

    fn build_err(project: &Path) -> BuildError {
        match Builder::new(project).unwrap().build() {
            Ok(_) => panic!("expected the build to fail"),
            Err(e) => e,
        }
    }

    #[test]
    fn errors_fail_the_build_with_positions_and_leave_output_untouched() {
        let td = site(&[
            (
                "src/SUMMARY.brf",
                "# Summary\n\n- @ref[index.brf](Home)\n- @ref[guide.brf](Guide)\n",
            ),
            ("src/index.brf", "# Home\n\n@bogus[x]\n"),
            ("src/guide.brf", "# Guide\n\nSee @link[the spec].\n"),
            ("dist/marker", "previous build"),
        ]);
        let err = build_err(td.path());
        // Every failing page is reported, each at its own line and column.
        assert!(err.report.contains("error[B0401]"), "{}", err.report);
        assert!(err.report.contains("src/index.brf:3:1"), "{}", err.report);
        assert!(err.report.contains("error[B0403]"), "{}", err.report);
        assert!(err.report.contains("src/guide.brf:3:5"), "{}", err.report);
        assert!(
            err.message.starts_with("2 errors in index.brf, guide.brf"),
            "{}",
            err.message
        );
        assert!(td.path().join("dist/marker").exists());
        assert!(!td.path().join("dist/index.html").exists());
    }

    #[test]
    fn warnings_render_with_positions_and_can_be_denied() {
        let td = site(&[(
            "src/index.brf",
            "# Home\n\n@callout(kind: info)\nBody.\n@end\n",
        )]);
        let outcome = Builder::new(td.path()).unwrap().build().unwrap();
        assert_eq!(outcome.warnings.len(), 1, "{:?}", outcome.warnings);
        assert!(outcome.warnings[0].contains("warning[B0408]"));
        assert!(outcome.warnings[0].contains("src/index.brf:3:1"));

        fs::remove_dir_all(td.path().join("dist")).unwrap();
        let mut builder = Builder::new(td.path()).unwrap();
        builder.deny_warnings = true;
        let err = match builder.build() {
            Ok(_) => panic!("warnings should fail under deny_warnings"),
            Err(e) => e,
        };
        assert!(err.report.contains("warning[B0408]"), "{}", err.report);
        assert!(
            err.message.contains("1 warning in index.brf"),
            "{}",
            err.message
        );
        assert!(!td.path().join("dist/index.html").exists());
    }

    #[test]
    fn failed_rebuild_keeps_the_last_good_output() {
        let td = site(&[("src/index.brf", "# Home\n\nfirst version\n")]);
        let builder = Builder::new(td.path()).unwrap();
        builder.build().unwrap();
        write(
            &td.path().join("src/index.brf"),
            "# Home\n\n*unterminated\n",
        );
        assert!(builder.build().is_err());
        let html = fs::read_to_string(td.path().join("dist/index.html")).unwrap();
        assert!(html.contains("first version"), "{}", html);
    }

    #[test]
    fn broken_file_outside_the_site_does_not_fail_the_build() {
        let td = site(&[
            ("src/SUMMARY.brf", "# Summary\n\n- @ref[index.brf](Home)\n"),
            ("src/index.brf", "# Home\n\nText.\n"),
            ("src/wip.brf", "*unfinished\n"),
        ]);
        let outcome = Builder::new(td.path()).unwrap().build().unwrap();
        assert_eq!(outcome.pages_built, 1);
        assert!(outcome.warnings.is_empty(), "{:?}", outcome.warnings);
    }

    #[test]
    fn ref_into_a_broken_file_fails_at_the_ref() {
        let td = site(&[
            ("src/SUMMARY.brf", "# Summary\n\n- @ref[index.brf](Home)\n"),
            ("src/index.brf", "# Home\n\nSee @ref[wip.brf](the draft).\n"),
            ("src/wip.brf", "*unfinished\n"),
        ]);
        let err = build_err(td.path());
        assert!(err.report.contains("error[B0605]"), "{}", err.report);
        assert!(err.report.contains("src/index.brf:3:5"), "{}", err.report);
        // The draft's own errors stay out of it: it is not a site page.
        assert!(!err.report.contains("wip.brf:1"), "{}", err.report);
    }

    #[test]
    fn ref_to_a_file_outside_the_site_is_an_error() {
        let td = site(&[
            ("src/SUMMARY.brf", "# Summary\n\n- @ref[index.brf](Home)\n"),
            (
                "src/index.brf",
                "# Home\n\nSee @ref[draft.brf](the draft).\n",
            ),
            ("src/draft.brf", "# Draft\n\nFine on its own.\n"),
        ]);
        let err = build_err(td.path());
        assert!(
            err.report
                .contains("error: `draft.brf` exists but is not part of the site"),
            "{}",
            err.report
        );
        assert!(err.report.contains("src/index.brf:3:5"), "{}", err.report);
        assert!(
            err.report.contains("help: add `draft.brf` to SUMMARY.brf"),
            "{}",
            err.report
        );
        assert!(!td.path().join("dist/index.html").exists());
    }

    #[test]
    fn single_page_mode_rejects_refs_to_siblings() {
        let td = site(&[
            ("src/index.brf", "# Home\n\nSee @ref[other.brf](other).\n"),
            ("src/other.brf", "# Other\n"),
        ]);
        let err = build_err(td.path());
        assert!(
            err.report
                .contains("`other.brf` exists but is not part of the site"),
            "{}",
            err.report
        );
    }

    #[test]
    fn duplicate_summary_entry_is_an_error_not_a_panic() {
        let td = site(&[
            (
                "src/SUMMARY.brf",
                "# Summary\n\n- @ref[index.brf](Home)\n- @ref[index.brf](Again)\n",
            ),
            ("src/index.brf", "# Home\n"),
        ]);
        let err = build_err(td.path());
        assert!(
            err.report
                .contains("error: `index.brf` is listed in SUMMARY.brf more than once"),
            "{}",
            err.report
        );
        assert!(err.report.contains("src/SUMMARY.brf:4:3"), "{}", err.report);
    }

    #[test]
    fn content_hrefs_are_left_as_emitted() {
        let td = site(&[
            (
                "book.toml",
                "[book]\ntitle = \"T\"\n[site]\nbase_url = \"/docs/\"\n\
                 [shortcodes.bad]\nkind = \"inline\"\n\
                 template_html = \"<a href=\\\"{{content}}>open</a>\"\n",
            ),
            (
                "src/index.brf",
                "# Home\n\n@link[mail](mailto:a@example.com) and @bad[x]\n",
            ),
        ]);
        let outcome = Builder::new(td.path()).unwrap().build().unwrap();
        assert_eq!(outcome.pages_built, 1);
        let html = fs::read_to_string(td.path().join("dist/index.html")).unwrap();
        // An unclosed quote as the last one on the page used to panic the
        // href rewriter.
        assert!(html.contains("<a href=\"x>open</a>"), "{}", html);
        assert!(html.contains("href=\"mailto:a@example.com\""), "{}", html);
    }

    #[test]
    fn summary_targets_cannot_leave_src() {
        let td = tempfile::TempDir::new().unwrap();
        let outside = td.path().join("escape.brf");
        let project = td.path().join("site");
        let absolute = outside.to_string_lossy().into_owned();
        for target in [
            "../escape.brf",
            absolute.as_str(),
            "sub\\\\escape.brf",
            "notes.md",
        ] {
            let _ = fs::remove_dir_all(&project);
            write(&outside, "# Outside\n");
            write(&project.join("book.toml"), BOOK);
            write(
                &project.join("src/SUMMARY.brf"),
                &format!(
                    "# Summary\n\n- @ref[index.brf](Home)\n- @ref[{}](X)\n",
                    target
                ),
            );
            write(&project.join("src/index.brf"), "# Home\n");
            write(&project.join("src/notes.md"), "# Notes\n");
            let err = build_err(&project);
            assert!(
                err.report.contains("error[B0603]"),
                "{}: {}",
                target,
                err.report
            );
            assert!(
                err.report.contains("src/SUMMARY.brf:4:3"),
                "{}: {}",
                target,
                err.report
            );
            assert!(!td.path().join("escape.html").exists(), "{}", target);
            assert!(!project.join("escape.html").exists(), "{}", target);
            assert!(!project.join("dist/notes.md.html").exists(), "{}", target);
        }
    }

    #[test]
    fn output_paths_must_stay_inside_the_output_dir() {
        let out = Path::new("/site/dist");
        assert_eq!(
            output_path(out, Path::new("a/b.html")).unwrap(),
            out.join("a/b.html")
        );
        for rel in ["", "../x.html", "a/../../x.html", "./x.html", "/etc/x.html"] {
            assert!(output_path(out, Path::new(rel)).is_err(), "{:?}", rel);
        }
    }
}
