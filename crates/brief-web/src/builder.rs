//! Build pipeline: walk source tree, compile each `.brf` to HTML, wrap in the
//! theme template, write to `dist/`.

use brief::ast::{Block, Document, Inline};
use brief::config::registry_from;
use brief::diag::{Severity, render_all};
use brief::emit::html;
use brief::lexer;
use brief::parser;
use brief::resolve;
use brief::shortcode::Registry;
use brief::span::SourceMap;
use brief::validate;

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use crate::config::BookConfig;
use crate::page::{self, SiteIndex};
use crate::summary::{self, Entry, PageEntry};
use crate::theme::{self, PageContext, Theme};

/// Outcome of a build: how many pages produced, plus any non-fatal warnings.
pub struct BuildOutcome {
    pub pages_built: usize,
    pub warnings: Vec<String>,
    pub site_index: SiteIndex,
}

#[derive(Clone)]
pub struct Builder {
    pub project_dir: PathBuf,
    pub config: BookConfig,
    /// When true, the page template embeds the live-reload SSE client.
    pub inject_reload: bool,
}

impl Builder {
    pub fn new(project_dir: impl AsRef<Path>) -> Result<Self, String> {
        let project_dir = project_dir.as_ref().to_path_buf();
        let config = BookConfig::load_from_dir(&project_dir)?;
        Ok(Builder {
            project_dir,
            config,
            inject_reload: false,
        })
    }

