use crate::ast::{Block, Document, Inline};
use crate::diag::{Code, Diagnostic};
use crate::project::ProjectIndex;
use crate::shortcode::{ArgType, ArgValue, Registry, ShortKindOpt, Shortcode};
use crate::span::Span;

pub struct ResolveProject<'a> {
    pub index: &'a ProjectIndex,
    pub current: &'a std::path::Path,
}

/// Resolve without project context. Equivalent to
/// `resolve_with_project(.., None)`. `@ref` invocations error with
/// `B0604` when no project root is available.
pub fn resolve(doc: &mut Document, registry: &Registry) -> Vec<Diagnostic> {
    resolve_with_project(doc, registry, None)
}

pub fn resolve_with_project(
    doc: &mut Document,
    registry: &Registry,
    project: Option<&ResolveProject<'_>>,
) -> Vec<Diagnostic> {
    let mut diags = Vec::new();
    for block in &mut doc.blocks {
        resolve_block(block, registry, &mut diags);
    }
    let mut resolved = std::collections::BTreeMap::new();
    for block in &doc.blocks {
        scan_refs_block(block, project, &mut diags, &mut resolved);
    }
    doc.resolved_refs = resolved;
    diags
}

fn scan_refs_block(
    block: &Block,
    project: Option<&ResolveProject<'_>>,
    diags: &mut Vec<Diagnostic>,
    out: &mut std::collections::BTreeMap<crate::span::Span, crate::ast::ResolvedRef>,
) {
    match block {
        Block::Heading { content, .. } | Block::Paragraph { content, .. } => {
            for n in content {
                scan_refs_inline(n, project, diags, out);
            }
        }
        Block::List { items, .. } => {
            for it in items {
                for n in &it.content {
                    scan_refs_inline(n, project, diags, out);
                }
                for c in &it.children {
                    scan_refs_block(c, project, diags, out);
                }
            }
        }
        Block::Blockquote { children, .. } | Block::BlockShortcode { children, .. } => {
            for c in children {
                scan_refs_block(c, project, diags, out);
            }
        }
        Block::Table { header, rows, .. } => {
            for cell in &header.cells {
                for n in cell {
                    scan_refs_inline(n, project, diags, out);
                }
            }
            for row in rows {
                for cell in &row.cells {
                    for n in cell {
                        scan_refs_inline(n, project, diags, out);
                    }
                }
            }
        }
        Block::DefinitionList { items, .. } => {
            for it in items {
                for n in &it.term {
                    scan_refs_inline(n, project, diags, out);
                }
                for n in &it.definition {
                    scan_refs_inline(n, project, diags, out);
                }
            }
        }
        Block::CodeBlock { .. } | Block::HorizontalRule { .. } => {}
    }
}

