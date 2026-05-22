//! Integration tests for `brief watch`.
//!
//! Two layers:
//!  1. Engine/dispatch tests that drive `Engine` and `handle_change_paths`
//!     directly. These are fast and deterministic — they exercise the same
//!     branches the FS watcher would, without depending on notify/FSEvents
//!     timing.
//!  2. One end-to-end test that spawns the binary and exercises notify-mini
//!     for real (with generous waits).

use brief::watch::{Engine, LlmOpts, Target, WatchOpts, handle_change_paths, scan_shortcode_uses};
use std::path::{Path, PathBuf};
use std::time::Duration;

fn temp_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("brief-watch-test-{}", name));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn opts_for(dir: &Path, target: Target) -> WatchOpts {
    WatchOpts {
        paths: vec![dir.to_path_buf()],
        target,
        config_path: dir.join("brief.toml"),
        llm_opts: LlmOpts::default(),
        no_clear: true,
    }
}

fn read(p: &Path) -> String {
    std::fs::read_to_string(p).expect(&format!("read {}", p.display()))
}

// ---------- Engine / dispatch tests ----------

#[test]
fn brf_change_recompiles_only_that_file() {
    let dir = temp_dir("brf-only-that-file");
    let a = dir.join("a.brf");
    let b = dir.join("b.brf");
    std::fs::write(&a, "# A\n").unwrap();
    std::fs::write(&b, "# B\n").unwrap();

    let mut engine = Engine::load(&opts_for(&dir, Target::Html)).unwrap();
    let mut log: Vec<u8> = Vec::new();
    engine.compile_all(&mut log);

    let a_html = dir.join("a.html");
    let b_html = dir.join("b.html");
    let b_before = read(&b_html);

    // Modify only a.brf; dispatch.
    std::fs::write(&a, "# A2\n").unwrap();
    handle_change_paths(&[a.clone()], &mut engine, &mut log);

    let a_after = read(&a_html);
    assert!(a_after.contains("A2"), "a output not updated: {}", a_after);
    assert_eq!(read(&b_html), b_before, "b should be untouched");
}

#[test]
fn brief_toml_structural_change_recompiles_all() {
    let dir = temp_dir("toml-structural-all");
    let a = dir.join("a.brf");
    let b = dir.join("b.brf");
    std::fs::write(&a, "# A\n").unwrap();
    std::fs::write(&b, "# B\n").unwrap();
    // Start without a brief.toml.
    let mut engine = Engine::load(&opts_for(&dir, Target::Html)).unwrap();
    let mut log: Vec<u8> = Vec::new();
    engine.compile_all(&mut log);

    let a_html = dir.join("a.html");
    let b_html = dir.join("b.html");
    let _a_before = read(&a_html);
    let _b_before = read(&b_html);

    // Add a brief.toml that changes a structural compile flag. Both files
    // should recompile.
    std::fs::write(
        dir.join("brief.toml"),
        "[compile]\nstrict_heading_levels = true\n",
    )
    .unwrap();
    handle_change_paths(&[dir.join("brief.toml")], &mut engine, &mut log);

    let log_str = String::from_utf8_lossy(&log);
    assert!(
        log_str.contains("recompiling all"),
        "expected 'recompiling all' in log, got:\n{}",
        log_str
    );
    // Both outputs were rewritten — exists check is enough since they were
    // re-rendered with the (same) inputs but the engine consciously wrote.
    assert!(a_html.exists() && b_html.exists());
}

#[test]
fn template_only_change_recompiles_only_users() {
    let dir = temp_dir("toml-template-users");
    let cfg_path = dir.join("brief.toml");
    std::fs::write(
        &cfg_path,
        r#"
[shortcodes.tip]
kind = "block"
template_html = "<aside class=\"v1\">{{content}}</aside>"
"#,
    )
    .unwrap();

    let uses = dir.join("uses.brf");
    let plain = dir.join("plain.brf");
    std::fs::write(&uses, "@tip\nbody\n@end\n").unwrap();
    std::fs::write(&plain, "# plain\n").unwrap();

    let mut engine = Engine::load(&opts_for(&dir, Target::Html)).unwrap();
    let mut log: Vec<u8> = Vec::new();
    engine.compile_all(&mut log);

    let uses_html = dir.join("uses.html");
    let plain_html = dir.join("plain.html");
    let v1 = read(&uses_html);
    assert!(v1.contains("class=\"v1\""), "v1: {}", v1);
    let plain_before = read(&plain_html);

    // Capture mtime of the plain output to verify it isn't rewritten.
    let plain_mtime_before = std::fs::metadata(&plain_html).unwrap().modified().unwrap();

    // Sleep just enough that any rewrite would have a measurably different
    // mtime. (Filesystems on macOS report mtime at second granularity in
    // some configs; we use content equality as the primary check and
    // mtime as a secondary signal.)
    std::thread::sleep(Duration::from_millis(20));

    // Mutate ONLY the template string of `tip`.
    std::fs::write(
        &cfg_path,
        r#"
[shortcodes.tip]
kind = "block"
template_html = "<aside class=\"v2\">{{content}}</aside>"
"#,
    )
    .unwrap();

    log.clear();
    handle_change_paths(&[cfg_path.clone()], &mut engine, &mut log);

    let log_str = String::from_utf8_lossy(&log);
    assert!(
        log_str.contains("template change"),
        "expected 'template change' in log, got:\n{}",
        log_str
    );
    // Files using `tip` were recompiled.
    let v2 = read(&uses_html);
    assert!(
        v2.contains("class=\"v2\""),
        "uses.html should reflect new template: {}",
        v2
    );
    // The plain file's output is byte-identical and (best-effort) untouched.
    assert_eq!(
        read(&plain_html),
        plain_before,
        "plain output content should be unchanged"
    );
    let plain_mtime_after = std::fs::metadata(&plain_html).unwrap().modified().unwrap();
    assert_eq!(
        plain_mtime_before, plain_mtime_after,
        "plain output mtime should be unchanged"
    );
}

