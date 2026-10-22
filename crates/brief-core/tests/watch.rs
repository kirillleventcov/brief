//! Integration tests for `brief watch`.
//!
//! Two layers:
//!  1. Engine/dispatch tests that drive `Engine` and `handle_change_paths`
//!     directly. These are fast and deterministic — they exercise the same
//!     branches the FS watcher would, without depending on notify/FSEvents
//!     timing.
//!  2. End-to-end tests that spawn the binary and exercise notify for real
//!     (with generous waits) live in `crates/brief-cli/tests/watch_e2e.rs`.

#![cfg(feature = "watch")]

use brief::watch::{self, CompileOutcome, Engine, LlmOptions, Target, WatchOptions};
use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::slice;
use std::time::Duration;

fn temp_dir(name: &str) -> PathBuf {
    let dir = env::temp_dir().join(format!("brief-watch-test-{}", name));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).expect("create temp test dir");
    dir
}

fn opts_for(dir: &Path, target: Target) -> WatchOptions {
    WatchOptions {
        paths: vec![dir.to_path_buf()],
        target: Some(target),
        config_override: None,
        llm_options: LlmOptions::default(),
        no_clear: true,
    }
}

fn read(p: &Path) -> String {
    fs::read_to_string(p).unwrap_or_else(|e| panic!("read {}: {e}", p.display()))
}

// ---------- Engine / dispatch tests ----------

#[test]
fn brf_change_recompiles_only_that_file() {
    let dir = temp_dir("brf-only-that-file");
    let a = dir.join("a.brf");
    let b = dir.join("b.brf");
    fs::write(&a, "# A\n").unwrap();
    fs::write(&b, "# B\n").unwrap();

    let mut engine = Engine::load(&opts_for(&dir, Target::Html)).unwrap();
    let mut log: Vec<u8> = Vec::new();
    engine.compile_all(&mut log);

    let a_html = dir.join("a.html");
    let b_html = dir.join("b.html");
    let b_before = read(&b_html);

    // Modify only a.brf; dispatch.
    fs::write(&a, "# A2\n").unwrap();
    watch::handle_change_paths(slice::from_ref(&a), &mut engine, &mut log);

    let a_after = read(&a_html);
    assert!(a_after.contains("A2"), "a output not updated: {}", a_after);
    assert_eq!(read(&b_html), b_before, "b should be untouched");
}

