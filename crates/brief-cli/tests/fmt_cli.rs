//! End-to-end CLI tests for `brief fmt`.
//!
//! These exercise the binary via `std::process::Command`. Per-test temp dirs
//! keep artifacts isolated.

use std::path::PathBuf;
use std::process::Command;

fn brief_bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_brief"))
}

fn temp_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("brief-fmt-cli-{}", name));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

#[test]
fn default_mode_prints_to_stdout_for_single_file() {
    let dir = temp_dir("stdout_single");
    let input = dir.join("doc.brf");
    std::fs::write(&input, "hello   \n\n\nworld\n").unwrap();
    let out = Command::new(brief_bin())
        .arg("fmt")
        .arg(&input)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "stderr: {:?}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8(out.stdout).unwrap();
    assert_eq!(stdout, "hello\n\nworld\n");
    // The original file is unchanged in default mode.
    let on_disk = std::fs::read_to_string(&input).unwrap();
    assert_eq!(on_disk, "hello   \n\n\nworld\n");
}

#[test]
fn write_mode_rewrites_file_in_place() {
    let dir = temp_dir("write_in_place");
    let input = dir.join("doc.brf");
    std::fs::write(&input, "hello   \n\n\nworld\n").unwrap();
    let status = Command::new(brief_bin())
        .arg("fmt")
        .arg("--write")
        .arg(&input)
        .status()
        .unwrap();
    assert!(status.success());
    let on_disk = std::fs::read_to_string(&input).unwrap();
    assert_eq!(on_disk, "hello\n\nworld\n");
}

#[test]
fn check_mode_exit_zero_when_clean() {
    let dir = temp_dir("check_clean");
    let input = dir.join("doc.brf");
    std::fs::write(&input, "hello\n").unwrap();
    let out = Command::new(brief_bin())
        .arg("fmt")
        .arg("--check")
        .arg(&input)
        .output()
        .unwrap();
    assert!(out.status.success());
    assert!(out.stderr.is_empty());
    assert!(out.stdout.is_empty());
}

#[test]
fn check_mode_exit_one_when_dirty_and_lists_path() {
    let dir = temp_dir("check_dirty");
    let input = dir.join("doc.brf");
    std::fs::write(&input, "hello   \n").unwrap();
    let out = Command::new(brief_bin())
        .arg("fmt")
        .arg("--check")
        .arg(&input)
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(1));
    // The diff (including the file path) is printed to stdout, not stderr.
    let stdout = String::from_utf8(out.stdout).unwrap();
    assert!(
        stdout.contains(input.to_str().unwrap()),
        "expected stdout to contain file path {:?}, got {:?}",
        input,
        stdout
    );
    // File is left untouched in check mode.
    let on_disk = std::fs::read_to_string(&input).unwrap();
    assert_eq!(on_disk, "hello   \n");
}

#[test]
fn check_mode_dirty_file_prints_unified_diff() {
    let dir = temp_dir("check_diff_content");
    let input = dir.join("doc.brf");
    std::fs::write(&input, "hello   \n").unwrap();
    let out = Command::new(brief_bin())
        .arg("fmt")
        .arg("--check")
        .arg(&input)
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(1));
    let stdout = String::from_utf8(out.stdout).unwrap();
    // A unified diff must include a hunk header, a removal line, and an addition line.
    assert!(
        stdout.contains("@@"),
        "expected hunk header '@@' in diff output, got:\n{}",
        stdout
    );
    assert!(
        stdout
            .lines()
            .any(|l| l.starts_with('-') && !l.starts_with("---")),
        "expected a '-' diff line in output, got:\n{}",
        stdout
    );
    assert!(
        stdout
            .lines()
            .any(|l| l.starts_with('+') && !l.starts_with("+++")),
        "expected a '+' diff line in output, got:\n{}",
        stdout
    );
    // The --- / +++ header lines must contain the file path.
    assert!(
        stdout.contains(input.to_str().unwrap()),
        "expected file path in diff header, got:\n{}",
        stdout
    );
}

#[test]
fn check_mode_clean_file_no_diff_output() {
    let dir = temp_dir("check_clean_nodiff");
    let input = dir.join("doc.brf");
    std::fs::write(&input, "hello\n").unwrap();
    let out = Command::new(brief_bin())
        .arg("fmt")
        .arg("--check")
        .arg(&input)
        .output()
        .unwrap();
    assert!(out.status.success(), "exit code should be 0 for clean file");
    assert!(
        out.stdout.is_empty(),
        "expected no stdout output for clean file, got: {:?}",
        String::from_utf8_lossy(&out.stdout)
    );
}

