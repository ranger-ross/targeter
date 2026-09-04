use std::{
    collections::HashSet,
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

use notify::RecursiveMode;
use ratatui::widgets::TableState;

use crate::scan::{Measurement, TargetEntry, build_cache_path};

/// Pause between a change event and re-measuring, so a burst of writes
/// during a build triggers one re-measure instead of one per file.
const MEASURE_DEBOUNCE: Duration = Duration::from_millis(500);

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SortKey {
    #[default]
    Size,
    Modified,
    Name,
}

impl SortKey {
    pub fn next(self) -> Self {
        match self {
            Self::Size => Self::Modified,
            Self::Modified => Self::Name,
            Self::Name => Self::Size,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Size => "size",
            Self::Modified => "modified",
            Self::Name => "name",
        }
    }
}
pub struct App {
    pub root: PathBuf,
    pub entries: Vec<TargetEntry>,
    pub build_cache: Option<TargetEntry>,
    /// Where the unstable cargo build cache lives, even when it has no
    /// entry yet. Lets the watcher and matcher cover its arrival.
    pub build_cache_path: Option<PathBuf>,
    pub total_size: u64,
    pub table_state: TableState,
    pub scanning: bool,
    pub sort: SortKey,
    /// True while filesystem watchers cover the known target dirs.
    pub watching: bool,
    /// Watched dirs with unprocessed change events.
    dirty: HashSet<PathBuf>,
    /// Last time dirty entries were flushed for measuring.
    last_flush: Instant,
    /// Watched dirs that were missing when last measured. Their recursive
    /// watches died with them, so they need re-watching on return.
    missing: HashSet<PathBuf>,
    /// A missing dir came back; the watcher must be rebuilt.
    rewatch_needed: bool,
}
impl App {
    pub fn new(root: PathBuf) -> Self {
        let mut table_state = TableState::default();
        table_state.select(Some(0));
        Self {
            root,
            entries: Vec::new(),
            build_cache: None,
            build_cache_path: build_cache_path(),
            total_size: 0,
            table_state,
            scanning: true,
            sort: SortKey::default(),
            watching: false,
            dirty: HashSet::new(),
            last_flush: Instant::now(),
            missing: HashSet::new(),
            rewatch_needed: false,
        }
    }

    pub fn set_entries(&mut self, mut entries: Vec<TargetEntry>, build_cache: Option<TargetEntry>) {
        self.sort_entries(&mut entries);
        self.total_size = entries.iter().map(|e| e.size).sum();
        self.entries = entries;
        self.build_cache = build_cache;
        self.scanning = false;
        // Watched dirs changed, so pending change events no longer apply.
        self.dirty.clear();
        self.last_flush = Instant::now();
        self.missing.clear();
        self.rewatch_needed = false;
        // Keep selection in bounds after rescan.
        if self.entries.is_empty() {
            self.table_state.select(None);
        } else {
            let selected = self.table_state.selected().unwrap_or(0);
            self.table_state
                .select(Some(selected.min(self.entries.len() - 1)));
        }
    }

    pub fn cycle_sort(&mut self) {
        self.sort = self.sort.next();
        let mut entries = std::mem::take(&mut self.entries);
        self.sort_entries(&mut entries);
        self.entries = entries;
    }
    fn sort_entries(&self, entries: &mut [TargetEntry]) {
        match self.sort {
            SortKey::Size => entries.sort_by_key(|a| std::cmp::Reverse(a.size)),
            SortKey::Modified => entries.sort_by_key(|a| std::cmp::Reverse(a.last_modified)),
            SortKey::Name => entries.sort_by(|a, b| a.project_path.cmp(&b.project_path)),
        }
    }

    /// Every directory currently watched: each known `target/`, plus the
    /// build cache location even before it has an entry.
    pub fn target_dirs(&self) -> Vec<PathBuf> {
        let mut dirs: Vec<PathBuf> = self
            .entries
            .iter()
            .map(|e| e.project_path.join("target"))
            .collect();
        if let Some(cache) = &self.build_cache {
            dirs.push(cache.project_path.clone());
        } else if let Some(path) = &self.build_cache_path {
            dirs.push(path.clone());
        }
        dirs
    }

    /// Directories to hand to the filesystem watcher. Each `target/` is
    /// watched recursively, plus its parent non-recursively: deleting a
    /// `target/` kills its own watches, but the parent survives and reports
    /// the recreation.
    pub fn watch_dirs(&self) -> Vec<(PathBuf, RecursiveMode)> {
        let mut dirs = Vec::new();
        for entry in &self.entries {
            dirs.push((entry.project_path.clone(), RecursiveMode::NonRecursive));
            dirs.push((entry.project_path.join("target"), RecursiveMode::Recursive));
        }
        let cache_path = self
            .build_cache
            .as_ref()
            .map(|c| c.project_path.clone())
            .or_else(|| self.build_cache_path.clone());
        if let Some(path) = cache_path {
            if let Some(parent) = path.parent() {
                dirs.push((parent.to_path_buf(), RecursiveMode::NonRecursive));
            }
            dirs.push((path, RecursiveMode::Recursive));
        }
        dirs
    }

    /// Find which watched dir owns a changed path, if any.
    pub fn match_target_dir(&self, path: &Path) -> Option<PathBuf> {
        self.target_dirs()
            .into_iter()
            .find(|dir| path.starts_with(dir))
    }

    /// True when a missing dir came back and watches must be rebuilt.
    /// Resets on read.
    pub fn take_rewatch_needed(&mut self) -> bool {
        std::mem::replace(&mut self.rewatch_needed, false)
    }

    /// Record a change event for later measuring.
    pub fn mark_dirty(&mut self, target_dir: PathBuf) {
        self.dirty.insert(target_dir);
    }

    /// Hand over dirty dirs once the debounce has passed. Returns `None`
    /// when there is nothing to do yet, so callers can poll this freely.
    pub fn take_dirty_if_due(&mut self) -> Option<Vec<PathBuf>> {
        if self.dirty.is_empty() || self.last_flush.elapsed() < MEASURE_DEBOUNCE {
            return None;
        }
        self.last_flush = Instant::now();
        Some(self.dirty.drain().collect())
    }

    /// Apply fresh measurements. Selection stays on the same project even
    /// when the new sizes reorder the table.
    pub fn apply_measurements(&mut self, measurements: Vec<Measurement>) {
        if measurements.is_empty() {
            return;
        }
        let selected_path = self
            .table_state
            .selected()
            .and_then(|i| self.entries.get(i))
            .map(|e| e.project_path.clone());
        for m in measurements {
            if m.target_dir.is_dir() {
                if self.missing.remove(&m.target_dir) {
                    // Came back after a deletion; its recursive watches died
                    // with it, so they must be re-established.
                    self.rewatch_needed = true;
                }
            } else {
                self.missing.insert(m.target_dir.clone());
            }
            if let Some(entry) = self
                .entries
                .iter_mut()
                .find(|e| e.project_path.join("target") == m.target_dir)
            {
                entry.size = m.size;
                entry.last_modified = m.last_modified;
            } else if let Some(cache) = &mut self.build_cache
                && cache.project_path == m.target_dir
            {
                cache.size = m.size;
                cache.last_modified = m.last_modified;
            } else if Some(&m.target_dir) == self.build_cache_path.as_ref() && m.target_dir.is_dir()
            {
                // The build cache arrived after startup; it gets a row now.
                self.build_cache = Some(TargetEntry {
                    project_path: m.target_dir.clone(),
                    size: m.size,
                    last_modified: m.last_modified,
                });
                // Nothing watches the new tree yet.
                self.rewatch_needed = true;
            }
        }
        self.total_size = self.entries.iter().map(|e| e.size).sum();
        let mut entries = std::mem::take(&mut self.entries);
        self.sort_entries(&mut entries);
        self.entries = entries;
        if let Some(path) = selected_path
            && let Some(i) = self.entries.iter().position(|e| e.project_path == path)
        {
            self.table_state.select(Some(i));
        }
    }

    pub fn next(&mut self) {
        if self.entries.is_empty() {
            return;
        }
        let i = self.table_state.selected().unwrap_or(0);
        self.table_state.select(Some((i + 1) % self.entries.len()));
    }

    pub fn previous(&mut self) {
        if self.entries.is_empty() {
            return;
        }
        let i = self.table_state.selected().unwrap_or(0);
        self.table_state
            .select(Some(i.checked_sub(1).unwrap_or(self.entries.len() - 1)));
    }

    pub fn top(&mut self) {
        if !self.entries.is_empty() {
            self.table_state.select(Some(0));
        }
    }

    pub fn bottom(&mut self) {
        if !self.entries.is_empty() {
            self.table_state.select(Some(self.entries.len() - 1));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scan::TargetEntry;
    use std::time::SystemTime;

    fn entry(path: &str, size: u64) -> TargetEntry {
        TargetEntry {
            project_path: PathBuf::from(path),
            size,
            last_modified: SystemTime::UNIX_EPOCH,
        }
    }

    fn app_with_entries() -> App {
        let mut app = App::new(PathBuf::from("."));
        app.set_entries(vec![entry("proj-big", 100), entry("proj-small", 10)], None);
        app
    }

    #[test]
    fn changed_path_maps_to_its_target_dir() {
        let app = app_with_entries();
        assert_eq!(
            app.match_target_dir(Path::new("proj-big/target/debug/foo")),
            Some(PathBuf::from("proj-big/target"))
        );
        assert_eq!(app.match_target_dir(Path::new("elsewhere/foo")), None);
    }

    #[test]
    fn measurements_update_size_and_keep_selection_on_project() {
        let mut app = app_with_entries();
        // Select proj-small (index 1 after size-desc sort).
        app.table_state.select(Some(1));
        // proj-small grows past proj-big; order flips but selection follows it.
        app.mark_dirty(PathBuf::from("proj-small/target"));
        let dirty = app.take_dirty_if_due();
        // Debounce may hold the flush; force it only when due.
        let due = dirty.unwrap_or_else(|| {
            std::thread::sleep(std::time::Duration::from_millis(550));
            app.take_dirty_if_due().expect("debounce passed")
        });
        assert_eq!(due, vec![PathBuf::from("proj-small/target")]);
        app.apply_measurements(vec![Measurement {
            target_dir: PathBuf::from("proj-small/target"),
            size: 200,
            last_modified: SystemTime::UNIX_EPOCH,
        }]);
        assert_eq!(app.entries[0].project_path, PathBuf::from("proj-small"));
        assert_eq!(app.total_size, 300);
        let selected = app
            .table_state
            .selected()
            .and_then(|i| app.entries.get(i))
            .expect("selection kept");
        assert_eq!(selected.project_path, PathBuf::from("proj-small"));
        assert_eq!(selected.size, 200);
    }

    #[test]
    fn unknown_measurement_is_ignored() {
        let mut app = app_with_entries();
        app.apply_measurements(vec![Measurement {
            target_dir: PathBuf::from("gone/target"),
            size: 999,
            last_modified: SystemTime::UNIX_EPOCH,
        }]);
        assert_eq!(app.total_size, 110);
    }

    #[test]
    fn watch_dirs_cover_targets_recursively_and_parents_plainly() {
        let mut app = app_with_entries();
        app.build_cache = Some(entry("/cache/build-cache", 7));
        let dirs = app.watch_dirs();
        assert!(dirs.contains(&(PathBuf::from("proj-big/target"), RecursiveMode::Recursive)));
        assert!(dirs.contains(&(PathBuf::from("proj-big"), RecursiveMode::NonRecursive)));
        assert!(dirs.contains(&(
            PathBuf::from("/cache/build-cache"),
            RecursiveMode::Recursive
        )));
        assert!(dirs.contains(&(PathBuf::from("/cache"), RecursiveMode::NonRecursive)));
    }

    #[test]
    fn recreated_dir_requests_rewatch_once() {
        use crate::scan::measure_target;
        let root = std::env::temp_dir().join("targeter-test-rewatch");
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("proj/target")).unwrap();
        let mut app = App::new(root.clone());
        app.set_entries(
            vec![TargetEntry {
                project_path: root.join("proj"),
                size: 1,
                last_modified: SystemTime::UNIX_EPOCH,
            }],
            None,
        );
        let target = root.join("proj/target");

        // Deletion zeroes the row but asks for no rebuild: the parent watch
        // survives and already covers the recreation.
        std::fs::remove_dir_all(&target).unwrap();
        app.apply_measurements(vec![measure_target(&target)]);
        assert_eq!(app.entries[0].size, 0);
        assert!(!app.take_rewatch_needed());

        // Recreation restores the row and asks for exactly one rebuild, so
        // the new tree gets recursive watches again.
        std::fs::create_dir_all(target.join("debug")).unwrap();
        std::fs::write(target.join("debug/a.bin"), "1234").unwrap();
        app.apply_measurements(vec![measure_target(&target)]);
        assert!(app.entries[0].size > 0);
        assert!(app.take_rewatch_needed());
        assert!(!app.take_rewatch_needed());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn prospective_cache_is_watched_and_matched() {
        let mut app = app_with_entries();
        app.build_cache = None;
        app.build_cache_path = Some(PathBuf::from("/cache/build-cache"));
        let dirs = app.watch_dirs();
        assert!(dirs.contains(&(
            PathBuf::from("/cache/build-cache"),
            RecursiveMode::Recursive
        )));
        assert!(dirs.contains(&(PathBuf::from("/cache"), RecursiveMode::NonRecursive)));
        assert_eq!(
            app.match_target_dir(Path::new("/cache/build-cache/content/x")),
            Some(PathBuf::from("/cache/build-cache"))
        );
    }

    #[test]
    fn first_cache_measurement_creates_entry_and_rewatches() {
        use crate::scan::measure_target;
        let root = std::env::temp_dir().join("targeter-test-cache-arrival");
        let _ = std::fs::remove_dir_all(&root);
        let mut app = app_with_entries();
        app.build_cache = None;
        app.build_cache_path = Some(root.join("build-cache"));
        assert!(app.build_cache.is_none());

        // Nothing there yet: no row, no rebuild.
        app.apply_measurements(vec![measure_target(&root.join("build-cache"))]);
        assert!(app.build_cache.is_none());
        assert!(!app.take_rewatch_needed());

        // The cache appears: a row is created and watches are rebuilt for it.
        std::fs::create_dir_all(root.join("build-cache/content")).unwrap();
        std::fs::write(root.join("build-cache/content/a.bin"), "12345678").unwrap();
        app.apply_measurements(vec![measure_target(&root.join("build-cache"))]);
        let cache = app.build_cache.as_ref().expect("cache row created");
        assert!(cache.size > 0);
        assert!(app.take_rewatch_needed());
    }
}
