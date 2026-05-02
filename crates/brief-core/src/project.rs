//! Project-wide compile state: root discovery and the cross-document
//! reference index.
//!
//! `@ref` validation is the first multi-pass element of the Brief
//! compiler. `discover_root` walks up the directory tree from a starting
//! point looking for `brief.toml`; `build_index` (Task 3) walks the
//! discovered project tree and collects the heading anchors that `@ref`
//! is allowed to point at.

use std::path::{Path, PathBuf};

/// Walk up from `start`, returning the first ancestor directory that
/// contains `brief.toml`. Returns `None` if no such ancestor exists.
///
/// `start` may be a file or directory. If a file, its parent is the
/// first directory considered. The search includes `start` itself if
/// `start` is a directory.
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

use std::collections::{BTreeMap, BTreeSet};

/// Diagnostics emitted while indexing one project file, paired with the
/// source so the consumer can render them at correct positions.
#[derive(Debug)]
pub struct FileDiagnostics {
    pub source: crate::span::SourceMap,
    pub diagnostics: Vec<crate::diag::Diagnostic>,
}

/// Snapshot of the project's `@ref` targets. Built once per compilation
/// (or watch tick). Read-only after construction.
#[derive(Clone, Debug, Default)]
pub struct ProjectIndex {
    /// Absolute, canonicalized project root (the directory holding `brief.toml`).
    pub root: PathBuf,
    /// Project-relative `.brf` path (forward-slash, ASCII) → set of heading anchors that exist in it.
    pub anchors: BTreeMap<String, BTreeSet<String>>,
}

/// Walk the project tree and collect heading anchors per file.
///
/// Skips:
///   * hidden directories (name starts with `.`),
///   * the `target/`, `dist/`, `node_modules/` directories.
///
/// Files that fail to lex or parse are still indexed with an empty anchor
/// set — references to anchors in such files surface as `B0602`. Returns
/// the index along with per-file diagnostics, each paired with the
/// `SourceMap` of the file that produced them so callers can render
/// diagnostics at the correct positions. The caller decides whether to
/// treat any errors as fatal.
pub fn build_index(root: &Path) -> (ProjectIndex, Vec<FileDiagnostics>) {
    use crate::lexer;
    use crate::parser;
    use crate::span::SourceMap;

    let mut idx = ProjectIndex {
        root: root.to_path_buf(),
        anchors: BTreeMap::new(),
    };
    let mut all_diags: Vec<FileDiagnostics> = Vec::new();

    walk_brf_files(root, &mut |abs_path: &Path| {
        let rel: String = match abs_path.strip_prefix(root) {
            Ok(r) => to_forward_slash(r),
            Err(_) => return,
        };
        let raw = match std::fs::read_to_string(abs_path) {
            Ok(s) => s,
            Err(_) => {
                idx.anchors.insert(rel, BTreeSet::new());
                return;
            }
        };
        let raw = raw.strip_prefix('\u{feff}').unwrap_or(&raw).to_string();
        let src = SourceMap::new(abs_path.to_string_lossy(), raw);
        let tokens = match lexer::lex(&src) {
            Ok(t) => t,
            Err(d) => {
                all_diags.push(FileDiagnostics {
                    source: src,
                    diagnostics: d,
                });
                idx.anchors.insert(rel, BTreeSet::new());
                return;
            }
        };
        let (doc, parse_diags) = parser::parse(tokens, &src);
        if !parse_diags.is_empty() {
            all_diags.push(FileDiagnostics {
                source: src.clone(),
                diagnostics: parse_diags,
            });
        }
        let anchors = collect_doc_anchors(&doc);
        idx.anchors.insert(rel, anchors);
    });

    (idx, all_diags)
}

fn walk_brf_files(root: &Path, visit: &mut dyn FnMut(&Path)) {
    let rd = match std::fs::read_dir(root) {
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

fn collect_doc_anchors(doc: &crate::ast::Document) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    for b in &doc.blocks {
        collect_block_anchors(b, &mut out);
    }
    out
}

fn collect_block_anchors(b: &crate::ast::Block, out: &mut BTreeSet<String>) {
    use crate::ast::Block;
    match b {
        Block::Heading {
            anchor: Some(a), ..
        } => {
            out.insert(a.clone());
        }
        Block::Heading { .. } => {}
        Block::Blockquote { children, .. } | Block::BlockShortcode { children, .. } => {
            for c in children {
                collect_block_anchors(c, out);
            }
        }
        Block::List { items, .. } => {
            for it in items {
                for c in &it.children {
                    collect_block_anchors(c, out);
                }
            }
        }
        _ => {}
    }
}

fn to_forward_slash(p: &Path) -> String {
    p.components()
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
        .collect::<Vec<_>>()
        .join("/")
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
        let (idx, diags) = build_index(root);
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
        let (idx, diags) = build_index(root);
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
    fn build_index_records_files_with_no_anchors_too() {
        let td = TempDir::new().unwrap();
        let root = td.path();
        touch(&root.join("brief.toml"));
        write(&root.join("plain.brf"), "Just a paragraph.\n");
        let (idx, diags) = build_index(root);
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
