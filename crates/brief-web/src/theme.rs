//! Default theme — embedded HTML/CSS for the built-in renderer.
//!
//! Templates are intentionally trivial: simple `{{ name }}` substitution, no
//! conditionals or loops. Logic that wants to live in templates belongs in a
//! Brief shortcode instead (see design §5.5).

use crate::config::BookConfig;
use std::collections::BTreeMap;
use std::path::Path;

pub const DEFAULT_PAGE_TEMPLATE: &str = include_str!("../assets/page.html");
pub const DEFAULT_STYLESHEET: &str = include_str!("../assets/style.css");
pub const RELOAD_CLIENT_JS: &str = include_str!("../assets/reload.js");

/// Loads a theme from `theme_dir` if present, otherwise falls back to the
/// built-in defaults.
pub struct Theme {
    pub page_template: String,
    pub stylesheet: String,
}

impl Theme {
    pub fn load(theme_dir: Option<&Path>) -> Result<Self, String> {
        let mut t = Theme {
            page_template: DEFAULT_PAGE_TEMPLATE.to_string(),
            stylesheet: DEFAULT_STYLESHEET.to_string(),
        };
        if let Some(dir) = theme_dir {
            let p = dir.join("page.html");
            if p.exists() {
                t.page_template = std::fs::read_to_string(&p)
                    .map_err(|e| format!("cannot read theme template {}: {}", p.display(), e))?;
            }
            let c = dir.join("style.css");
            if c.exists() {
                t.stylesheet = std::fs::read_to_string(&c)
                    .map_err(|e| format!("cannot read theme stylesheet {}: {}", c.display(), e))?;
            }
        }
        Ok(t)
    }
}

/// Substitution context for theme templates.
pub struct PageContext<'a> {
    pub title: &'a str,
    pub description: &'a str,
    pub site_title: &'a str,
    pub content_html: &'a str,
    pub sidebar_html: &'a str,
    pub base_url: &'a str,
    pub stylesheet_path: &'a str,
    pub reload_script: &'a str,
}

pub fn render_page(template: &str, ctx: &PageContext<'_>) -> String {
    // Text-ish values (page H1, frontmatter description, site title, paths)
    // are escaped here, at the single template boundary — a page titled
    // `# x <script>…` must not become live markup in `<title>` or the
    // header. Only content/sidebar/reload_script are trusted HTML.
    let title = escape_text(ctx.title);
    let description = escape_text(ctx.description);
    let site_title = escape_text(ctx.site_title);
    let base_url = escape_text(ctx.base_url);
    let stylesheet = escape_text(ctx.stylesheet_path);
    let mut subs: BTreeMap<&str, &str> = BTreeMap::new();
    subs.insert("title", &title);
    subs.insert("description", &description);
    subs.insert("site_title", &site_title);
    subs.insert("content", ctx.content_html);
    subs.insert("sidebar", ctx.sidebar_html);
    subs.insert("base_url", &base_url);
    subs.insert("stylesheet", &stylesheet);
    subs.insert("reload_script", ctx.reload_script);
    expand(template, &subs)
}

/// Escape for both element-text and double-quoted-attribute positions —
/// theme templates use the same placeholder in either.
fn escape_text(s: &str) -> String {
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

fn expand(template: &str, subs: &BTreeMap<&str, &str>) -> String {
    let mut out = String::with_capacity(template.len() + 256);
    let mut rest = template;
    while let Some(open_idx) = rest.find("{{") {
        // Copy literal up to `{{`.
        out.push_str(&rest[..open_idx]);
        let after_open = &rest[open_idx + 2..];
        match after_open.find("}}") {
            Some(close_rel) => {
                let key = after_open[..close_rel].trim();
                if let Some(val) = subs.get(key) {
                    out.push_str(val);
                } else {
                    // Leave unknown placeholders untouched so typos are
                    // visible in the rendered output.
                    out.push_str("{{");
                    out.push_str(&after_open[..close_rel]);
                    out.push_str("}}");
                }
                rest = &after_open[close_rel + 2..];
            }
            None => {
                out.push_str("{{");
                out.push_str(after_open);
                rest = "";
                break;
            }
        }
    }
    out.push_str(rest);
    out
}

/// Convenience: produce the absolute URL of a built-in static asset under
/// `base_url`. We do not bother re-rooting absolute paths the user passes in.
pub fn asset_url(base_url: &str, name: &str) -> String {
    let mut out = String::with_capacity(base_url.len() + name.len() + 1);
    out.push_str(base_url);
    if !out.ends_with('/') {
        out.push('/');
    }
    out.push_str(name);
    out
}

/// Extracts a friendly site title for the default header. Falls back to the
/// project directory name if the user did not set one.
pub fn site_title(cfg: &BookConfig) -> &str {
    if cfg.book.title.is_empty() {
        "Brief site"
    } else {
        cfg.book.title.as_str()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn render_page_escapes_title_and_description() {
        let template = "<title>{{ title }} — {{ site_title }}</title>\n<meta content=\"{{ description }}\">\n<main>{{ content }}</main>";
        let ctx = PageContext {
            title: "x <script>alert(1)</script>",
            description: "a \"quoted\" & <desc>",
            site_title: "S<b>",
            content_html: "<p>real html stays</p>",
            sidebar_html: "",
            base_url: "/",
            stylesheet_path: "/style.css",
            reload_script: "",
        };
        let out = render_page(template, &ctx);
        assert!(!out.contains("<script>alert(1)</script>"), "{}", out);
        assert!(
            out.contains("x &lt;script&gt;alert(1)&lt;/script&gt;"),
            "{}",
            out
        );
        assert!(
            out.contains("a &quot;quoted&quot; &amp; &lt;desc&gt;"),
            "{}",
            out
        );
        assert!(out.contains("S&lt;b&gt;"), "{}", out);
        assert!(out.contains("<p>real html stays</p>"), "{}", out);
    }
}
