//! End-to-end subprocess test for `brief watch`.
//!
//! Lives in brief-cli (not brief-core) because it spawns the compiled
//! `brief` binary via `CARGO_BIN_EXE_brief`. The other watch tests — which
//! drive `Engine` / `handle_change_paths` directly without a subprocess —
//! live in `crates/brief-core/tests/watch.rs`.

use std::io::{BufRead, BufReader};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

fn temp_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("brief-watch-e2e-{}", name));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn brief_bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_brief"))
}

fn wait_for<F: FnMut() -> bool>(mut f: F, timeout: Duration) -> bool {
    let start = Instant::now();
    while start.elapsed() < timeout {
        if f() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    false
}

/// Smoke test for the spawned `brief watch` binary. Exercises the actual
/// notify backend and the 100ms debounce. Generous timeouts to keep this
/// non-flaky on CI.
#[test]
fn e2e_watch_recompiles_on_brf_change() {
    let dir = temp_dir("e2e-brf");
    let a = dir.join("a.brf");
    std::fs::write(&a, "# Hello\n").unwrap();
    let a_html = dir.join("a.html");

    let mut child = std::process::Command::new(brief_bin())
        .arg("watch")
        .arg(&dir)
        .arg("--target=html")
        .stderr(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .spawn()
        .expect("spawn brief watch");

    let got_initial = wait_for(
        || {
            a_html.exists()
                && std::fs::read_to_string(&a_html)
                    .unwrap_or_default()
                    .contains("Hello")
        },
        Duration::from_secs(8),
    );
    assert!(got_initial, "initial compile did not produce a.html");

    std::fs::write(&a, "# Updated\n").unwrap();
    let got_update = wait_for(
        || {
            std::fs::read_to_string(&a_html)
                .unwrap_or_default()
                .contains("Updated")
        },
        Duration::from_secs(8),
    );

    let _ = child.kill();
    let _ = child.wait();

    assert!(got_update, "watcher did not recompile after .brf change");
}

/// Spawn `brief watch` and collect its stderr lines as they arrive.
fn spawn_watch(dir: &std::path::Path) -> (std::process::Child, Arc<Mutex<Vec<String>>>) {
    let mut child = std::process::Command::new(brief_bin())
        .arg("watch")
        .arg(dir)
        .arg("--target=html")
        .arg("--no-clear")
        .stderr(std::process::Stdio::piped())
        .stdout(std::process::Stdio::null())
        .spawn()
        .expect("spawn brief watch");
    let lines = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&lines);
    let stderr = child.stderr.take().unwrap();
    std::thread::spawn(move || {
        for line in BufReader::new(stderr).lines().map_while(Result::ok) {
            sink.lock().unwrap().push(line);
        }
    });
    (child, lines)
}

/// Rebuild lines (`brief: <src> → <dst>`) logged so far for `src`.
fn rebuilds(lines: &Mutex<Vec<String>>, src: &str) -> usize {
    lines
        .lock()
        .unwrap()
        .iter()
        .filter(|l| l.contains(src) && l.contains(" → "))
        .count()
}

/// A compile reads its inputs, and inotify reports those opens. If they
/// counted as changes, every rebuild would trigger the next one and the
/// watcher would spin forever after the first edit.
#[test]
fn e2e_watch_goes_idle_after_an_edit() {
    let dir = temp_dir("e2e-idle");
    std::fs::write(dir.join("brief.toml"), "").unwrap();
    std::fs::write(dir.join("other.brf"), "# Other {#o}\n").unwrap();
    let a = dir.join("a.brf");
    std::fs::write(&a, "# Hello\n\nSee @ref[other.brf#o](other).\n").unwrap();

    let (mut child, lines) = spawn_watch(&dir);
    let ready = wait_for(
        || lines.lock().unwrap().iter().any(|l| l.contains("watching")),
        Duration::from_secs(8),
    );
    assert!(ready, "watcher never started: {:?}", lines.lock().unwrap());
    assert_eq!(rebuilds(&lines, "a.brf"), 1, "initial compile only");

    std::fs::write(&a, "# Updated\n\nSee @ref[other.brf#o](other).\n").unwrap();
    let rebuilt = wait_for(|| rebuilds(&lines, "a.brf") >= 2, Duration::from_secs(8));
    assert!(
        rebuilt,
        "edit was not picked up: {:?}",
        lines.lock().unwrap()
    );

    // One edit is one rebuild (an editor save may land as two events, but
    // never more), and the watcher then stays quiet.
    std::thread::sleep(Duration::from_millis(1500));
    let settled = rebuilds(&lines, "a.brf");
    std::thread::sleep(Duration::from_millis(1000));
    let later = rebuilds(&lines, "a.brf");

    let _ = child.kill();
    let _ = child.wait();

    let log = lines.lock().unwrap().join("\n");
    assert!(
        settled <= 3,
        "rebuilt {} times for one edit:\n{}",
        settled,
        log
    );
    assert_eq!(
        later, settled,
        "watcher kept rebuilding while idle:\n{}",
        log
    );
    assert_eq!(
        rebuilds(&lines, "other.brf"),
        1,
        "untouched file rebuilt:\n{}",
        log
    );
}
