//! Exit codes of `brief-web build`: the site's errors must fail the process,
//! not scroll past as warnings.

use std::fs;
use std::path::Path;
use std::process::{Command, Output};

fn write(p: &Path, content: &str) {
    if let Some(parent) = p.parent() {
        fs::create_dir_all(parent).expect("create test project directory");
    }
    fs::write(p, content).expect("write test project file");
}

fn build(project: &Path, extra: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_brief-web"))
        .arg("build")
        .arg(project)
        .args(extra)
        .output()
        .expect("run the brief-web binary")
}

#[test]
fn build_exit_codes_follow_the_diagnostics() {
    let td = tempfile::TempDir::new().unwrap();
    let project = td.path();
    write(&project.join("book.toml"), "[book]\ntitle = \"T\"\n");
    let index = project.join("src/index.brf");

    write(&index, "# Home\n\nClean.\n");
    let out = build(project, &[]);
    assert!(out.status.success(), "{:?}", out);

    write(&index, "# Home\n\n@callout(kind: info)\nBody.\n@end\n");
    let out = build(project, &[]);
    assert!(out.status.success(), "{:?}", out);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("warning[B0408]"), "{}", stderr);
    let out = build(project, &["--deny-warnings"]);
    assert_eq!(out.status.code(), Some(1), "{:?}", out);

    write(&index, "# Home\n\n@bogus[x]\n");
    let out = build(project, &[]);
    assert_eq!(out.status.code(), Some(1), "{:?}", out);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("error[B0401]"), "{}", stderr);
    assert!(stderr.contains("src/index.brf:3:1"), "{}", stderr);
    assert!(
        stderr.contains("brief-web: build failed: 1 error in index.brf"),
        "{}",
        stderr
    );
}