    pub fn from_config(project_dir: impl AsRef<Path>, config: BookConfig) -> Self {
        Builder {
            project_dir: project_dir.as_ref().to_path_buf(),
            config,
            inject_reload: false,
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

    pub fn build(&self) -> Result<BuildOutcome, String> {
        let src = self.src_dir();
        if !src.exists() {
            return Err(format!(
                "source directory does not exist: {}",
                src.display()
            ));
        }
        let out = self.out_dir();
        if out.exists() {
            // Wipe the previous output so stale files don't leak into the new
            // build. Only ever inside the project's own dist/.
            fs::remove_dir_all(&out)
                .map_err(|e| format!("cannot clear {}: {}", out.display(), e))?;
        }
        fs::create_dir_all(&out).map_err(|e| format!("cannot create {}: {}", out.display(), e))?;

        let theme = Theme::load(self.theme_dir().as_deref())?;
        let mut warnings = Vec::new();

        // 1. Build the registry (built-ins + user shortcodes + @page).
        let mut brief_cfg = brief::config::Config::default();
        brief_cfg.shortcodes = self.config.shortcodes.clone();
        let mut registry: Registry = registry_from(&brief_cfg);
        page::register(&mut registry);

        // 2. Decide between single-page and multi-page mode.
        let summary_path = src.join("SUMMARY.brf");
        let entries: Vec<Entry> = if summary_path.exists() {
            let summary_doc = compile_to_doc(&summary_path, &registry)?;
            summary::extract(&summary_doc)?
        } else {
            // Single-page mode: synthesize an entry for index.brf.
            let index = src.join("index.brf");
            if !index.exists() {
                return Err(format!(
                    "neither SUMMARY.brf nor index.brf was found under {}",
                    src.display()
                ));
            }
            vec![Entry::Page(PageEntry {
                path: PathBuf::from("index.brf"),
                title: theme::site_title(&self.config).to_string(),
                children: Vec::new(),
            })]
        };

        // 3. Collect every page entry (flattened) so we can build the index.
        let mut flat: Vec<PageEntry> = Vec::new();
        for e in &entries {
            if let Entry::Page(p) = e {
                flat.extend(p.flatten().iter().map(|r| (*r).clone()));
            }
        }
        if flat.is_empty() {
            return Err("SUMMARY.brf contains no @page entries".into());
        }

        // 4. First pass — parse every page to gather anchors and decide URLs.
        let mut docs: BTreeMap<PathBuf, Document> = BTreeMap::new();
        let mut index = SiteIndex::default();
        index.base_url = self.config.site.base_url.clone();
        for entry in &flat {
            let abs = src.join(&entry.path);
            if !abs.exists() {
                return Err(format!(
                    "SUMMARY.brf points to {} but the file does not exist",
                    abs.display()
                ));
            }
            let doc = compile_to_doc(&abs, &registry)?;
            let anchors = page::collect_anchors(&doc);
            index.anchors.insert(entry.path.clone(), anchors);
            index
                .pages
                .insert(entry.path.clone(), source_path_to_url(&entry.path));
            docs.insert(entry.path.clone(), doc);
        }

        // 5. Second pass — rewrite @page references and emit HTML.
        // Compute display titles from each doc's H1 so the sidebar shows the
        // page's actual title rather than whatever was in SUMMARY.brf.
        let mut display_titles: BTreeMap<PathBuf, String> = BTreeMap::new();
        for entry in &flat {
            if let Some(doc) = docs.get(&entry.path) {
                display_titles.insert(entry.path.clone(), derive_page_title(doc, &entry.title));
            }
        }
        let sidebar_html = render_sidebar(&entries, &display_titles);
        let stylesheet_url = theme::asset_url(&self.config.site.base_url, "style.css");
        let reload = if self.inject_reload {
            theme::RELOAD_CLIENT_JS
        } else {
            ""
        };
        let site_title = theme::site_title(&self.config).to_string();
        for entry in &flat {
            let mut doc = docs.remove(&entry.path).expect("doc was inserted");
            let errs = page::rewrite(&mut doc, &index, &entry.path);
            for e in errs {
                warnings.push(format!("{}", e));
            }
            let body_html = html::render(&doc, &registry);
            let title = derive_page_title(&doc, &entry.title);
            let description = first_paragraph_text(&doc);
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
            let rendered = theme::render_page(&theme.page_template, &ctx);
            let out_path = out.join(source_path_to_url(&entry.path));
            if let Some(p) = out_path.parent() {
                fs::create_dir_all(p)
                    .map_err(|e| format!("cannot create {}: {}", p.display(), e))?;
            }
            fs::write(&out_path, &rendered)
                .map_err(|e| format!("cannot write {}: {}", out_path.display(), e))?;
        }

        // 6. Stylesheet.
        fs::write(out.join("style.css"), &theme.stylesheet)
            .map_err(|e| format!("cannot write style.css: {}", e))?;

        // 7. Asset copy: every non-.brf file under src/ is copied verbatim.
        copy_assets(&src, &out)?;

        Ok(BuildOutcome {
            pages_built: flat.len(),
            warnings,
            site_index: index,
        })
    }
}

fn compile_to_doc(path: &Path, registry: &Registry) -> Result<Document, String> {
    let raw =
        fs::read_to_string(path).map_err(|e| format!("cannot read {}: {}", path.display(), e))?;
    let raw = raw.strip_prefix('\u{feff}').unwrap_or(&raw).to_string();
    let src = SourceMap::new(path.to_string_lossy(), raw);
    let tokens = lexer::lex(&src).map_err(|d| render_all(&d, &src))?;
    let (mut doc, mut diags) = parser::parse(tokens, &src);
    diags.extend(resolve::resolve(&mut doc, registry));
    diags.extend(validate::validate(
        &doc,
        &validate::ValidateOpts::default(),
        &src,
    ));
    if diags.iter().any(|d| d.severity == Severity::Error) {
        return Err(render_all(&diags, &src));
    }
    Ok(doc)
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
                let dest = out_root.join(rel);
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
            // Trim to a reasonable meta-description length.
            if t.len() > 200 {
                t.truncate(200);
                t.push('…');
            }
            if !t.is_empty() {
                return escape_attr(&t);
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

#[allow(dead_code)]
fn _unused(_: &Inline) {}