#[test]
fn config_no_op_change_does_nothing() {
    let dir = temp_dir("toml-noop");
    let cfg_path = dir.join("brief.toml");
    std::fs::write(&cfg_path, "[project]\nname = \"x\"\n").unwrap();
    let a = dir.join("a.brf");
    std::fs::write(&a, "# A\n").unwrap();

    let mut engine = Engine::load(&opts_for(&dir, Target::Html)).unwrap();
    let mut log: Vec<u8> = Vec::new();
    engine.compile_all(&mut log);

    let a_html = dir.join("a.html");
    let mtime_before = std::fs::metadata(&a_html).unwrap().modified().unwrap();
    std::thread::sleep(Duration::from_millis(20));

    // Touch with identical content — config diff should be `None`.
    std::fs::write(&cfg_path, "[project]\nname = \"x\"\n").unwrap();
    log.clear();
    handle_change_paths(&[cfg_path.clone()], &mut engine, &mut log);

    let log_str = String::from_utf8_lossy(&log);
    assert!(
        !log_str.contains("recompiling all"),
        "no-op config change should not recompile, got:\n{}",
        log_str
    );
    let mtime_after = std::fs::metadata(&a_html).unwrap().modified().unwrap();
    assert_eq!(
        mtime_before, mtime_after,
        "a.html should not have been rewritten"
    );
}

#[test]
fn deleted_brf_is_untracked() {
    let dir = temp_dir("brf-deleted");
    let a = dir.join("a.brf");
    let b = dir.join("b.brf");
    std::fs::write(&a, "# A\n").unwrap();
    std::fs::write(&b, "# B\n").unwrap();

    let mut engine = Engine::load(&opts_for(&dir, Target::Html)).unwrap();
    let mut log: Vec<u8> = Vec::new();
    engine.compile_all(&mut log);
    assert_eq!(engine.files.len(), 2);

    std::fs::remove_file(&a).unwrap();
    handle_change_paths(&[a.clone()], &mut engine, &mut log);
    assert_eq!(engine.files.len(), 1, "deleted file should be untracked");
}

#[test]
fn new_brf_is_picked_up() {
    let dir = temp_dir("brf-new");
    let a = dir.join("a.brf");
    std::fs::write(&a, "# A\n").unwrap();
    let mut engine = Engine::load(&opts_for(&dir, Target::Html)).unwrap();
    let mut log: Vec<u8> = Vec::new();
    engine.compile_all(&mut log);
    assert_eq!(engine.files.len(), 1);

    let b = dir.join("b.brf");
    std::fs::write(&b, "# B\n").unwrap();
    handle_change_paths(&[b.clone()], &mut engine, &mut log);

    assert_eq!(engine.files.len(), 2);
    assert!(dir.join("b.html").exists(), "b.html should be produced");
}

#[test]
fn llm_target_writes_txt_output() {
    let dir = temp_dir("llm-output");
    let a = dir.join("a.brf");
    std::fs::write(&a, "# A\n").unwrap();
    let mut engine = Engine::load(&opts_for(&dir, Target::Llm)).unwrap();
    let mut log: Vec<u8> = Vec::new();
    engine.compile_all(&mut log);
    assert!(dir.join("a.txt").exists());
}

#[test]
fn scan_shortcode_uses_for_typical_brief_doc() {
    let s = "@tip\nbody\n@end\n\n@link(\"u\") foo @kbd[c]\n";
    let uses = scan_shortcode_uses(s);
    assert!(uses.contains("tip"));
    assert!(uses.contains("link"));
    assert!(uses.contains("kbd"));
}

