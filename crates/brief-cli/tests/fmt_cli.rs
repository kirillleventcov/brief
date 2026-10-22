//! End-to-end CLI tests for `brief fmt`.
//!
//! These exercise the binary via `std::process::Command`. Per-test temp dirs
//! keep artifacts isolated.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::str;

fn brief_bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_brief"))
}

/// Run `brief fmt <args>` from `cwd`.
fn fmt_in(cwd: &Path, args: &[&str]) -> Output {
    Command::new(brief_bin())
        .arg("fmt")
        .args(args)
        .current_dir(cwd)
        .output()
        .expect("run brief fmt")
}

fn fmt_stdin(args: &[&str], input: &str) -> Output {
    use std::io::Write as _;

    let mut child = Command::new(brief_bin())
        .arg("fmt")
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn brief fmt");
    child
        .stdin
        .take()
        .expect("stdin is piped")
        .write_all(input.as_bytes())
        .expect("write stdin");
    child.wait_with_output().expect("wait for brief fmt")
}

fn write(path: &Path, contents: &str) {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).expect("create parent dirs");
    }
    fs::write(path, contents).expect("write test file");
}

fn read(path: &Path) -> String {
    fs::read_to_string(path).expect("read test file")
}

fn stdout(out: &Output) -> String {
    str::from_utf8(&out.stdout)
        .expect("stdout is UTF-8")
        .to_owned()
}

fn stderr(out: &Output) -> String {
    str::from_utf8(&out.stderr)
        .expect("stderr is UTF-8")
        .to_owned()
}

const DIRTY: &str = "hello   \n\n\nworld\n";
const CLEAN: &str = "hello\n\nworld\n";

#[test]
fn formats_file_in_place_by_default() {
    let td = tempfile::tempdir().unwrap();
    write(&td.path().join("doc.brf"), DIRTY);
    let out = fmt_in(td.path(), &["doc.brf"]);
    assert_eq!(out.status.code(), Some(0), "stderr: {}", stderr(&out));
    assert_eq!(stdout(&out), "");
    assert_eq!(
        stderr(&out),
        "brief: 1 file reformatted, 0 files unchanged\n"
    );
    assert_eq!(read(&td.path().join("doc.brf")), CLEAN);
}

#[test]
fn summary_counts_reformatted_and_unchanged_files() {
    let td = tempfile::tempdir().unwrap();
    write(&td.path().join("a.brf"), DIRTY);
    write(&td.path().join("b.brf"), DIRTY);
    write(&td.path().join("c.brf"), CLEAN);
    let out = fmt_in(td.path(), &["a.brf", "b.brf", "c.brf"]);
    assert_eq!(out.status.code(), Some(0));
    assert_eq!(
        stderr(&out),
        "brief: 2 files reformatted, 1 file unchanged\n"
    );
    for f in ["a.brf", "b.brf", "c.brf"] {
        assert_eq!(read(&td.path().join(f)), CLEAN, "{}", f);
    }
}

#[cfg(unix)]
#[test]
fn unchanged_file_is_not_rewritten() {
    use std::os::unix::fs::MetadataExt;

    let td = tempfile::tempdir().unwrap();
    let doc = td.path().join("doc.brf");
    write(&doc, CLEAN);
    let before = fs::metadata(&doc).unwrap().ino();
    let out = fmt_in(td.path(), &["doc.brf"]);
    assert_eq!(out.status.code(), Some(0));
    assert_eq!(
        stderr(&out),
        "brief: 0 files reformatted, 1 file unchanged\n"
    );
    assert_eq!(fs::metadata(&doc).unwrap().ino(), before);
}

#[cfg(unix)]
#[test]
fn rewrite_replaces_file_atomically_and_keeps_permissions() {
    use std::fs::Permissions;
    use std::os::unix::fs::{MetadataExt, PermissionsExt};

    let td = tempfile::tempdir().unwrap();
    let doc = td.path().join("doc.brf");
    write(&doc, DIRTY);
    fs::set_permissions(&doc, Permissions::from_mode(0o640)).unwrap();
    let before = fs::metadata(&doc).unwrap().ino();
    let out = fmt_in(td.path(), &["doc.brf"]);
    assert_eq!(out.status.code(), Some(0), "stderr: {}", stderr(&out));
    let meta = fs::metadata(&doc).unwrap();
    // A new inode means a renamed temp file, not an in-place truncation.
    assert_ne!(meta.ino(), before);
    assert_eq!(meta.permissions().mode() & 0o777, 0o640);
    assert_eq!(read(&doc), CLEAN);
    let entries: Vec<_> = fs::read_dir(td.path())
        .unwrap()
        .map(|e| e.unwrap().file_name())
        .collect();
    assert_eq!(entries, vec!["doc.brf"], "temp file left behind");
}

