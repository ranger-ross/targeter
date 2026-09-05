use std::{
    path::{Path, PathBuf},
    sync::mpsc,
};

use eyre::Context;
use notify::{Config, EventKind, RecommendedWatcher, Watcher};

use crate::app::App;
use crate::scan::{self, Measurement};

/// Owns the filesystem watcher and the re-measure pipeline.
///
/// Holding the watcher keeps the watches alive. It is rebuilt after every
/// scan and whenever a missing dir comes back.
pub struct LiveWatcher {
    watcher: Option<RecommendedWatcher>,
    watcher_rx: Option<mpsc::Receiver<notify::Result<notify::Event>>>,
    // One-shot re-measure threads report back here. Only the newest flush
    // applies, so overlapping measures cannot write stale sizes.
    measure_tx: mpsc::Sender<(u64, Vec<Measurement>)>,
    measure_rx: mpsc::Receiver<(u64, Vec<Measurement>)>,
    flush_seq: u64,
    latest_sent: u64,
}

impl LiveWatcher {
    pub fn new() -> Self {
        let (measure_tx, measure_rx) = mpsc::channel();
        Self {
            watcher: None,
            watcher_rx: None,
            measure_tx,
            measure_rx,
            flush_seq: 0,
            latest_sent: 0,
        }
    }

    /// Drop the current watcher and build a fresh one for the latest entries.
    /// Old watches die with the old watcher.
    pub fn rewatch(&mut self, app: &mut App) {
        if let Some((w, rx)) = rebuild_watcher(app) {
            self.watcher = Some(w);
            self.watcher_rx = Some(rx);
            app.watching = true;
        } else {
            self.clear(app);
        }
    }

    /// Drop all watches, e.g. before a full rescan.
    pub fn clear(&mut self, app: &mut App) {
        self.watcher = None;
        self.watcher_rx = None;
        app.watching = false;
    }

    /// Drain filesystem events into dirty dirs, flush due dirs to background
    /// measuring, and collect fresh measurements.
    pub fn poll(&mut self, app: &mut App) {
        // Reads are ignored so our own measuring never marks anything dirty.
        if let Some(rx) = self.watcher_rx.as_ref() {
            while let Ok(res) = rx.try_recv() {
                if let Ok(ev) = res
                    && !matches!(ev.kind, EventKind::Access(_))
                {
                    for path in &ev.paths {
                        if let Some(dir) = app.match_target_dir(path) {
                            app.mark_dirty(dir);
                        }
                    }
                }
            }
        }

        // Flush dirty dirs for measuring, at most one background job per debounce.
        if let Some(due) = app.take_dirty_if_due() {
            self.flush_seq += 1;
            self.latest_sent = self.flush_seq;
            let tx = self.measure_tx.clone();
            let id = self.flush_seq;
            std::thread::spawn(move || {
                let measurements = due.iter().map(|dir| scan::measure_target(dir)).collect();
                let _ = tx.send((id, measurements));
            });
        }

        while let Ok((id, measurements)) = self.measure_rx.try_recv() {
            if id == self.latest_sent {
                app.apply_measurements(measurements);
                if app.take_rewatch_needed() {
                    self.rewatch(app);
                }
            }
        }
    }
}

impl Default for LiveWatcher {
    fn default() -> Self {
        Self::new()
    }
}

/// Watch every known target dir for live updates. Returns `None` when there
/// is nothing to watch. Failed watches are skipped, never fatal.
pub fn rebuild_watcher(
    app: &App,
) -> Option<(
    RecommendedWatcher,
    mpsc::Receiver<notify::Result<notify::Event>>,
)> {
    let dirs = app.watch_dirs();
    if dirs.is_empty() {
        return None;
    }
    let (tx, rx) = mpsc::channel();
    let mut watcher = RecommendedWatcher::new(
        move |res| {
            let _ = tx.send(res);
        },
        Config::default(),
    )
    .wrap_err("creating filesystem watcher")
    .ok()?;
    for (dir, mode) in dirs {
        let _ = watcher.watch(&dir, mode);
    }
    Some((watcher, rx))
}

/// Resolve the scan root to an absolute path.
///
/// Filesystem watchers report absolute event paths, so a relative root would
/// silently break live updates: no event path would ever match a known dir.
pub fn resolve_root(raw: &Path) -> PathBuf {
    std::fs::canonicalize(raw).unwrap_or_else(|_| raw.to_path_buf())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scan_root_resolves_absolute() {
        // Live updates match absolute watcher event paths against known
        // dirs, so the root must never stay relative (e.g. ".").
        assert!(resolve_root(Path::new(".")).is_absolute());
    }

    #[test]
    fn scan_root_falls_back_when_missing() {
        let missing = PathBuf::from("targeter-no-such-dir-xyz");
        assert!(!missing.exists());
        assert_eq!(resolve_root(&missing), missing);
    }

    #[test]
    fn watched_target_deletion_maps_to_known_dir() {
        use std::time::{Duration, Instant};
        // End-to-end shape of the live path: scan, watch, delete, map.
        // Guards the relative-root bug where absolute watcher events never
        // matched relative known dirs.
        let raw = std::env::temp_dir().join("targeter-test-watch-delete");
        let _ = std::fs::remove_dir_all(&raw);
        std::fs::create_dir_all(raw.join("proj/target/debug")).unwrap();
        std::fs::write(raw.join("proj/Cargo.toml"), "[package]\n").unwrap();
        std::fs::write(raw.join("proj/target/debug/a.bin"), "12345678").unwrap();

        let root = resolve_root(&raw);
        assert!(root.is_absolute());
        let entries = scan::scan(&root);
        assert_eq!(entries.len(), 1);
        let mut app = App::new(root.clone());
        app.set_entries(entries, None);
        let (_watch, rx) = rebuild_watcher(&app).expect("watcher builds");
        std::thread::sleep(Duration::from_millis(500));
        while rx.try_recv().is_ok() {}

        std::fs::remove_dir_all(root.join("proj/target")).unwrap();
        let start = Instant::now();
        let mut mapped = 0usize;
        while start.elapsed() < Duration::from_secs(5) {
            let mut quiet = true;
            while let Ok(res) = rx.try_recv() {
                quiet = false;
                if let Ok(ev) = res
                    && !matches!(ev.kind, EventKind::Access(_))
                    && ev.paths.iter().any(|p| app.match_target_dir(p).is_some())
                {
                    mapped += 1;
                }
            }
            if mapped > 0 && quiet && start.elapsed() > Duration::from_millis(500) {
                break;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        let _ = std::fs::remove_dir_all(&raw);
        assert!(mapped > 0, "deletion must map to its watched target dir");
    }
}
