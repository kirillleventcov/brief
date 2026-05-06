use std::path::Path;
use std::process::Command;

fn brief_bin() -> std::path::PathBuf {
    // `cargo test` exposes `CARGO_BIN_EXE_brief` — the binary cargo
    // just built for this crate.
    std::path::PathBuf::from(env!("CARGO_BIN_EXE_brief"))
}

/// Convert a Markdown fixture and run the same self-test the CLI does
/// in strict mode. Returns `Ok(brief_source)` if the output compiled,
/// `Err(rendered_diagnostics)` otherwise.
fn convert_and_strict_check(md: &str, name: &str) -> Result<String, String> {
    let result = brief::convert::convert(md, name);
    let src = brief::span::SourceMap::new(name.to_string(), result.brief_source.clone());
    let diags = match brief::lexer::lex(&src) {
        Ok(toks) => brief::parser::parse(toks, &src).1,
        Err(d) => d,
    };
    let any_error = diags
        .iter()
        .any(|d| d.severity == brief::diag::Severity::Error);
    if any_error {
        Err(format!(
            "convert→compile failed for {}:\n{}\n--- brief output ---\n{}",
            name,
            brief::diag::render_all(&diags, &src),
            result.brief_source
        ))
    } else {
        Ok(result.brief_source)
    }
}

#[test]
fn corpus_strict_all_patterns_round_trip() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/strict");
    let mut checked = 0usize;
    for entry in std::fs::read_dir(&dir).expect("fixtures dir") {
        let path = entry.expect("dir entry").path();
        if path.extension().and_then(|e| e.to_str()) != Some("md") {
            continue;
        }
        let md = std::fs::read_to_string(&path).expect("read fixture");
        match convert_and_strict_check(&md, &path.to_string_lossy()) {
            Ok(_) => {}
            Err(report) => panic!("fixture {}: {}", path.display(), report),
        }
        checked += 1;
    }
    assert!(
        checked >= 5,
        "expected at least 5 fixtures, found {}",
        checked
    );
}

#[test]
fn cli_no_strict_flag_is_accepted() {
    // Flag-plumbing sanity check — confirms `--no-strict` parses and the
    // binary exits 0 on a known-good input. The actual contract that
    // strict mode rejects broken Brief is unit-tested in
    // `strict_tests::rejects_brief_that_does_not_compile` next to the
    // `strict_self_test` definition in `main.rs`.
    let dir = tempfile::tempdir().unwrap();
    let md_path = dir.path().join("ok.md");
    std::fs::write(&md_path, "# ok\n").unwrap();
    let out = Command::new(brief_bin())
        .arg("convert")
        .arg("--no-strict")
        .arg("--force")
        .arg(&md_path)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}