#[cfg(unix)]
#[test]
fn rewrite_through_symlink_keeps_the_link() {
    use std::os::unix::fs as unix_fs;

    let td = tempfile::tempdir().unwrap();
    let real = td.path().join("real.brf");
    let link = td.path().join("link.brf");
    write(&real, DIRTY);
    unix_fs::symlink(&real, &link).unwrap();
    let out = fmt_in(td.path(), &["link.brf"]);
    assert_eq!(out.status.code(), Some(0), "stderr: {}", stderr(&out));
    assert!(fs::symlink_metadata(&link).unwrap().is_symlink());
    assert_eq!(read(&real), CLEAN);
}

#[test]
fn bom_alone_counts_as_a_change() {
    let td = tempfile::tempdir().unwrap();
    write(&td.path().join("doc.brf"), "\u{feff}hello\n");
    let out = fmt_in(td.path(), &["doc.brf"]);
    assert_eq!(out.status.code(), Some(0));
    assert_eq!(read(&td.path().join("doc.brf")), "hello\n");
}

#[test]
fn check_lists_changed_paths_and_writes_nothing() {
    let td = tempfile::tempdir().unwrap();
    write(&td.path().join("clean.brf"), CLEAN);
    write(&td.path().join("dirty.brf"), DIRTY);
    let out = fmt_in(td.path(), &["--check", "clean.brf", "dirty.brf"]);
    assert_eq!(out.status.code(), Some(1));
    assert_eq!(stdout(&out), "dirty.brf\n");
    assert_eq!(
        stderr(&out),
        "brief: 1 file would be reformatted, 1 file already formatted\n"
    );
    assert_eq!(read(&td.path().join("dirty.brf")), DIRTY);
}

#[test]
fn check_exits_zero_when_clean() {
    let td = tempfile::tempdir().unwrap();
    write(&td.path().join("doc.brf"), CLEAN);
    let out = fmt_in(td.path(), &["--check", "doc.brf"]);
    assert_eq!(out.status.code(), Some(0));
    assert_eq!(stdout(&out), "");
    assert_eq!(
        stderr(&out),
        "brief: 0 files would be reformatted, 1 file already formatted\n"
    );
}

#[test]
fn diff_prints_unified_diff_with_a_b_headers() {
    let td = tempfile::tempdir().unwrap();
    write(&td.path().join("doc.brf"), "hello   \n");
    write(&td.path().join("clean.brf"), CLEAN);
    let out = fmt_in(td.path(), &["--diff", "doc.brf", "clean.brf"]);
    assert_eq!(out.status.code(), Some(1));
    assert_eq!(
        stdout(&out),
        "--- a/doc.brf\n+++ b/doc.brf\n@@ -1 +1 @@\n-hello   \n+hello\n"
    );
    assert_eq!(read(&td.path().join("doc.brf")), "hello   \n");
}

#[test]
fn diff_exits_zero_when_clean() {
    let td = tempfile::tempdir().unwrap();
    write(&td.path().join("doc.brf"), CLEAN);
    let out = fmt_in(td.path(), &["--diff", "doc.brf"]);
    assert_eq!(out.status.code(), Some(0));
    assert_eq!(stdout(&out), "");
}

#[test]
fn dash_formats_stdin_to_stdout() {
    let out = fmt_stdin(&["-"], DIRTY);
    assert_eq!(out.status.code(), Some(0));
    assert_eq!(stdout(&out), CLEAN);
    assert_eq!(stderr(&out), "");
}

#[test]
fn dash_with_check_reports_stdin() {
    let out = fmt_stdin(&["--check", "-"], DIRTY);
    assert_eq!(out.status.code(), Some(1));
    assert_eq!(stdout(&out), "<stdin>\n");
    let out = fmt_stdin(&["--check", "-"], CLEAN);
    assert_eq!(out.status.code(), Some(0));
    assert_eq!(stdout(&out), "");
}