fn scan_refs_inline(
    node: &crate::ast::Inline,
    project: Option<&ResolveProject<'_>>,
    diags: &mut Vec<Diagnostic>,
    out: &mut std::collections::BTreeMap<crate::span::Span, crate::ast::ResolvedRef>,
) {
    use crate::ast::Inline;
    match node {
        Inline::Bold { content, .. }
        | Inline::Italic { content, .. }
        | Inline::Underline { content, .. }
        | Inline::Strike { content, .. } => {
            for n in content {
                scan_refs_inline(n, project, diags, out);
            }
        }
        Inline::Shortcode {
            name,
            args,
            content,
            span,
        } if name == "ref" => {
            // Project context must exist before any other validation; outside
            // a project, B0604 is the actionable diagnostic regardless of
            // whether the body itself is malformed.
            let project = match project {
                Some(p) => p,
                None => {
                    diags.push(
                        Diagnostic::new(crate::diag::Code::RefNoProject, *span)
                            .label("`@ref` requires a `brief.toml`-rooted project")
                            .help("create a `brief.toml` at the project root to enable cross-document references"),
                    );
                    return;
                }
            };
            // Body must be a single Text node — no nested emphasis or shortcodes.
            let body_text = match content.as_deref() {
                Some([Inline::Text { value, .. }]) => Some(value.clone()),
                Some([]) | None => None,
                _ => {
                    diags.push(
                        Diagnostic::new(crate::diag::Code::RefBadTarget, *span)
                            .label("@ref body must be a plain path; emphasis and nested shortcodes are not allowed"),
                    );
                    return;
                }
            };
            let Some(body_text) = body_text else {
                diags.push(
                    Diagnostic::new(crate::diag::Code::RefBadTarget, *span)
                        .label("@ref body cannot be empty"),
                );
                return;
            };
            let display = args
                .keyword
                .get("title")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            // (`title` is required at the registry level, so a missing one
            //  has already produced B0403; we still emit a reasonable display
            //  string so downstream emitters don't blow up.)
            match parse_target(&body_text) {
                Err(reason) => {
                    diags.push(
                        Diagnostic::new(crate::diag::Code::RefBadTarget, *span).label(reason),
                    );
                }
                Ok((path, anchor)) => match project.index.anchors.get(&path) {
                    None => {
                        let mut help_paths: Vec<&String> = project.index.anchors.keys().collect();
                        help_paths.sort();
                        let suggestion = help_paths
                            .iter()
                            .take(5)
                            .map(|p| p.as_str())
                            .collect::<Vec<_>>()
                            .join(", ");
                        diags.push(
                            Diagnostic::new(crate::diag::Code::RefMissingFile, *span)
                                .label(format!("file `{}` not found in project", path))
                                .help(if suggestion.is_empty() {
                                    "no `.brf` files were indexed under this project root"
                                        .to_string()
                                } else {
                                    format!("known files: {}", suggestion)
                                }),
                        );
                    }
                    Some(anchors) => {
                        if let Some(a) = &anchor
                            && !anchors.contains(a)
                        {
                            let mut all: Vec<&String> = anchors.iter().collect();
                            all.sort();
                            let listed = all
                                .iter()
                                .take(10)
                                .map(|s| s.as_str())
                                .collect::<Vec<_>>()
                                .join(", ");
                            diags.push(
                                Diagnostic::new(crate::diag::Code::RefMissingAnchor, *span)
                                    .label(format!("anchor `{}` not found in `{}`", a, path))
                                    .help(if listed.is_empty() {
                                        format!("`{}` defines no anchors", path)
                                    } else {
                                        format!("anchors in `{}`: {}", path, listed)
                                    }),
                            );
                            return;
                        }
                        out.insert(
                            *span,
                            crate::ast::ResolvedRef {
                                target_path: path.clone(),
                                target_anchor: anchor,
                                display,
                            },
                        );
                    }
                },
            }
        }
        Inline::Shortcode {
            content: Some(c), ..
        } => {
            for n in c {
                scan_refs_inline(n, project, diags, out);
            }
        }
        _ => {}
    }
}

fn parse_target(s: &str) -> Result<(String, Option<String>), String> {
    let s = s.trim();
    if s.is_empty() {
        return Err("@ref target cannot be empty".to_string());
    }
    if s.starts_with('/') {
        return Err("@ref target must be relative; leading `/` is not allowed".to_string());
    }
    if s.contains('\\') {
        return Err("@ref target must use `/` separators".to_string());
    }
    let (path_part, anchor_part) = match s.split_once('#') {
        Some((p, a)) => (p, Some(a)),
        None => (s, None),
    };
    if path_part.is_empty() {
        return Err("@ref target path cannot be empty".to_string());
    }
    for seg in path_part.split('/') {
        if seg.is_empty() || seg == "." || seg == ".." {
            return Err(format!("invalid path segment `{}`", seg));
        }
    }
    if !path_part.ends_with(".brf") {
        return Err("@ref target path must end in `.brf`".to_string());
    }
    let anchor = match anchor_part {
        None => None,
        Some(a) => {
            if a.is_empty() {
                return Err("@ref anchor after `#` cannot be empty".to_string());
            }
            if !a
                .chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
            {
                return Err(format!("invalid anchor `{}`; must match [a-z0-9-]+", a));
            }
            Some(a.to_string())
        }
    };
    Ok((path_part.to_string(), anchor))
}

fn resolve_block(block: &mut Block, reg: &Registry, diags: &mut Vec<Diagnostic>) {
    match block {
        Block::Heading { content, .. } | Block::Paragraph { content, .. } => {
            for n in content {
                resolve_inline(n, reg, diags);
            }
        }
        Block::List { items, .. } => {
            for it in items {
                for n in &mut it.content {
                    resolve_inline(n, reg, diags);
                }
                for c in &mut it.children {
                    resolve_block(c, reg, diags);
                }
            }
        }
        Block::Blockquote { children, .. } => {
            for c in children {
                resolve_block(c, reg, diags);
            }
        }
        Block::CodeBlock { .. } | Block::HorizontalRule { .. } => {}
        Block::Table {
            args,
            header,
            rows,
            span,
        } => {
            check_shortcode("t", args, reg, true, *span, diags);
            for cell in &mut header.cells {
                for n in cell {
                    resolve_inline(n, reg, diags);
                }
            }
            for row in rows {
                for cell in &mut row.cells {
                    for n in cell {
                        resolve_inline(n, reg, diags);
                    }
                }
            }
        }
        Block::DefinitionList { items, .. } => {
            for it in items {
                for n in &mut it.term {
                    resolve_inline(n, reg, diags);
                }
                for n in &mut it.definition {
                    resolve_inline(n, reg, diags);
                }
            }
        }
        Block::BlockShortcode {
            name,
            args,
            children,
            span,
        } => {
            check_shortcode(name, args, reg, true, *span, diags);
            for c in children {
                resolve_block(c, reg, diags);
            }
        }
    }
}

