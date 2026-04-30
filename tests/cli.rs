//! End-to-end CLI tests for `brief convert`.
//!
//! Each test invokes the compiled binary via `std::process::Command`. Slower
//! than unit tests; one round-trip per case. We run them inside a per-test
//! temp dir so artifacts don't leak between tests.

use std::path::PathBuf;
use std::process::Command;

fn brief_bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_brief"))
}

fn temp_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("brief-cli-test-{}", name));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

#[test]
fn single_file_default_output() {
    let dir = temp_dir("single_default");
    let input = dir.join("note.md");
    std::fs::write(&input, "# Hello\n").unwrap();
    let status = Command::new(brief_bin())
        .arg("convert")
        .arg(&input)
        .status()
        .unwrap();
    assert!(status.success());
    let out = std::fs::read_to_string(dir.join("note.brf")).unwrap();
    assert_eq!(out, "# Hello\n");
}

#[test]
fn single_file_explicit_output() {
    let dir = temp_dir("single_explicit");
    let input = dir.join("note.md");
    std::fs::write(&input, "# Hello\n").unwrap();
    let dest = dir.join("custom.brf");
    let status = Command::new(brief_bin())
        .arg("convert")
        .arg(&input)
        .arg("-o")
        .arg(&dest)
        .status()
        .unwrap();
    assert!(status.success());
    let out = std::fs::read_to_string(&dest).unwrap();
    assert_eq!(out, "# Hello\n");
}

#[test]
fn multiple_inputs_with_output_flag_rejected() {
    let dir = temp_dir("multi_with_o");
    let a = dir.join("a.md");
    let b = dir.join("b.md");
    std::fs::write(&a, "# A\n").unwrap();
    std::fs::write(&b, "# B\n").unwrap();
    let out = Command::new(brief_bin())
        .arg("convert")
        .arg(&a)
        .arg(&b)
        .arg("-o")
        .arg("x.brf")
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(2));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("-o cannot be used with multiple inputs"));
}

#[test]
fn stdout_mode() {
    let dir = temp_dir("stdout_mode");
    let input = dir.join("note.md");
    std::fs::write(&input, "# Hello\n").unwrap();
    let out = Command::new(brief_bin())
        .arg("convert")
        .arg(&input)
        .arg("--stdout")
        .output()
        .unwrap();
    assert!(out.status.success());
    assert_eq!(String::from_utf8_lossy(&out.stdout), "# Hello\n");
}

#[test]
fn refuses_to_overwrite_without_force() {
    let dir = temp_dir("no_overwrite");
    let input = dir.join("note.md");
    let dest = dir.join("note.brf");
    std::fs::write(&input, "# Hello\n").unwrap();
    std::fs::write(&dest, "existing content").unwrap();
    let out = Command::new(brief_bin())
        .arg("convert")
        .arg(&input)
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("FAILED"));
    assert!(stderr.contains("--force"));
    assert_eq!(std::fs::read_to_string(&dest).unwrap(), "existing content");
}

#[test]
fn force_overwrites() {
    let dir = temp_dir("force_overwrite");
    let input = dir.join("note.md");
    let dest = dir.join("note.brf");
    std::fs::write(&input, "# Hello\n").unwrap();
    std::fs::write(&dest, "old").unwrap();
    let status = Command::new(brief_bin())
        .arg("convert")
        .arg(&input)
        .arg("--force")
        .status()
        .unwrap();
    assert!(status.success());
    assert_eq!(std::fs::read_to_string(&dest).unwrap(), "# Hello\n");
}

#[test]
fn compile_md_without_convert_errors() {
    let dir = temp_dir("compile_md_no_convert");
    let input = dir.join("note.md");
    // Use a Markdown-only construct that wouldn't survive Brief's strict lex.
    std::fs::write(&input, "**bold here**\n").unwrap();
    let out = Command::new(brief_bin())
        .arg("compile")
        .arg(&input)
        .arg("--target=llm")
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(2));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("--convert"),
        "expected stderr to suggest --convert, got: {}",
        stderr
    );
}

#[test]
fn compile_md_with_convert_succeeds() {
    let dir = temp_dir("compile_md_with_convert");
    let input = dir.join("note.md");
    std::fs::write(&input, "# Hello\n\n**bold here**\n").unwrap();
    let out = Command::new(brief_bin())
        .arg("compile")
        .arg(&input)
        .arg("--target=llm")
        .arg("--convert")
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "compile --convert failed: stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("Hello"));
    assert!(stdout.contains("bold here"));
    // Holes are reported on stderr but don't fail the compile.
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("double-emphasis"), "stderr={}", stderr);
}

#[test]
fn compile_brf_with_convert_flag_still_works() {
    // Passing --convert to a non-Markdown input runs the converter on Brief
    // source. Plain ASCII text round-trips, so the compile still succeeds.
    let dir = temp_dir("compile_brf_with_convert");
    let input = dir.join("doc.brf");
    std::fs::write(&input, "# Hello\n").unwrap();
    let out = Command::new(brief_bin())
        .arg("compile")
        .arg(&input)
        .arg("--target=llm")
        .arg("--convert")
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "compile --convert on .brf failed: stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn batch_continues_past_failure() {
    let dir = temp_dir("batch_continue");
    let good = dir.join("good.md");
    let bad = dir.join("missing.md");
    let good2 = dir.join("good2.md");
    std::fs::write(&good, "# G\n").unwrap();
    std::fs::write(&good2, "# G2\n").unwrap();
    // bad is intentionally not written
    let out = Command::new(brief_bin())
        .arg("convert")
        .arg(&good)
        .arg(&bad)
        .arg(&good2)
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("missing.md → FAILED"));
    assert!(stderr.contains("good.md → "));
    assert!(stderr.contains("good2.md → "));
    assert!(stderr.contains("2 of 3 files converted"));
    assert!(dir.join("good.brf").exists());
    assert!(dir.join("good2.brf").exists());
}
