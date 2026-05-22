//! End-to-end subprocess test for `brief watch`.
//!
//! Lives in brief-cli (not brief-core) because it spawns the compiled
//! `brief` binary via `CARGO_BIN_EXE_brief`. The other watch tests — which
//! drive `Engine` / `handle_change_paths` directly without a subprocess —
//! live in `crates/brief-core/tests/watch.rs`.

use std::path::PathBuf;
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
