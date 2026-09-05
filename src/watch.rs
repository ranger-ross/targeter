use std::{
    path::{Path, PathBuf},
    sync::mpsc,
};

use eyre::Context;
use notify::{Config, EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use rayon::prelude::*;

use crate::app::App;
use crate::scan::{self, Measurement};

/// Owns the filesystem watcher and the re-measure pipeline.
///
/// Watch registration walks every subdirectory, so it runs on a worker
/// thread and installs on a later `poll`. Only the latest request installs;
/// replaced builds die silently when their channel drops.
pub struct LiveWatcher {
    watcher: Option<RecommendedWatcher>,
    watcher_rx: Option<mpsc::Receiver<notify::Result<notify::Event>>>,
    pending: Option<mpsc::Receiver<BuiltWatcher>>,
    // One-shot re-measure threads report back here. Only the newest flush
    // applies, so overlapping measures cannot write stale sizes.
    measure_tx: mpsc::Sender<(u64, Vec<Measurement>)>,
    measure_rx: mpsc::Receiver<(u64, Vec<Measurement>)>,
    flush_seq: u64,
    latest_sent: u64,
}

/// A finished watch build. `None` when there was nothing to watch.
type BuiltWatcher = Option<(
    RecommendedWatcher,
    mpsc::Receiver<notify::Result<notify::Event>>,
)>;

impl LiveWatcher {
    pub fn new() -> Self {
        let (measure_tx, measure_rx) = mpsc::channel();
        Self {
            watcher: None,
            watcher_rx: None,
            pending: None,
            measure_tx,
            measure_rx,
            flush_seq: 0,
            latest_sent: 0,
        }
    }

    /// Drop current watches and rebuild fresh ones on a worker thread.
    /// Returns at once; the new watches install on a later `poll`.
    #[tracing::instrument(skip_all)]
    pub fn rewatch(&mut self, app: &mut App) {
        let dirs = app.watch_dirs();
        self.clear(app);
        if dirs.is_empty() {
            return;
        }
        let (tx, rx) = mpsc::channel();
        self.pending = Some(rx);
        std::thread::spawn(move || {
            let _span = tracing::info_span!("rebuild_watcher", dirs = dirs.len()).entered();
            let _ = tx.send(build_watcher(&dirs));
        });
    }

    /// Drop all watches and forget any pending build, e.g. before a full rescan.
    pub fn clear(&mut self, app: &mut App) {
        self.watcher = None;
        self.watcher_rx = None;
        self.pending = None;
        app.watching = false;
    }

    /// Install a finished background watch build, if any.
    fn collect_pending(&mut self, app: &mut App) {
        let Some(rx) = self.pending.take() else {
            return;
        };
        match rx.try_recv() {
            Ok(Some((watcher, events))) => {
                self.watcher = Some(watcher);
                self.watcher_rx = Some(events);
                app.watching = true;
            }
            Ok(None) => self.clear(app),
            Err(mpsc::TryRecvError::Empty) => self.pending = Some(rx),
            Err(mpsc::TryRecvError::Disconnected) => self.clear(app),
        }
    }

    /// Drain filesystem events into dirty dirs, flush due dirs to background
    /// measuring, and collect fresh measurements.
    #[tracing::instrument(skip_all)]
    pub fn poll(&mut self, app: &mut App) {
        self.collect_pending(app);
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
                let measurements = due
                    .par_iter()
                    .map(|dir| scan::measure_target(dir))
                    .collect();
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

/// Build watches for the given dirs. Slow: recursive watches register each
/// subdirectory. Always runs on a worker; see `LiveWatcher::rewatch`.
/// Returns `None` when there is nothing to watch. Failed watches are
/// skipped, never fatal.
#[tracing::instrument(skip_all)]
fn build_watcher(dirs: &[(PathBuf, RecursiveMode)]) -> BuiltWatcher {
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
        let _ = watcher.watch(dir, *mode);
    }
    Some((watcher, rx))
}

/// Resolve the scan root to an absolute path.
///
/// Filesystem watchers report absolute event paths. A relative root would break
/// live updates because no event path would match a known dir.
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
        let (_watch, rx) = build_watcher(&app.watch_dirs()).expect("watcher builds");
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

    #[test]
    fn rewatch_returns_at_once_and_installs_on_poll() {
        use std::time::{Duration, Instant};
        // The 3s main-thread freeze: watch registration must not block.
        let raw = std::env::temp_dir().join("targeter-test-watch-async");
        let _ = std::fs::remove_dir_all(&raw);
        std::fs::create_dir_all(raw.join("proj/target")).unwrap();
        std::fs::write(raw.join("proj/Cargo.toml"), "[package]\n").unwrap();
        std::fs::write(raw.join("proj/target/a.bin"), "12345678").unwrap();

        let root = resolve_root(&raw);
        let entries = scan::scan(&root);
        assert_eq!(entries.len(), 1);
        let mut app = App::new(root.clone());
        app.set_entries(entries, None);
        let mut live = LiveWatcher::new();
        live.rewatch(&mut app);
        // Returns before any build: nothing installed yet.
        assert!(!app.watching);
        // A later poll installs the background build.
        let start = Instant::now();
        while !app.watching && start.elapsed() < Duration::from_secs(5) {
            live.poll(&mut app);
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(app.watching, "background build installs on poll");
        // The installed watcher is live: deleting target zeroes the row.
        std::fs::remove_dir_all(root.join("proj/target")).unwrap();
        let start = Instant::now();
        while app.entries[0].size > 0 && start.elapsed() < Duration::from_secs(8) {
            live.poll(&mut app);
            std::thread::sleep(Duration::from_millis(50));
        }
        assert_eq!(app.entries[0].size, 0);
        let _ = std::fs::remove_dir_all(&raw);
    }
}
