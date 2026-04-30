use crate::ast::{Block, Document, Inline};
use crate::diag::{Code, Diagnostic};
use crate::shortcode::{ArgType, ArgValue, Registry, ShortKindOpt, Shortcode};
use crate::span::Span;

pub fn resolve(doc: &mut Document, registry: &Registry) -> Vec<Diagnostic> {
    let mut diags = Vec::new();
    for block in &mut doc.blocks {
        resolve_block(block, registry, &mut diags);
    }
    diags
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
                    "info" => (Some("note"), Some("`kind: info` is deprecated; use `kind: note`")),
                    "danger" => (
                        Some("caution"),
                        Some("`kind: danger` is deprecated; use `kind: caution`"),
                    ),
                    _ => (None, None),
                };
                if let (Some(canonical), Some(msg)) = (canonical, label_msg) {
                    diags.push(
                        Diagnostic::warning(Code::DeprecatedCalloutKind, span).label(msg),
                    );
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
        if let Some(spec) = sc.arguments.get(kw) {
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
