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
fn compile_llm_minifies_json_block() {
    let dir = temp_dir("minify_json");
    let input = dir.join("doc.brf");
    std::fs::write(
        &input,
        "# Doc\n\n```json\n{\n  \"a\": 1,\n  \"b\": [1, 2, 3]\n}\n```\n",
    )
    .unwrap();
    let out = Command::new(brief_bin())
        .arg("compile")
        .arg(&input)
        .arg("--target=llm")
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("{\"a\":1,\"b\":[1,2,3]}"),
        "stdout={}",
        stdout
    );
}

#[test]
fn compile_llm_invalid_json_warns_but_succeeds() {
    let dir = temp_dir("invalid_json");
    let input = dir.join("doc.brf");
    std::fs::write(&input, "```json\n{ not valid }\n```\n").unwrap();
    let out = Command::new(brief_bin())
        .arg("compile")
        .arg(&input)
        .arg("--target=llm")
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "compile must not fail on invalid JSON"
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("B0701"), "stderr={}", stderr);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("{ not valid }"),
        "verbatim body kept: {}",
        stdout
    );
}

#[test]
fn compile_llm_unknown_attr_is_error() {
    let dir = temp_dir("unknown_attr");
    let input = dir.join("doc.brf");
    std::fs::write(&input, "```json @bogus\n{}\n```\n").unwrap();
    let out = Command::new(brief_bin())
        .arg("compile")
        .arg(&input)
        .arg("--target=llm")
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(1), "compile must fail");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("B0315"), "stderr={}", stderr);
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

#[test]
fn compile_w_refuses_to_overwrite_input() {
    let dir = temp_dir("compile_w_selfclobber");
    let input = dir.join("note.txt");
    std::fs::write(&input, "# Hello\n\nContent.\n").unwrap();
    // --target=llm derives note.txt as the output path — the input itself.
    let out = Command::new(brief_bin())
        .arg("compile")
        .arg(&input)
        .arg("--target=llm")
        .arg("-w")
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(2));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("refusing to overwrite input"), "{}", stderr);
    let content = std::fs::read_to_string(&input).unwrap();
    assert_eq!(content, "# Hello\n\nContent.\n", "source must be untouched");
}

#[test]
fn compile_o_refuses_output_equal_to_input() {
    let dir = temp_dir("compile_o_selfclobber");
    let input = dir.join("note.brf");
    std::fs::write(&input, "# Hello\n").unwrap();
    let out = Command::new(brief_bin())
        .arg("compile")
        .arg(&input)
        .arg("-o")
        .arg(&input)
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(2));
    assert_eq!(std::fs::read_to_string(&input).unwrap(), "# Hello\n");
}

#[test]
fn compile_w_recompile_over_existing_output_is_fine() {
    let dir = temp_dir("compile_w_recompile");
    let input = dir.join("note.brf");
    std::fs::write(&input, "# Hello\n").unwrap();
    for _ in 0..2 {
        let status = Command::new(brief_bin())
            .arg("compile")
            .arg(&input)
            .arg("--target=llm")
            .arg("-w")
            .status()
            .unwrap();
        assert!(status.success());
    }
    assert!(dir.join("note.txt").exists());
}

#[test]
fn compile_unknown_config_key_is_an_error() {
    let dir = temp_dir("strict_config");
    std::fs::write(dir.join("brief.toml"), "[compile]\ntypo_key = true\n").unwrap();
    let input = dir.join("d.brf");
    std::fs::write(&input, "# Hi\n").unwrap();
    let out = Command::new(brief_bin())
        .current_dir(&dir)
        .arg("compile")
        .arg("d.brf")
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(2));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("bad brief.toml"), "{}", stderr);
}

#[test]
fn compile_respects_default_target_from_config() {
    let dir = temp_dir("default_target");
    std::fs::write(
        dir.join("brief.toml"),
        "[compile]\ndefault_target = \"llm\"\n",
    )
    .unwrap();
    let input = dir.join("d.brf");
    std::fs::write(&input, "# Hi\n\nBody text.\n").unwrap();
    let out = Command::new(brief_bin())
        .current_dir(&dir)
        .arg("compile")
        .arg("d.brf")
        .output()
        .unwrap();
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        !stdout.contains("<h1>"),
        "llm target must not emit HTML: {}",
        stdout
    );
    assert!(stdout.contains("# Hi"), "{}", stdout);
}

#[test]
fn compile_explicit_target_overrides_config_default() {
    let dir = temp_dir("default_target_override");
    std::fs::write(
        dir.join("brief.toml"),
        "[compile]\ndefault_target = \"llm\"\n",
    )
    .unwrap();
    let input = dir.join("d.brf");
    std::fs::write(&input, "# Hi\n").unwrap();
    let out = Command::new(brief_bin())
        .current_dir(&dir)
        .arg("compile")
        .arg("d.brf")
        .arg("--target=html")
        .output()
        .unwrap();
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("<h1>"), "{}", stdout);
}

#[test]
fn explain_covers_every_diagnostic_code() {
    for code in brief::diag::Code::ALL {
        let out = Command::new(brief_bin())
            .arg("explain")
            .arg(code.as_str())
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "`brief explain {}` returned unknown-code",
            code.as_str()
        );
    }
}