#[test]
fn dash_cannot_be_combined_with_paths() {
    let td = tempfile::tempdir().unwrap();
    write(&td.path().join("doc.brf"), DIRTY);
    let out = fmt_in(td.path(), &["-", "doc.brf"]);
    assert_eq!(out.status.code(), Some(2));
    assert_eq!(read(&td.path().join("doc.brf")), DIRTY);
}

#[test]
fn stdout_flag_previews_one_file_without_writing() {
    let td = tempfile::tempdir().unwrap();
    write(&td.path().join("doc.brf"), DIRTY);
    let out = fmt_in(td.path(), &["doc.brf", "--stdout"]);
    assert_eq!(out.status.code(), Some(0));
    assert_eq!(stdout(&out), CLEAN);
    assert_eq!(stderr(&out), "");
    assert_eq!(read(&td.path().join("doc.brf")), DIRTY);
}

#[test]
fn stdout_flag_rejects_several_files_or_a_directory() {
    let td = tempfile::tempdir().unwrap();
    write(&td.path().join("a.brf"), DIRTY);
    write(&td.path().join("b.brf"), DIRTY);
    let out = fmt_in(td.path(), &["--stdout", "a.brf", "b.brf"]);
    assert_eq!(out.status.code(), Some(2));
    let out = fmt_in(td.path(), &["--stdout", "."]);
    assert_eq!(out.status.code(), Some(2));
    assert_eq!(read(&td.path().join("a.brf")), DIRTY);
}

#[test]
fn directory_recurses_over_brf_files_with_project_excludes() {
    let td = tempfile::tempdir().unwrap();
    let dir = td.path().join("dir");
    for f in [
        "a.brf",
        "sub/b.brf",
        ".hidden/c.brf",
        "target/d.brf",
        "dist/e.brf",
        "node_modules/f.brf",
        "notes.md",
    ] {
        write(&dir.join(f), DIRTY);
    }
    let out = fmt_in(td.path(), &["--check", "dir"]);
    assert_eq!(out.status.code(), Some(1));
    assert_eq!(stdout(&out), "dir/a.brf\ndir/sub/b.brf\n");

    let out = fmt_in(td.path(), &["dir"]);
    assert_eq!(out.status.code(), Some(0));
    assert_eq!(read(&dir.join("a.brf")), CLEAN);
    assert_eq!(read(&dir.join("sub/b.brf")), CLEAN);
    for f in [
        ".hidden/c.brf",
        "target/d.brf",
        "dist/e.brf",
        "node_modules/f.brf",
        "notes.md",
    ] {
        assert_eq!(read(&dir.join(f)), DIRTY, "{} was touched", f);
    }
}

#[test]
fn no_paths_formats_the_whole_project() {
    let td = tempfile::tempdir().unwrap();
    let proj = td.path().join("proj");
    // fmt needs only the root, so a broken config must not stop it.
    write(&proj.join("brief.toml"), "this is not toml\n");
    write(&proj.join("top.brf"), DIRTY);
    write(&proj.join("sub/inner.brf"), DIRTY);
    write(&td.path().join("outside.brf"), DIRTY);

    let out = fmt_in(&proj, &["--check"]);
    assert_eq!(out.status.code(), Some(1), "stderr: {}", stderr(&out));
    assert_eq!(stdout(&out), "sub/inner.brf\ntop.brf\n");

    // From a subdirectory, the project root above it is still the scope.
    let out = fmt_in(&proj.join("sub"), &[]);
    assert_eq!(out.status.code(), Some(0), "stderr: {}", stderr(&out));
    assert_eq!(
        stderr(&out),
        "brief: 2 files reformatted, 0 files unchanged\n"
    );
    assert_eq!(read(&proj.join("top.brf")), CLEAN);
    assert_eq!(read(&proj.join("sub/inner.brf")), CLEAN);
    assert_eq!(read(&td.path().join("outside.brf")), DIRTY);
}

#[test]
fn no_paths_outside_a_project_formats_the_current_directory() {
    let td = tempfile::tempdir().unwrap();
    let here = td.path().join("here");
    write(&here.join("a.brf"), DIRTY);
    write(&td.path().join("above.brf"), DIRTY);
    let out = fmt_in(&here, &[]);
    assert_eq!(out.status.code(), Some(0), "stderr: {}", stderr(&out));
    assert_eq!(read(&here.join("a.brf")), CLEAN);
    assert_eq!(read(&td.path().join("above.brf")), DIRTY);
}

