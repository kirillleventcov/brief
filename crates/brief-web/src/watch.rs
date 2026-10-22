//! File-watching wrapper around `notify`.
//!
//! Spawns a watcher on the project's `src/`, `book.toml`, and `theme/` paths,
//! debounces events to ~150ms, and invokes the supplied callback whenever a
//! batch of changes lands. The dev server uses this to trigger rebuilds and
//! push reload events to connected browsers.

use notify::event::{AccessKind, AccessMode};
use notify::{Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{RecvTimeoutError, channel};
use std::thread;
use std::time::Duration;

/// Quiet period after the last change before the callback fires. An editor
/// save is several events; this coalesces them into one rebuild.
const DEBOUNCE: Duration = Duration::from_millis(150);

/// A running watcher. Watching stops when this is dropped.
pub struct WatchHandles {
    _watcher: RecommendedWatcher,
}

/// Watch `paths` (recursively for directories; missing paths are skipped)
/// and call `on_change` on a background thread once per debounced batch of
/// changes.
///
/// Fails if the platform watcher cannot be created. A path that cannot be
/// watched is reported on stderr and skipped.
pub fn watch(
    paths: &[PathBuf],
    mut on_change: impl FnMut() + Send + 'static,
) -> notify::Result<WatchHandles> {
    let (tx, rx) = channel();
    let mut watcher = notify::recommended_watcher(move |res: notify::Result<Event>| {
        // Ignore notify errors — they tend to be transient (e.g. a watched
        // file briefly disappearing during an editor's atomic save) and
        // treating them as change signals can drive a rebuild loop.
        if let Ok(event) = res
            && is_change(&event.kind)
        {
            let _ = tx.send(());
        }
    })?;
    for p in paths {
        if !p.exists() {
            continue;
        }
        let mode = if p.is_dir() {
            RecursiveMode::Recursive
        } else {
            RecursiveMode::NonRecursive
        };
        if let Err(e) = watcher.watch(p, mode) {
            eprintln!("brief-web: cannot watch {}: {}", p.display(), e);
        }
    }
    thread::spawn(move || {
        while rx.recv().is_ok() {
            loop {
                match rx.recv_timeout(DEBOUNCE) {
                    Ok(()) => continue,
                    Err(RecvTimeoutError::Timeout) => break,
                    Err(RecvTimeoutError::Disconnected) => return,
                }
            }
            on_change();
        }
    });
    Ok(WatchHandles { _watcher: watcher })
}

/// Whether an event means a watched path actually changed.
///
/// notify's inotify backend also reports opens (`IN_OPEN`), and a build opens
/// every source and theme file — so counting access events as changes makes
/// each rebuild trigger the next one, forever. A close-after-write is the one
/// access event that does signal new content.
fn is_change(kind: &EventKind) -> bool {
    match kind {
        EventKind::Access(AccessKind::Close(AccessMode::Write)) => true,
        EventKind::Access(_) => false,
        _ => true,
    }
}

/// The paths `brief-web serve` watches: the source directory, `book.toml`,
/// and the theme directory if there is one.
#[must_use]
pub fn default_watch_paths(project_dir: &Path, src: &Path, theme: Option<&Path>) -> Vec<PathBuf> {
    let mut paths = vec![src.to_path_buf(), project_dir.join("book.toml")];
    if let Some(t) = theme {
        paths.push(t.to_path_buf());
    }
    paths
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::slice;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[test]
    fn reads_do_not_fire_but_writes_do() {
        let td = tempfile::TempDir::new().unwrap();
        let src = td.path().join("src");
        fs::create_dir_all(src.join("posts")).unwrap();
        let page = src.join("posts/a.brf");
        fs::write(&page, "# A\n").unwrap();

        let fired = Arc::new(AtomicUsize::new(0));
        let counter = Arc::clone(&fired);
        let _handles = watch(slice::from_ref(&src), move || {
            counter.fetch_add(1, Ordering::SeqCst);
        })
        .unwrap();
        thread::sleep(Duration::from_millis(300));

        // What a build does to its own inputs: list directories, read files.
        for _ in 0..3 {
            let _ = fs::read_dir(&src).unwrap().count();
            let _ = fs::read_to_string(&page).unwrap();
        }
        thread::sleep(DEBOUNCE * 4);
        assert_eq!(
            fired.load(Ordering::SeqCst),
            0,
            "reading sources must not trigger a rebuild"
        );

        fs::write(&page, "# A\n\nedited\n").unwrap();
        thread::sleep(DEBOUNCE * 6);
        assert_eq!(
            fired.load(Ordering::SeqCst),
            1,
            "one write should trigger exactly one rebuild"
        );
    }
}
