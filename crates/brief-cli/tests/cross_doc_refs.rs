//! End-to-end CLI tests for `@ref` cross-document references.

use std::fs;
use std::path::Path;
use std::process::Command;
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

#[test]
fn project_compile_resolves_valid_ref_to_html() {
    let td = TempDir::new().unwrap();
    let root = td.path();
    write(&root.join("brief.toml"), "");
    write(&root.join("a.brf"), "# Top {#top}\n\nbody.\n");
    write(&root.join("b.brf"), "# B\n\nSee @ref[a.brf#top](Anchor).\n");

    let output = Command::new(brief_bin())
        .arg("compile")
        .arg(root.join("b.brf"))
        .arg("--target=html")
        .output()
        .expect("run brief");
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(
        stdout.contains("<a href=\"a.html#top\">Anchor</a>"),
        "stdout: {}",
        stdout
    );
}

#[test]
fn project_compile_links_relative_to_the_compiled_file() {
    let td = TempDir::new().unwrap();
    let root = td.path();
    write(&root.join("brief.toml"), "");
    write(&root.join("guide.brf"), "# Guide\n\n## Install Steps\n");
    write(&root.join("sub/b.brf"), "# B\n");
    write(
        &root.join("sub/a.brf"),
        "# A\n\n@ref[guide.brf#install-steps](Steps) @ref[sub/b.brf](B) @ref[sub/a.brf#a](Top)\n",
    );

    let output = Command::new(brief_bin())
        .arg("compile")
        .arg(root.join("sub/a.brf"))
        .arg("--target=html")
        .output()
        .expect("run brief");
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(
        stdout.contains(
            "<p><a href=\"../guide.html#install-steps\">Steps</a> <a href=\"b.html\">B</a> <a href=\"#a\">Top</a></p>"
        ),
        "stdout: {}",
        stdout
    );
}

#[test]
fn project_compile_reports_missing_anchor() {
    let td = TempDir::new().unwrap();
    let root = td.path();
    write(&root.join("brief.toml"), "");
    write(&root.join("a.brf"), "# A {#real}\n");
    write(&root.join("b.brf"), "@ref[a.brf#missing](X)\n");

    let output = Command::new(brief_bin())
        .arg("compile")
        .arg(root.join("b.brf"))
        .arg("--target=html")
        .output()
        .expect("run brief");
    assert!(!output.status.success());
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(stderr.contains("B0602"), "stderr: {}", stderr);
}

#[test]
fn ref_outside_project_is_b0604() {
    let td = TempDir::new().unwrap();
    let root = td.path();
    // No brief.toml.
    write(&root.join("lonely.brf"), "@ref[other.brf](X)\n");
    let output = Command::new(brief_bin())
        .arg("compile")
        .arg(root.join("lonely.brf"))
        .arg("--target=html")
        .output()
        .expect("run brief");
    assert!(!output.status.success());
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(stderr.contains("B0604"), "stderr: {}", stderr);
}

#[test]
fn prepass_errors_render_against_their_own_source() {
    let td = TempDir::new().unwrap();
    let root = td.path().canonicalize().unwrap();
    write(&root.join("brief.toml"), "");
    // a.brf has a parse error: a heading with too many '#'
    write(&root.join("a.brf"), "####### Bad heading\n");
    write(&root.join("b.brf"), "# OK\n\nSee @ref[a.brf](A).\n");

    let output = Command::new(brief_bin())
        .arg("compile")
        .arg(root.join("b.brf"))
        .arg("--target=html")
        .output()
        .expect("run brief");
    assert_eq!(output.status.code(), Some(1));
    let stderr = String::from_utf8(output.stderr).unwrap();
    // B0605 points at b.brf, where the @ref is; a.brf's own error follows,
    // located in a.brf where the broken heading lives.
    assert!(
        stderr.contains(&format!("  --> {}:3:5\n", root.join("b.brf").display())),
        "stderr: {}",
        stderr
    );
    assert!(
        stderr.contains(&format!(
            "error[B0301]: heading level exceeds maximum of 6\n  --> {}:1:1\n",
            root.join("a.brf").display()
        )),
        "stderr: {}",
        stderr
    );
}

#[test]
fn explain_b0601_describes_missing_file() {
    let out = Command::new(brief_bin())
        .arg("explain")
        .arg("B0601")
        .output()
        .unwrap();
    assert!(out.status.success());
    let s = String::from_utf8(out.stdout).unwrap();
    assert!(s.contains("B0601"));
    assert!(s.to_lowercase().contains("file"));
}

#[test]
fn explain_b0604_mentions_brief_toml() {
    let out = Command::new(brief_bin())
        .arg("explain")
        .arg("B0604")
        .output()
        .unwrap();
    assert!(out.status.success());
    let s = String::from_utf8(out.stdout).unwrap();
    assert!(s.contains("brief.toml"));
}

#[test]
fn adding_brief_toml_makes_ref_valid() {
    let td = TempDir::new().unwrap();
    let root = td.path();
    write(&root.join("a.brf"), "# A {#x}\n");
    write(&root.join("b.brf"), "@ref[a.brf#x](X)\n");

    // Without brief.toml: B0604.
    let out = Command::new(brief_bin())
        .arg("compile")
        .arg(root.join("b.brf"))
        .arg("--target=html")
        .output()
        .unwrap();
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("B0604"));

    // Add brief.toml: should compile clean.
    write(&root.join("brief.toml"), "");
    let out = Command::new(brief_bin())
        .arg("compile")
        .arg(root.join("b.brf"))
        .arg("--target=html")
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}