#[test]
fn brief_toml_structural_change_recompiles_all() {
    let dir = temp_dir("toml-structural-all");
    let a = dir.join("a.brf");
    let b = dir.join("b.brf");
    fs::write(&a, "# A\n").unwrap();
    fs::write(&b, "# B\n").unwrap();
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
    fs::write(
        dir.join("brief.toml"),
        "[compile]\nstrict_heading_levels = true\n",
    )
    .unwrap();
    watch::handle_change_paths(&[dir.join("brief.toml")], &mut engine, &mut log);

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
    fs::write(
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
    fs::write(&uses, "@tip\nbody\n@end\n").unwrap();
    fs::write(&plain, "# plain\n").unwrap();

    let mut engine = Engine::load(&opts_for(&dir, Target::Html)).unwrap();
    let mut log: Vec<u8> = Vec::new();
    engine.compile_all(&mut log);

    let uses_html = dir.join("uses.html");
    let plain_html = dir.join("plain.html");
    let v1 = read(&uses_html);
    assert!(v1.contains("class=\"v1\""), "v1: {}", v1);
    let plain_before = read(&plain_html);

    // Capture mtime of the plain output to verify it isn't rewritten.
    let plain_mtime_before = fs::metadata(&plain_html).unwrap().modified().unwrap();

    // Sleep just enough that any rewrite would have a measurably different
    // mtime. (Filesystems on macOS report mtime at second granularity in
    // some configs; we use content equality as the primary check and
    // mtime as a secondary signal.)
    std::thread::sleep(Duration::from_millis(20));

    // Mutate ONLY the template string of `tip`.
    fs::write(
        &cfg_path,
        r#"
[shortcodes.tip]
kind = "block"
template_html = "<aside class=\"v2\">{{content}}</aside>"
"#,
    )
    .unwrap();

    log.clear();
    watch::handle_change_paths(slice::from_ref(&cfg_path), &mut engine, &mut log);

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
    let plain_mtime_after = fs::metadata(&plain_html).unwrap().modified().unwrap();
    assert_eq!(
        plain_mtime_before, plain_mtime_after,
        "plain output mtime should be unchanged"
    );
}

#[test]
fn config_no_op_change_does_nothing() {
    let dir = temp_dir("toml-noop");
    let cfg_path = dir.join("brief.toml");
    fs::write(&cfg_path, "[project]\nname = \"x\"\n").unwrap();
    let a = dir.join("a.brf");
    fs::write(&a, "# A\n").unwrap();

    let mut engine = Engine::load(&opts_for(&dir, Target::Html)).unwrap();
    let mut log: Vec<u8> = Vec::new();
    engine.compile_all(&mut log);

    let a_html = dir.join("a.html");
    let mtime_before = fs::metadata(&a_html).unwrap().modified().unwrap();
    std::thread::sleep(Duration::from_millis(20));

    // Touch with identical content — config diff should be `None`.
    fs::write(&cfg_path, "[project]\nname = \"x\"\n").unwrap();
    log.clear();
    watch::handle_change_paths(slice::from_ref(&cfg_path), &mut engine, &mut log);

    let log_str = String::from_utf8_lossy(&log);
    assert!(
        !log_str.contains("recompiling all"),
        "no-op config change should not recompile, got:\n{}",
        log_str
    );
    let mtime_after = fs::metadata(&a_html).unwrap().modified().unwrap();
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
    fs::write(&a, "# A\n").unwrap();
    fs::write(&b, "# B\n").unwrap();

    let mut engine = Engine::load(&opts_for(&dir, Target::Html)).unwrap();
    let mut log: Vec<u8> = Vec::new();
    engine.compile_all(&mut log);
    assert_eq!(engine.files.len(), 2);

    fs::remove_file(&a).unwrap();
    watch::handle_change_paths(slice::from_ref(&a), &mut engine, &mut log);
    assert_eq!(engine.files.len(), 1, "deleted file should be untracked");
}

#[test]
fn new_brf_is_picked_up() {
    let dir = temp_dir("brf-new");
    let a = dir.join("a.brf");
    fs::write(&a, "# A\n").unwrap();
    let mut engine = Engine::load(&opts_for(&dir, Target::Html)).unwrap();
    let mut log: Vec<u8> = Vec::new();
    engine.compile_all(&mut log);
    assert_eq!(engine.files.len(), 1);

    let b = dir.join("b.brf");
    fs::write(&b, "# B\n").unwrap();
    watch::handle_change_paths(slice::from_ref(&b), &mut engine, &mut log);

    assert_eq!(engine.files.len(), 2);
    assert!(dir.join("b.html").exists(), "b.html should be produced");
}

#[test]
fn llm_target_writes_txt_output() {
    let dir = temp_dir("llm-output");
    let a = dir.join("a.brf");
    fs::write(&a, "# A\n").unwrap();
    let mut engine = Engine::load(&opts_for(&dir, Target::Llm)).unwrap();
    let mut log: Vec<u8> = Vec::new();
    engine.compile_all(&mut log);
    assert!(dir.join("a.txt").exists());
}

#[test]
fn scan_shortcode_uses_for_typical_brief_doc() {
    let s = "@tip\nbody\n@end\n\n@link(\"u\") foo @kbd[c]\n";
    let uses = watch::scan_shortcode_uses(s);
    assert!(uses.contains("tip"));
    assert!(uses.contains("link"));
    assert!(uses.contains("kbd"));
}

// ---------- Idle stability ----------

#[test]
fn unchanged_content_is_not_rebuilt() {
    let dir = temp_dir("unchanged-content");
    let a = dir.join("a.brf");
    fs::write(&a, "# A\n").unwrap();
    let mut opts = opts_for(&dir, Target::Html);
    opts.no_clear = false;
    let mut engine = Engine::load(&opts).unwrap();
    let mut log: Vec<u8> = Vec::new();
    engine.compile_all(&mut log);
    let a_html = dir.join("a.html");
    let mtime_before = fs::metadata(&a_html).unwrap().modified().unwrap();
    std::thread::sleep(Duration::from_millis(20));

    // Same bytes rewritten, as an editor's save-without-changes does.
    fs::write(&a, "# A\n").unwrap();
    let mut log: Vec<u8> = Vec::new();
    watch::handle_change_paths(slice::from_ref(&a), &mut engine, &mut log);
    assert_eq!(String::from_utf8_lossy(&log), "", "no rebuild, no clear");
    let mtime_after = fs::metadata(&a_html).unwrap().modified().unwrap();
    assert_eq!(mtime_before, mtime_after);
}

#[test]
fn own_output_events_are_ignored() {
    let dir = temp_dir("own-outputs");
    let a = dir.join("a.brf");
    fs::write(&a, "# A\n").unwrap();
    let mut opts = opts_for(&dir, Target::Html);
    opts.no_clear = false;
    let mut engine = Engine::load(&opts).unwrap();
    let mut log: Vec<u8> = Vec::new();
    engine.compile_all(&mut log);

    let mut log: Vec<u8> = Vec::new();
    watch::handle_change_paths(&[dir.join("a.html")], &mut engine, &mut log);
    assert_eq!(String::from_utf8_lossy(&log), "");
}

#[test]
fn single_file_watch_does_not_adopt_siblings() {
    let dir = temp_dir("single-file-scope");
    let a = dir.join("a.brf");
    let b = dir.join("b.brf");
    fs::write(&a, "# A\n").unwrap();
    let mut opts = opts_for(&dir, Target::Html);
    opts.paths = vec![a];
    let mut engine = Engine::load(&opts).unwrap();
    let mut log: Vec<u8> = Vec::new();
    engine.compile_all(&mut log);

    fs::write(&b, "# B\n").unwrap();
    watch::handle_change_paths(slice::from_ref(&b), &mut engine, &mut log);
    assert_eq!(engine.files.len(), 1);
    assert!(!dir.join("b.html").exists());
}

// ---------- Project discovery ----------

#[test]
fn each_file_compiles_in_its_own_project() {
    let dir = temp_dir("own-project");
    fs::create_dir_all(dir.join("p1")).unwrap();
    fs::create_dir_all(dir.join("p2")).unwrap();
    fs::write(
        dir.join("p1/brief.toml"),
        "[shortcodes.lede]\nkind = \"inline\"\ntemplate_html = \"<b>{{content}}</b>\"\n",
    )
    .unwrap();
    fs::write(
        dir.join("p2/brief.toml"),
        "[compile]\ndefault_target = \"llm\"\n\n\
         [shortcodes.lede]\nkind = \"inline\"\ntemplate_html = \"<i>{{content}}</i>\"\n",
    )
    .unwrap();
    fs::write(dir.join("p1/a.brf"), "@lede[one]\n").unwrap();
    fs::write(dir.join("p2/a.brf"), "@lede[two]\n").unwrap();

    // No --target: each project's default_target decides.
    let mut opts = opts_for(&dir, Target::Html);
    opts.target = None;
    let mut engine = Engine::load(&opts).unwrap();
    let mut log: Vec<u8> = Vec::new();
    engine.compile_all(&mut log);
    let log = String::from_utf8_lossy(&log);
    assert!(!log.contains("error"), "{}", log);
    assert!(read(&dir.join("p1/a.html")).contains("<b>one</b>"));
    assert!(dir.join("p2/a.txt").exists(), "{}", log);
    assert!(!dir.join("p2/a.html").exists());
}

#[test]
fn brief_toml_above_the_watched_dir_governs_it() {
    let dir = temp_dir("toml-above");
    fs::write(
        dir.join("brief.toml"),
        "[shortcodes.lede]\nkind = \"inline\"\ntemplate_html = \"<b>{{content}}</b>\"\n",
    )
    .unwrap();
    fs::create_dir_all(dir.join("docs")).unwrap();
    fs::write(dir.join("docs/a.brf"), "@lede[hi]\n").unwrap();
    let mut opts = opts_for(&dir, Target::Html);
    opts.paths = vec![dir.join("docs")];
    let mut engine = Engine::load(&opts).unwrap();
    let mut log: Vec<u8> = Vec::new();
    engine.compile_all(&mut log);
    assert!(read(&dir.join("docs/a.html")).contains("<b>hi</b>"));
}

#[test]
fn config_override_applies_to_every_project() {
    let dir = temp_dir("config-override");
    fs::create_dir_all(dir.join("p1")).unwrap();
    fs::write(dir.join("p1/brief.toml"), "").unwrap();
    fs::write(dir.join("p1/a.brf"), "@lede[x]\n").unwrap();
    fs::write(dir.join("b.brf"), "@lede[y]\n").unwrap();
    let cfg = dir.join("override.toml");
    fs::write(
        &cfg,
        "[shortcodes.lede]\nkind = \"inline\"\ntemplate_html = \"<u>{{content}}</u>\"\n",
    )
    .unwrap();
    let mut opts = opts_for(&dir, Target::Html);
    opts.config_override = Some(cfg.clone());
    let mut engine = Engine::load(&opts).unwrap();
    let mut log: Vec<u8> = Vec::new();
    engine.compile_all(&mut log);
    assert!(read(&dir.join("p1/a.html")).contains("<u>x</u>"));
    assert!(read(&dir.join("b.html")).contains("<u>y</u>"));

    // Editing the override recompiles both.
    fs::write(
        &cfg,
        "[shortcodes.lede]\nkind = \"inline\"\ntemplate_html = \"<s>{{content}}</s>\"\n",
    )
    .unwrap();
    let mut log: Vec<u8> = Vec::new();
    watch::handle_change_paths(slice::from_ref(&cfg), &mut engine, &mut log);
    assert!(read(&dir.join("p1/a.html")).contains("<s>x</s>"));
    assert!(read(&dir.join("b.html")).contains("<s>y</s>"));
}

#[test]
fn bad_config_fails_startup() {
    let dir = temp_dir("bad-config");
    fs::write(dir.join("brief.toml"), "[compile]\nnope = 1\n").unwrap();
    fs::write(dir.join("a.brf"), "# A\n").unwrap();
    let err = Engine::load(&opts_for(&dir, Target::Html))
        .err()
        .expect("bad config must fail");
    let cfg = dir.canonicalize().unwrap().join("brief.toml");
    assert!(
        err.starts_with(&format!("bad config {}: ", cfg.display())),
        "{}",
        err
    );
}

// ---------- Broken-file policy ----------

#[test]
fn broken_file_only_fails_files_that_ref_it() {
    let dir = temp_dir("broken-sibling");
    fs::write(dir.join("brief.toml"), "").unwrap();
    fs::write(dir.join("draft.brf"), "# D {#d}\n\n*unfinished\n").unwrap();
    fs::write(dir.join("guide.brf"), "# Guide\n").unwrap();
    fs::write(dir.join("links.brf"), "See @ref[draft.brf#d](the draft).\n").unwrap();
    let mut engine = Engine::load(&opts_for(&dir, Target::Html)).unwrap();

    let mut log: Vec<u8> = Vec::new();
    let guide = engine.compile_one(&dir.join("guide.brf").canonicalize().unwrap(), &mut log);
    assert!(matches!(guide, CompileOutcome::Ok { .. }), "{:?}", guide);
    assert_eq!(String::from_utf8_lossy(&log), "");

    let mut log: Vec<u8> = Vec::new();
    let links = engine.compile_one(&dir.join("links.brf").canonicalize().unwrap(), &mut log);
    assert!(
        matches!(links, CompileOutcome::Errors { count: 1, .. }),
        "{:?}",
        links
    );
    let log = String::from_utf8_lossy(&log);
    let b0605 = log.find("error[B0605]").expect("B0605");
    let draft = log.find("error[B0207]").expect("draft's own error");
    assert!(b0605 < draft, "{}", log);
}

#[test]
fn failed_file_is_retried_on_an_identical_save() {
    let dir = temp_dir("failed-retry");
    let a = dir.join("a.brf");
    fs::write(&a, "@nope[x]\n").unwrap();
    let mut engine = Engine::load(&opts_for(&dir, Target::Html)).unwrap();
    let mut log: Vec<u8> = Vec::new();
    engine.compile_all(&mut log);

    // The fix lives outside this file (here: a shortcode added to a new
    // brief.toml the watcher has not seen); saving again must retry.
    fs::write(
        dir.join("brief.toml"),
        "[shortcodes.nope]\nkind = \"inline\"\ntemplate_html = \"<b>{{content}}</b>\"\n",
    )
    .unwrap();
    fs::write(&a, "@nope[x]\n").unwrap();
    let mut log: Vec<u8> = Vec::new();
    watch::handle_change_paths(slice::from_ref(&a), &mut engine, &mut log);
    let log = String::from_utf8_lossy(&log);
    assert!(log.contains("a.html"), "{}", log);
    assert!(read(&dir.join("a.html")).contains("<b>x</b>"));
}