fn resolve_inline(node: &mut Inline, reg: &Registry, diags: &mut Vec<Diagnostic>) {
    match node {
        Inline::Bold { content, .. }
        | Inline::Italic { content, .. }
        | Inline::Underline { content, .. }
        | Inline::Strike { content, .. } => {
            for n in content {
                resolve_inline(n, reg, diags);
            }
        }
        Inline::Shortcode {
            name,
            args,
            content,
            span,
        } => {
            check_shortcode(name, args, reg, false, *span, diags);
            if let Some(c) = content {
                for n in c {
                    resolve_inline(n, reg, diags);
                }
            }
        }
        _ => {}
    }
}

fn check_shortcode(
    name: &str,
    args: &mut crate::ast::ShortArgs,
    reg: &Registry,
    is_block: bool,
    span: Span,
    diags: &mut Vec<Diagnostic>,
) {
    // `@br` is deliberately not registered: Brief already has `\` as the
    // canonical hard-break sigil. Catch it explicitly so the user gets a
    // useful pointer instead of the generic "register it" help.
    if name == "br" {
        diags.push(
            Diagnostic::new(Code::UnknownShortcode, span)
                .label("`@br` is not a Brief shortcode".to_string())
                .help(
                    "use `\\` at end of line for a hard break (see §12); `@br` will not be registered as a built-in",
                ),
        );
        return;
    }
    let Some(sc) = reg.get(name) else {
        diags.push(
            Diagnostic::new(Code::UnknownShortcode, span)
                .label(format!("shortcode `{}` is not registered", name))
                .help("register it in `brief.toml` under `[shortcodes.<name>]`"),
        );
        return;
    };
    let form_ok = matches!(
        (&sc.kind, is_block),
        (ShortKindOpt::Block, true) | (ShortKindOpt::Inline, false) | (ShortKindOpt::Both, _)
    );
    if !form_ok {
        diags.push(Diagnostic::new(Code::FormMismatch, span).label(format!(
            "`{}` was used as {} but is registered as {:?}",
            name,
            if is_block { "block" } else { "inline" },
            sc.kind
        )));
    }

    // Deprecation alias rewrite for @callout(kind:).
    // Must run before the oneof check so the canonical value passes validation.
    if name == "callout" {
        if let Some(v) = args.keyword.get("kind") {
            if let Some(s) = v.as_str() {
                let (canonical, label_msg): (Option<&str>, Option<&str>) = match s {
                    "info" => (
                        Some("note"),
                        Some("`kind: info` is deprecated; use `kind: note`"),
                    ),
                    "danger" => (
                        Some("caution"),
                        Some("`kind: danger` is deprecated; use `kind: caution`"),
                    ),
                    _ => (None, None),
                };
                if let (Some(canonical), Some(msg)) = (canonical, label_msg) {
                    diags.push(Diagnostic::warning(Code::DeprecatedCalloutKind, span).label(msg));
                    args.keyword
                        .insert("kind".into(), ArgValue::Str(canonical.into()));
                }
            }
        }
    }

    bind_positional(sc, args, span, diags);
    typecheck_args(sc, args, span, diags);
    for (kw, spec) in &sc.arguments {
        if spec.required && !args.keyword.contains_key(kw) {
            diags.push(
                Diagnostic::new(Code::MissingArg, span)
                    .label(format!("missing required argument `{}` for `{}`", kw, name)),
            );
        }
        if let (Some(allowed), Some(v)) = (&spec.oneof, args.keyword.get(kw)) {
            if let Some(s) = v.as_str() {
                if !allowed.iter().any(|a| a == s) {
                    diags.push(Diagnostic::new(Code::BadEnumValue, span).label(format!(
                        "`{}` is not in {{{}}}",
                        s,
                        allowed.join(", ")
                    )));
                }
            }
        }
    }
}