#[test]
fn explicit_non_brf_file_is_refused_before_anything_is_written() {
    let td = tempfile::tempdir().unwrap();
    write(&td.path().join("doc.brf"), DIRTY);
    write(&td.path().join("notes.md"), DIRTY);
    let out = fmt_in(td.path(), &["doc.brf", "notes.md"]);
    assert_eq!(out.status.code(), Some(2));
    assert_eq!(
        stderr(&out),
        "brief: notes.md is not a .brf file; fmt only formats Brief sources\n"
    );
    assert_eq!(read(&td.path().join("notes.md")), DIRTY);
    assert_eq!(read(&td.path().join("doc.brf")), DIRTY);
}

#[test]
fn missing_input_file_reports_and_exits_two() {
    let td = tempfile::tempdir().unwrap();
    let out = fmt_in(td.path(), &["nope.brf"]);
    assert_eq!(out.status.code(), Some(2));
    assert!(stderr(&out).starts_with("brief: cannot read nope.brf: "));
}

#[test]
fn write_flag_is_an_accepted_no_op() {
    let td = tempfile::tempdir().unwrap();
    write(&td.path().join("a.brf"), DIRTY);
    write(&td.path().join("b.brf"), DIRTY);
    let out = fmt_in(td.path(), &["--write", "a.brf"]);
    assert_eq!(out.status.code(), Some(0));
    let out = fmt_in(td.path(), &["-w", "b.brf"]);
    assert_eq!(out.status.code(), Some(0));
    assert_eq!(read(&td.path().join("a.brf")), CLEAN);
    assert_eq!(read(&td.path().join("b.brf")), CLEAN);
}

#[test]
fn check_and_write_mutually_exclusive() {
    let td = tempfile::tempdir().unwrap();
    write(&td.path().join("doc.brf"), DIRTY);
    let out = fmt_in(td.path(), &["--check", "--write", "doc.brf"]);
    assert_eq!(out.status.code(), Some(2));
    assert_eq!(read(&td.path().join("doc.brf")), DIRTY);
}

#[test]
fn sort_frontmatter_flag_reorders_top_level_keys() {
    let td = tempfile::tempdir().unwrap();
    write(
        &td.path().join("doc.brf"),
        "+++\nz = 1\na = 2\n+++\n# Doc\n",
    );
    let out = fmt_in(td.path(), &["--sort-frontmatter", "doc.brf"]);
    assert_eq!(out.status.code(), Some(0));
    assert_eq!(
        read(&td.path().join("doc.brf")),
        "+++\na = 2\nz = 1\n+++\n# Doc\n"
    );
}

#[test]
fn table_with_escaped_pipe_still_compiles_after_fmt() {
    // FMT-01 end to end: fmt used to split `\|` and break the table.
    let td = tempfile::tempdir().unwrap();
    let doc = td.path().join("t.brf");
    write(&doc, "@t\n| a \\| b | c\n| d | e\n");
    let out = fmt_in(td.path(), &["t.brf"]);
    assert_eq!(out.status.code(), Some(0));
    assert_eq!(read(&doc), "@t\n| a \\| b | c\n| d      | e\n");
    let status = Command::new(brief_bin())
        .arg("compile")
        .arg(&doc)
        .output()
        .unwrap()
        .status;
    assert!(status.success());
}

#[test]
fn formatter_is_idempotent_on_real_repo_doc() {
    // The flagship doc shipped with the repo should round-trip through the
    // formatter without any further change after the first pass. This is
    // the broadest "real input" smoke test we can run.
    let learn = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join("LearnXinYminutes.brf");
    let td = tempfile::tempdir().unwrap();
    let staging = td.path().join("learn.brf");
    write(&staging, &read(&learn));

    let pass1 = fmt_in(td.path(), &["--stdout", "learn.brf"]);
    assert!(
        pass1.status.success(),
        "fmt failed on LearnXinYminutes: {}",
        stderr(&pass1)
    );
    let pass1_out = stdout(&pass1);

    write(&staging, &pass1_out);
    let pass2 = fmt_in(td.path(), &["--check", "learn.brf"]);
    assert_eq!(
        pass2.status.code(),
        Some(0),
        "fmt is not idempotent on real doc"
    );
}
