//! End-to-end CLI tests for project discovery and the broken-file policy.
//!
//! Every command finds the `brief.toml` that governs its input by walking up
//! from the file, never from the working directory, and a broken file only
//! fails a compile that `@ref`s it.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use tempfile::TempDir;

fn brief_bin() -> &'static str {
    env!("CARGO_BIN_EXE_brief")
}

fn write(p: &Path, content: &str) {
    if let Some(parent) = p.parent() {
        fs::create_dir_all(parent).expect("create parent dirs");
    }
    fs::write(p, content).expect("write test file");
}

fn run_in(cwd: &Path, args: &[&str]) -> Output {
    Command::new(brief_bin())
        .current_dir(cwd)
        .args(args)
        .output()
        .expect("run brief")
}

fn stderr(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

fn stdout(out: &Output) -> String {
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// Temp layout built by [`proj_and_elsewhere`]. `dir` owns the temp
/// directory and deletes it on drop.
struct ProjAndElsewhere {
    dir: TempDir,
    proj: PathBuf,
    elsewhere: PathBuf,
}

/// `<td>/proj` with a custom `lede` shortcode and an llm default target,
/// next to `<td>/elsewhere`, whose `brief.toml` is broken. Running from
/// `elsewhere` proves the cwd plays no part.
fn proj_and_elsewhere() -> ProjAndElsewhere {
    let dir = TempDir::new().expect("create temp dir");
    let base = dir.path().canonicalize().expect("canonicalize temp dir");
    let proj = base.join("proj");
    let elsewhere = base.join("elsewhere");
    write(
        &proj.join("brief.toml"),
        "[compile]\ndefault_target = \"llm\"\n\n\
         [shortcodes.lede]\nkind = \"inline\"\ntemplate_html = \"<b>{{content}}</b>\"\n",
    );
    write(&proj.join("docs/guide.brf"), "# Guide\n\n@lede[hi] text.\n");
    write(&elsewhere.join("brief.toml"), "[compile]\nnope = 1\n");
    ProjAndElsewhere {
        dir,
        proj,
        elsewhere,
    }
}

#[test]
fn compile_uses_the_files_project_not_the_cwd() {
    let ProjAndElsewhere {
        dir: _dir,
        elsewhere,
        ..
    } = proj_and_elsewhere();
    let out = run_in(&elsewhere, &["compile", "../proj/docs/guide.brf"]);
    assert_eq!(out.status.code(), Some(0), "stderr: {}", stderr(&out));
    assert_eq!(stderr(&out), "");
    // The project's default_target (llm) and its shortcode both applied.
    let text = stdout(&out);
    assert!(!text.contains("<h1"), "{}", text);
    assert!(text.contains("hi"), "{}", text);
}

#[test]
fn a_cwd_brief_toml_does_not_configure_a_standalone_file() {
    let ProjAndElsewhere { dir, proj, .. } = proj_and_elsewhere();
    let lonely = dir.path().join("lonely/a.brf");
    write(&lonely, "@lede[hi]\n");
    let out = run_in(&proj, &["compile", lonely.to_str().unwrap()]);
    assert_eq!(out.status.code(), Some(1));
    let err = stderr(&out);
    assert!(
        err.starts_with("error[B0401]: shortcode is not registered\n"),
        "{}",
        err
    );
}

#[test]
fn config_flag_replaces_the_config_but_keeps_the_root() {
    let td = TempDir::new().unwrap();
    let root = td.path();
    write(&root.join("brief.toml"), "");
    write(&root.join("a.brf"), "# A {#top}\n");
    write(
        &root.join("b.brf"),
        "See @ref[a.brf#top](A) and @lede[x].\n",
    );
    let other = td.path().join("conf/other.toml");
    write(
        &other,
        "[shortcodes.lede]\nkind = \"inline\"\ntemplate_html = \"<i>{{content}}</i>\"\n",
    );
    let out = run_in(
        td.path(),
        &["compile", "b.brf", "--config", other.to_str().unwrap()],
    );
    assert_eq!(out.status.code(), Some(0), "stderr: {}", stderr(&out));
    let html = stdout(&out);
    assert!(html.contains("<a href=\"a.html#top\">A</a>"), "{}", html);
    assert!(html.contains("<i>x</i>"), "{}", html);
}

#[test]
fn compile_convert_uses_the_markdown_files_project() {
    let ProjAndElsewhere {
        dir: _dir,
        proj,
        elsewhere,
    } = proj_and_elsewhere();
    write(&proj.join("note.md"), "# Note\n\nBody.\n");
    let out = run_in(&elsewhere, &["compile", "--convert", "../proj/note.md"]);
    assert_eq!(out.status.code(), Some(0), "stderr: {}", stderr(&out));
    let text = stdout(&out);
    assert!(
        !text.contains("<h1"),
        "llm default target expected: {}",
        text
    );
    assert!(text.contains("Note"), "{}", text);
}

#[test]
fn reverse_convert_uses_the_files_project_not_the_cwd() {
    let ProjAndElsewhere {
        dir: _dir,
        elsewhere,
        ..
    } = proj_and_elsewhere();
    let out = run_in(
        &elsewhere,
        &["convert", "../proj/docs/guide.brf", "--stdout"],
    );
    assert_eq!(out.status.code(), Some(0), "stderr: {}", stderr(&out));
    assert_eq!(stdout(&out), "# Guide\n\n<b>hi</b> text.\n");
}

#[test]
fn reverse_convert_reports_a_bad_project_config() {
    let td = TempDir::new().unwrap();
    let root = td.path().canonicalize().unwrap();
    write(&root.join("brief.toml"), "[compile]\nnope = 1\n");
    write(&root.join("a.brf"), "# A\n");
    let out = run_in(&root, &["convert", "a.brf", "--stdout"]);
    assert_eq!(out.status.code(), Some(1));
    let err = stderr(&out);
    let prefix = format!(
        "brief: a.brf → FAILED: bad config {}: ",
        root.join("brief.toml").display()
    );
    assert!(err.starts_with(&prefix), "{}", err);
}

// ------------------------------------------------------ broken-file policy

/// A project whose `draft.brf` does not parse, and a `guide.brf` that does
/// not reference it.
fn project_with_broken_draft() -> (TempDir, PathBuf) {
    let td = TempDir::new().expect("create temp dir");
    let root = td.path().canonicalize().expect("canonicalize temp dir");
    write(&root.join("brief.toml"), "");
    write(&root.join("draft.brf"), "# Draft {#d}\n\n*unfinished\n");
    write(&root.join("guide.brf"), "# Guide\n\nText.\n");
    (td, root)
}

#[test]
fn unrelated_broken_file_does_not_block_compile() {
    let (_td, root) = project_with_broken_draft();
    let out = run_in(&root, &["compile", "guide.brf"]);
    assert_eq!(out.status.code(), Some(0), "stderr: {}", stderr(&out));
    assert_eq!(stderr(&out), "");
    assert!(stdout(&out).contains("<h1 id=\"guide\">Guide</h1>"));
}

#[test]
fn ref_into_broken_file_is_b0605_followed_by_the_targets_errors() {
    let (_td, root) = project_with_broken_draft();
    write(
        &root.join("links.brf"),
        "See @ref[draft.brf#d](the draft).\n",
    );
    let out = run_in(&root, &["compile", "links.brf"]);
    assert_eq!(out.status.code(), Some(1));
    assert_eq!(stdout(&out), "");
    let err = stderr(&out);
    let lines: Vec<&str> = err.lines().collect();
    assert_eq!(
        lines[0],
        "error[B0605]: cross-document reference target has errors"
    );
    assert_eq!(lines[1], "  --> links.brf:1:5");
    // The target's own diagnostics follow, rendered against the target.
    let draft = root.join("draft.brf");
    let at = lines
        .iter()
        .position(|l| *l == format!("  --> {}:3:1", draft.display()))
        .unwrap_or_else(|| panic!("draft diagnostics missing:\n{}", err));
    assert!(lines[at - 1].starts_with("error[B0207]"), "{}", err);
    assert!(lines[at + 2].contains("*unfinished"), "{}", err);
}

#[test]
fn reverse_convert_ignores_unrelated_broken_files() {
    let (_td, root) = project_with_broken_draft();
    let out = run_in(&root, &["convert", "guide.brf", "--stdout"]);
    assert_eq!(out.status.code(), Some(0), "stderr: {}", stderr(&out));
    assert_eq!(stdout(&out), "# Guide\n\nText.\n");
}

#[test]
fn reverse_convert_of_a_ref_into_a_broken_file_fails_with_b0605() {
    let (_td, root) = project_with_broken_draft();
    write(
        &root.join("links.brf"),
        "See @ref[draft.brf#d](the draft).\n",
    );
    let out = run_in(&root, &["convert", "links.brf", "--stdout"]);
    assert_eq!(out.status.code(), Some(1));
    let err = stderr(&out);
    let lines: Vec<&str> = err.lines().collect();
    assert_eq!(lines[0], "brief: links.brf → FAILED:");
    assert_eq!(
        lines[1],
        "error[B0605]: cross-document reference target has errors"
    );
    assert!(err.contains("error[B0207]"), "{}", err);
    assert!(!root.join("links.md").exists());
}