fn bind_positional(
    sc: &Shortcode,
    args: &mut crate::ast::ShortArgs,
    span: Span,
    diags: &mut Vec<Diagnostic>,
) {
    let positional = std::mem::take(&mut args.positional);
    for (i, v) in positional.into_iter().enumerate() {
        let pos = i + 1;
        let bound = sc.arguments.iter().find(|(_, s)| s.position == Some(pos));
        if let Some((kw, _)) = bound {
            args.keyword.insert(kw.clone(), v);
        } else {
            diags.push(Diagnostic::new(Code::BadArgSyntax, span).label(format!(
                "positional argument #{} has no `position = {}` mapping",
                pos, pos
            )));
        }
    }
}

fn typecheck_args(
    sc: &Shortcode,
    args: &crate::ast::ShortArgs,
    span: Span,
    diags: &mut Vec<Diagnostic>,
) {
    for (kw, v) in &args.keyword {
        let Some(spec) = sc.arguments.get(kw) else {
            let mut d = Diagnostic::new(Code::UnknownArg, span)
                .label(format!("no argument named `{}` on this shortcode", kw));
            if !sc.arguments.is_empty() {
                let known: Vec<&str> = sc.arguments.keys().map(|s| s.as_str()).collect();
                d = d.help(format!("declared arguments: {}", known.join(", ")));
            }
            diags.push(d);
            continue;
        };
        if !type_matches(&spec.ty, v) {
            diags.push(Diagnostic::new(Code::ArgTypeMismatch, span).label(format!(
                "argument `{}` has type {} but expected {:?}",
                kw,
                v.type_name(),
                spec.ty
            )));
        }
    }
}