#[test]
fn check_and_write_mutually_exclusive() {
    let dir = temp_dir("flags_conflict");
    let input = dir.join("doc.brf");
    std::fs::write(&input, "hello\n").unwrap();
    let out = Command::new(brief_bin())
        .arg("fmt")
        .arg("--check")
        .arg("--write")
        .arg(&input)
        .output()
        .unwrap();
    assert!(
        !out.status.success(),
        "expected failure when both flags set"
    );
}

#[test]
fn write_mode_handles_multiple_files_independently() {
    let dir = temp_dir("write_multi");
    let a = dir.join("a.brf");
    let b = dir.join("b.brf");
    std::fs::write(&a, "a   \n").unwrap();
    std::fs::write(&b, "b\n").unwrap();
    let status = Command::new(brief_bin())
        .arg("fmt")
        .arg("--write")
        .arg(&a)
        .arg(&b)
        .status()
        .unwrap();
    assert!(status.success());
    assert_eq!(std::fs::read_to_string(&a).unwrap(), "a\n");
    assert_eq!(std::fs::read_to_string(&b).unwrap(), "b\n");
}

#[test]
fn default_mode_with_multiple_files_is_rejected() {
    let dir = temp_dir("stdout_multi_reject");
    let a = dir.join("a.brf");
    let b = dir.join("b.brf");
    std::fs::write(&a, "a\n").unwrap();
    std::fs::write(&b, "b\n").unwrap();
    let out = Command::new(brief_bin())
        .arg("fmt")
        .arg(&a)
        .arg(&b)
        .output()
        .unwrap();
    assert!(!out.status.success());
    assert_eq!(out.status.code(), Some(2));
}

#[test]
fn check_mode_aggregates_across_files() {
    let dir = temp_dir("check_multi");
    let clean = dir.join("clean.brf");
    let dirty = dir.join("dirty.brf");
    std::fs::write(&clean, "ok\n").unwrap();
    std::fs::write(&dirty, "needs   \n").unwrap();
    let out = Command::new(brief_bin())
        .arg("fmt")
        .arg("--check")
        .arg(&clean)
        .arg(&dirty)
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(1));
    // Diffs are printed to stdout; dirty files appear in diff headers.
    let stdout = String::from_utf8(out.stdout).unwrap();
    assert!(stdout.contains(dirty.to_str().unwrap()));
    assert!(!stdout.contains(clean.to_str().unwrap()));
}

#[test]
fn sort_frontmatter_flag_reorders_top_level_keys() {
    let dir = temp_dir("sort_fm");
    let input = dir.join("doc.brf");
    std::fs::write(&input, "+++\nz = 1\na = 2\n+++\n# Doc\n").unwrap();
    let out = Command::new(brief_bin())
        .arg("fmt")
        .arg("--sort-frontmatter")
        .arg(&input)
        .output()
        .unwrap();
    assert!(out.status.success());
    let stdout = String::from_utf8(out.stdout).unwrap();
    let a_pos = stdout.find("a = ").unwrap();
    let z_pos = stdout.find("z = ").unwrap();
    assert!(a_pos < z_pos, "frontmatter not reordered: {:?}", stdout);
}

#[test]
fn missing_input_file_reports_and_exits_two() {
    let dir = temp_dir("missing");
    let input = dir.join("nope.brf");
    let out = Command::new(brief_bin())
        .arg("fmt")
        .arg("--write")
        .arg(&input)
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(2));
    let stderr = String::from_utf8(out.stderr).unwrap();
    assert!(stderr.contains("nope.brf"));
}

#[test]
fn formatter_is_idempotent_on_real_repo_doc() {
    // The flagship doc shipped with the repo should round-trip through the
    // formatter without any further change after the first pass. This is
    // the broadest "real input" smoke test we can run.
    let learn = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join("LearnXinYminutes.brf");
    let raw = std::fs::read_to_string(&learn).unwrap();
    let dir = temp_dir("idempotent_learn");
    let staging = dir.join("learn.brf");
    std::fs::write(&staging, &raw).unwrap();

    // Format once → capture.
    let pass1 = Command::new(brief_bin())
        .arg("fmt")
        .arg(&staging)
        .output()
        .unwrap();
    assert!(
        pass1.status.success(),
        "fmt failed on LearnXinYminutes: {:?}",
        String::from_utf8_lossy(&pass1.stderr)
    );
    let pass1_out = String::from_utf8(pass1.stdout).unwrap();

    // Write the formatted output back, re-format it, expect bit-identical.
    std::fs::write(&staging, &pass1_out).unwrap();
    let pass2 = Command::new(brief_bin())
        .arg("fmt")
        .arg(&staging)
        .output()
        .unwrap();
    assert!(pass2.status.success());
    let pass2_out = String::from_utf8(pass2.stdout).unwrap();
    assert_eq!(pass1_out, pass2_out, "fmt is not idempotent on real doc");
}
