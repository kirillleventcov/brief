//! File-watching wrapper around `notify-debouncer-mini`.
//!
//! Spawns a watcher on the project's `src/`, `book.toml`, and `theme/` paths,
//! debounces events to ~150ms, and invokes the supplied callback whenever a
//! batch of changes lands. The dev server uses this to trigger rebuilds and
//! push reload events to connected browsers.

use notify::RecursiveMode;
use notify_debouncer_mini::new_debouncer;
use std::path::{Path, PathBuf};
use std::sync::mpsc::channel;
use std::time::Duration;

pub struct WatchHandles {
    pub _debouncer: notify_debouncer_mini::Debouncer<notify::RecommendedWatcher>,
}

pub fn watch(paths: &[PathBuf], mut on_change: impl FnMut() + Send + 'static) -> WatchHandles {
    let (tx, rx) = channel();
    let mut debouncer = new_debouncer(Duration::from_millis(150), move |res| {
        let _ = tx.send(res);
    })
    .expect("creating watcher");
    for p in paths {
        if !p.exists() {
            continue;
        }
        let mode = if p.is_dir() {
            RecursiveMode::Recursive
        } else {
            RecursiveMode::NonRecursive
        };
        if let Err(e) = debouncer.watcher().watch(p, mode) {
            eprintln!("brief-web: cannot watch {}: {}", p.display(), e);
        }
    }
    std::thread::spawn(move || {
        for res in rx {
            // Ignore notify errors — they tend to be transient (e.g. a watched
            // file briefly disappearing during an editor's atomic save) and
            // treating them as change signals can drive a rebuild loop.
            if let Ok(events) = res {
                if !events.is_empty() {
                    on_change();
                }
            }
        }
    });
    WatchHandles {
        _debouncer: debouncer,
    }
}

pub fn default_watch_paths(project_dir: &Path, src: &Path, theme: Option<&Path>) -> Vec<PathBuf> {
    let mut paths = vec![src.to_path_buf(), project_dir.join("book.toml")];
    if let Some(t) = theme {
        paths.push(t.to_path_buf());
    }
    paths
}