fn type_matches(t: &ArgType, v: &ArgValue) -> bool {
    matches!(
        (t, v),
        (ArgType::String, ArgValue::Str(_))
            | (ArgType::String, ArgValue::Ident(_))
            | (ArgType::Int, ArgValue::Int(_))
            | (ArgType::Ident, ArgValue::Ident(_))
            | (ArgType::Array, ArgValue::Array(_))
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::project::ProjectIndex;
    use std::collections::BTreeSet;
    use std::path::PathBuf;

    fn parse_only(src: &str) -> crate::ast::Document {
        use crate::{lexer, parser, span::SourceMap};
        let src = SourceMap::new("t.brf", src);
        let tokens = lexer::lex(&src).expect("lex");
        let (doc, _) = parser::parse(tokens, &src);
        doc
    }

    #[test]
    fn resolved_refs_starts_empty() {
        let doc = parse_only("hello\n");
        assert!(doc.resolved_refs.is_empty());
    }

    #[test]
    fn unknown_keyword_arg_is_b0409() {
        let mut doc = parse_only("@details(summary: \"s\", bogus: 1)\nx\n@end\n");
        let reg = crate::shortcode::Registry::with_builtins();
        let diags = resolve(&mut doc, &reg);
        assert!(
            diags.iter().any(|d| d.code == Code::UnknownArg),
            "diags: {:?}",
            diags
        );
    }

    #[test]
    fn declared_keyword_arg_passes() {
        let mut doc = parse_only("@details(summary: \"s\")\nx\n@end\n");
        let reg = crate::shortcode::Registry::with_builtins();
        let diags = resolve(&mut doc, &reg);
        assert!(
            diags.iter().all(|d| d.code != Code::UnknownArg),
            "diags: {:?}",
            diags
        );
    }

    #[test]
    fn resolve_project_records_valid_ref() {
        let mut doc = parse_only("See @ref[other.brf#x](Other).\n");
        let mut idx = ProjectIndex {
            root: PathBuf::from("/tmp/proj"),
            ..Default::default()
        };
        idx.anchors
            .insert("other.brf".to_string(), BTreeSet::from(["x".into()]));
        let project = ResolveProject {
            index: &idx,
            current: &PathBuf::from("here.brf"),
        };
        let reg = crate::shortcode::Registry::with_builtins();
        let diags = resolve_with_project(&mut doc, &reg, Some(&project));
        assert!(
            diags
                .iter()
                .all(|d| d.severity != crate::diag::Severity::Error),
            "diags: {:?}",
            diags,
        );
        assert_eq!(doc.resolved_refs.len(), 1);
    }

    use crate::diag::Code;

    fn run_resolve(
        brief: &str,
        current: &str,
        files: &[(&str, &[&str])],
    ) -> (crate::ast::Document, Vec<crate::diag::Diagnostic>) {
        let mut doc = parse_only(brief);
        let mut idx = ProjectIndex {
            root: PathBuf::from("/tmp/p"),
            ..Default::default()
        };
        for (path, anchors) in files {
            idx.anchors.insert(
                path.to_string(),
                anchors.iter().map(|s| s.to_string()).collect(),
            );
        }
        let project = ResolveProject {
            index: &idx,
            current: &PathBuf::from(current),
        };
        let reg = crate::shortcode::Registry::with_builtins();
        let diags = resolve_with_project(&mut doc, &reg, Some(&project));
        (doc, diags)
    }

    fn has(diags: &[crate::diag::Diagnostic], c: Code) -> bool {
        diags.iter().any(|d| d.code == c)
    }

    #[test]
    fn ref_to_unknown_file_is_b0601() {
        let (_, diags) = run_resolve(
            "See @ref[missing.brf#x](Foo).\n",
            "here.brf",
            &[("here.brf", &[])],
        );
        assert!(has(&diags, Code::RefMissingFile), "{:?}", diags);
    }

    #[test]
    fn ref_to_unknown_anchor_is_b0602() {
        let (_, diags) = run_resolve(
            "See @ref[other.brf#missing](Foo).\n",
            "here.brf",
            &[("other.brf", &["present"])],
        );
        assert!(has(&diags, Code::RefMissingAnchor), "{:?}", diags);
    }

    #[test]
    fn ref_with_dot_dot_is_b0603() {
        let (_, diags) = run_resolve(
            "See @ref[../escape.brf](Foo).\n",
            "here.brf",
            &[("here.brf", &[])],
        );
        assert!(has(&diags, Code::RefBadTarget), "{:?}", diags);
    }

    #[test]
    fn ref_without_brf_extension_is_b0603() {
        let (_, diags) = run_resolve(
            "See @ref[no-extension](Foo).\n",
            "here.brf",
            &[("here.brf", &[])],
        );
        assert!(has(&diags, Code::RefBadTarget), "{:?}", diags);
    }

    #[test]
    fn ref_outside_project_is_b0604() {
        let mut doc = parse_only("@ref[a.brf](X)\n");
        let reg = crate::shortcode::Registry::with_builtins();
        let diags = resolve_with_project(&mut doc, &reg, None);
        assert!(has(&diags, Code::RefNoProject), "{:?}", diags);
    }

    #[test]
    fn valid_ref_records_in_resolved_refs() {
        let (doc, diags) = run_resolve(
            "See @ref[other.brf#x](Other).\n",
            "here.brf",
            &[("other.brf", &["x"])],
        );
        assert!(diags.iter().all(|d| d.code != Code::RefMissingFile
            && d.code != Code::RefMissingAnchor
            && d.code != Code::RefBadTarget
            && d.code != Code::RefNoProject));
        assert_eq!(doc.resolved_refs.len(), 1);
        let (_span, rr) = doc.resolved_refs.iter().next().unwrap();
        assert_eq!(rr.target_path, "other.brf");
        assert_eq!(rr.target_anchor.as_deref(), Some("x"));
        assert_eq!(rr.display, "Other");
    }

    #[test]
    fn ref_to_self_is_allowed_when_anchor_exists() {
        let (doc, diags) = run_resolve(
            "## Top {#top}\n\nSee @ref[here.brf#top](Top).\n",
            "here.brf",
            &[("here.brf", &["top"])],
        );
        assert!(
            diags
                .iter()
                .all(|d| d.severity != crate::diag::Severity::Error),
            "diags: {:?}",
            diags,
        );
        assert_eq!(doc.resolved_refs.len(), 1);
    }

    #[test]
    fn ref_with_no_anchor_resolves_against_file_only() {
        let (doc, diags) = run_resolve(
            "See @ref[other.brf](Other).\n",
            "here.brf",
            &[("other.brf", &[])],
        );
        assert!(
            diags
                .iter()
                .all(|d| d.severity != crate::diag::Severity::Error),
            "{:?}",
            diags
        );
        assert_eq!(doc.resolved_refs.len(), 1);
        let (_span, rr) = doc.resolved_refs.iter().next().unwrap();
        assert!(rr.target_anchor.is_none());
    }

    #[test]
    fn empty_ref_body_outside_project_is_b0604_not_b0603() {
        let mut doc = parse_only("@ref[](X)\n");
        let reg = crate::shortcode::Registry::with_builtins();
        let diags = resolve_with_project(&mut doc, &reg, None);
        assert!(
            has(&diags, Code::RefNoProject),
            "expected B0604 first; got {:?}",
            diags
        );
        assert!(
            !has(&diags, Code::RefBadTarget),
            "B0603 should not fire when there is no project: {:?}",
            diags
        );
    }
}
