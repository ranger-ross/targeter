use std::{
    path::{Path, PathBuf},
    sync::mpsc,
    time::Duration,
};

use crossterm::{
    event::{self, Event},
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use eyre::{Context, Result};
use notify::{Config, EventKind, RecommendedWatcher, Watcher};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;

use app::App;
use args::Args;
use input::{Action, handle_key};
use scan::{Measurement, TargetEntry};

mod app;
mod args;
mod input;
mod scan;
mod ui;

fn main() -> Result<()> {
    let Args { root } = Args::new();
    if !root.is_dir() {
        eyre::bail!("scan root is not a directory: {}", root.display());
    }
    let root = resolve_root(&root);

    enable_raw_mode().wrap_err("enabling terminal raw mode")?;
    let mut stdout = std::io::stdout();
    execute!(stdout, EnterAlternateScreen).wrap_err("entering alternate screen")?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend).wrap_err("creating terminal")?;

    let result = run(&mut terminal, root);

    disable_raw_mode().wrap_err("disabling terminal raw mode")?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen).wrap_err("leaving alternate screen")?;
    terminal.show_cursor().wrap_err("restoring cursor")?;

    result
}

fn run(terminal: &mut Terminal<CrosstermBackend<std::io::Stdout>>, root: PathBuf) -> Result<()> {
    let mut app = App::new(root.clone());
    let mut scan_rx = spawn_scan(&root);
    // The watcher binding is never read; holding it keeps the watches alive.
    // It is rebuilt after every scan.
    let mut _watcher: Option<RecommendedWatcher> = None;
    let mut watcher_rx: Option<mpsc::Receiver<notify::Result<notify::Event>>> = None;
    // One-shot re-measure threads report back here. Only the newest flush
    // applies, so overlapping measures cannot write stale sizes.
    let (measure_tx, measure_rx) = mpsc::channel::<(u64, Vec<Measurement>)>();
    let mut flush_seq: u64 = 0;
    let mut latest_sent: u64 = 0;

    loop {
        terminal
            .draw(|frame| ui::render(frame, &mut app))
            .wrap_err("rendering frame")?;

        // Pick up a finished background scan without blocking the UI.
        if let Some(rx) = scan_rx.as_ref()
            && let Ok((entries, build_cache)) = rx.try_recv()
        {
            app.set_entries(entries, build_cache);
            scan_rx = None;
            rewatch(&mut app, &mut _watcher, &mut watcher_rx);
        }

        // Drain filesystem events into dirty dirs. Reads are ignored so our
        // own measuring never marks anything dirty.
        if let Some(rx) = watcher_rx.as_ref() {
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
            flush_seq += 1;
            latest_sent = flush_seq;
            let tx = measure_tx.clone();
            let id = flush_seq;
            std::thread::spawn(move || {
                let measurements = due.iter().map(|dir| scan::measure_target(dir)).collect();
                let _ = tx.send((id, measurements));
            });
        }

        while let Ok((id, measurements)) = measure_rx.try_recv() {
            if id == latest_sent {
                app.apply_measurements(measurements);
                if app.take_rewatch_needed() {
                    rewatch(&mut app, &mut _watcher, &mut watcher_rx);
                }
            }
        }

        if event::poll(Duration::from_millis(100)).wrap_err("polling terminal events")? {
            match event::read().wrap_err("reading terminal event")? {
                Event::Key(key) => match handle_key(&mut app, key) {
                    Action::Continue => {}
                    Action::Quit => return Ok(()),
                    Action::Rescan => {
                        app.scanning = true;
                        app.watching = false;
                        _watcher = None;
                        watcher_rx = None;
                        scan_rx = spawn_scan(&app.root);
                    }
                },
                Event::Resize(_, _) => {}
                _ => {}
            }
        }
    }
}

/// Run the recursive scan off the UI thread so the TUI stays responsive.
/// The build-cache measurement rides along so the UI gets both at once.
fn spawn_scan(root: &Path) -> Option<mpsc::Receiver<(Vec<TargetEntry>, Option<TargetEntry>)>> {
    let root = root.to_path_buf();
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let entries = scan::scan(&root);
        let build_cache = scan::build_cache_entry();
        let _ = tx.send((entries, build_cache));
    });
    Some(rx)
}

/// Watch every known target dir for live updates. Returns `None` when there
/// is nothing to watch. Failed watches are skipped, never fatal.
fn rebuild_watcher(
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

/// Drop the current watcher and build a fresh one for the latest entries.
/// Old watches die with the old watcher.
fn rewatch(
    app: &mut App,
    watcher: &mut Option<RecommendedWatcher>,
    watcher_rx: &mut Option<mpsc::Receiver<notify::Result<notify::Event>>>,
) {
    if let Some((w, rx)) = rebuild_watcher(app) {
        *watcher = Some(w);
        *watcher_rx = Some(rx);
        app.watching = true;
    } else {
        *watcher = None;
        *watcher_rx = None;
        app.watching = false;
    }
}

/// Resolve the scan root to an absolute path.
///
/// Filesystem watchers report absolute event paths, so a relative root would
/// silently break live updates: no event path would ever match a known dir.
fn resolve_root(raw: &Path) -> PathBuf {
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
