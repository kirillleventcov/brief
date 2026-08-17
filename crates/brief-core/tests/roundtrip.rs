//! Round-trip corpus test for the Brief→Markdown converter.
//!
//! Every Brief document shipped in this repository (the docs book and the
//! LearnXinYminutes tour) must convert to Markdown that the Markdown→Brief
//! converter accepts and the compiler parses cleanly — the same invariant
//! `brief convert doc.brf` enforces via its strict self-test.

use brief::convert::{convert, to_markdown};
use brief::diag::Severity;
use brief::lexer::lex;
use brief::parser::parse;
use brief::resolve::resolve;
use brief::shortcode::Registry;
use brief::span::SourceMap;
use std::path::{Path, PathBuf};

fn collect_brf(dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(dir).expect("read corpus dir") {
        let path = entry.expect("dir entry").path();
        if path.is_dir() {
            collect_brf(&path, out);
        } else if path.extension().and_then(|s| s.to_str()) == Some("brf") {
            out.push(path);
        }
    }
}

#[test]
fn repo_brief_corpus_round_trips_through_markdown() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let mut files = vec![root.join("LearnXinYminutes.brf")];
    collect_brf(&root.join("docs/src"), &mut files);
    assert!(
        files.len() > 20,
        "corpus unexpectedly small: {} files",
        files.len()
    );

    let reg = Registry::with_builtins();
    for file in files {
        let text = std::fs::read_to_string(&file).expect("read corpus file");
        let src = SourceMap::new(file.to_string_lossy(), text);
        let tokens =
            lex(&src).unwrap_or_else(|d| panic!("{} does not lex: {:?}", file.display(), d));
        let (mut doc, parse_diags) = parse(tokens, &src);
        let parse_errors: Vec<_> = parse_diags
            .iter()
            .filter(|d| d.severity == Severity::Error)
            .collect();
        assert!(
            parse_errors.is_empty(),
            "{} does not parse: {:?}",
            file.display(),
            parse_errors
        );
        // Resolver diagnostics are ignored on purpose: outside a project
        // `@ref` raises B0604, and the docs' custom shortcodes are not in
        // the builtin registry. Neither stops the emitter.
        let _ = resolve(&mut doc, &reg);

        let md = to_markdown(&doc, &reg, &src);
        let back = convert(&md.markdown, "roundtrip.md");
        let src2 = SourceMap::new("roundtrip.brf", back.brief_source.clone());
        let tokens2 = lex(&src2).unwrap_or_else(|d| {
            panic!(
                "{}: round-tripped Brief does not lex: {:?}\n--- markdown ---\n{}",
                file.display(),
                d,
                md.markdown
            )
        });
        let (_, diags2) = parse(tokens2, &src2);
        let errors: Vec<_> = diags2
            .iter()
            .filter(|d| d.severity == Severity::Error)
            .collect();
        assert!(
            errors.is_empty(),
            "{}: round-tripped Brief does not parse: {:?}\n--- markdown ---\n{}\n--- brief ---\n{}",
            file.display(),
            errors,
            md.markdown,
            back.brief_source
        );
    }
}
