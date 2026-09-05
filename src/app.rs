use std::{path::PathBuf, time::{Duration, Instant}};

use ratatui::widgets::TableState;
use regex::Regex;

use crate::scan::{Measurement, TargetEntry, build_cache_path};

pub struct App {
    pub root: PathBuf,
    pub entries: Vec<TargetEntry>,
    pub build_cache: Option<TargetEntry>,
    pub build_cache_path: Option<PathBuf>,
    pub total_size: u64,
    pub table_state: TableState,
    pub scanning: bool,
    pub sort: SortKey,
    pub loading_start: Instant,
    /// True once the user moves selection. Suppresses the auto-jump to top.
    pub navigated: bool,
    /// True while the user is typing a filter pattern.
    pub filtering: bool,
    pub filter_text: String,
    /// Last filter that compiled. Matches against name and path.
    pub filter_regex: Option<Regex>,
    /// First line of the latest regex error, if the text does not compile.
    pub filter_error: Option<String>,
}

impl App {
    /// New dirs arriving within this window of a scan start take selection
    /// to the top, unless the user already navigated.
    const AUTOTOP_GRACE: Duration = Duration::from_secs(3);

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
            loading_start: Instant::now(),
            navigated: false,
            filtering: false,
            filter_text: String::new(),
            filter_regex: None,
            filter_error: None,
        }
    }

    pub fn set_discovered(&mut self, mut projects: Vec<PathBuf>) {
        projects.sort();
        let mut entries: Vec<TargetEntry> = projects
            .into_iter()
            .map(|project_path| TargetEntry {
                project_path,
                size: None,
                last_modified: None,
            })
            .collect();
        self.sort_entries(&mut entries);
        self.total_size = 0;
        self.entries = entries;
        self.scanning = true;
        self.clamp_selection();
        if !self.navigated && self.loading_start.elapsed() < Self::AUTOTOP_GRACE {
            self.top_unmarked();
        }
    }

    /// End the size walk. Measured rows keep their sizes; entries still
    /// pending keep `None` and read as `-` until the poller measures them.
    pub fn finish_scan(&mut self, build_cache: Option<TargetEntry>) {
        self.build_cache = build_cache;
        self.scanning = false;
        let mut entries = std::mem::take(&mut self.entries);
        self.sort_entries(&mut entries);
        self.entries = entries;
        self.clamp_selection();
    }

    fn clamp_selection(&mut self) {
        // Keep selection in bounds after rescan.
        let visible = self.visible_indices().len();
        if visible == 0 {
            self.table_state.select(None);
        } else {
            let selected = self.table_state.selected().unwrap_or(0);
            self.table_state.select(Some(selected.min(visible - 1)));
        }
    }

    pub fn cycle_sort(&mut self) {
        self.sort = self.sort.next();
        let mut entries = std::mem::take(&mut self.entries);
        self.sort_entries(&mut entries);
        self.entries = entries;
    }

    /// Replace the filter text; an invalid pattern keeps the last good one.
    pub fn set_filter(&mut self, text: String) {
        self.filter_text = text;
        if self.filter_text.is_empty() {
            self.filter_regex = None;
            self.filter_error = None;
        } else {
            match Regex::new(&self.filter_text) {
                Ok(re) => {
                    self.filter_regex = Some(re);
                    self.filter_error = None;
                }
                Err(e) => {
                    self.filter_error = Some(
                        e.to_string()
                            .lines()
                            .next()
                            .unwrap_or("invalid regex")
                            .to_string(),
                    );
                }
            }
        }
        self.top();
    }

    pub fn visible_indices(&self) -> Vec<usize> {
        match &self.filter_regex {
            None => (0..self.entries.len()).collect(),
            Some(re) => self
                .entries
                .iter()
                .enumerate()
                .filter(|(_, e)| {
                    re.is_match(&e.project_name()) || re.is_match(&e.project_path.to_string_lossy())
                })
                .map(|(i, _)| i)
                .collect(),
        }
    }

    #[tracing::instrument(skip_all)]
    pub fn apply_measurements(&mut self, measurements: &[Measurement]) {
        if measurements.is_empty() {
            return;
        }
        let selected_path = self
            .table_state
            .selected()
            .and_then(|i| self.visible_indices().get(i).copied())
            .and_then(|i| self.entries.get(i))
            .map(|e| e.project_path.clone());
        for m in measurements {
            if let Some(entry) = self
                .entries
                .iter_mut()
                .find(|e| e.project_path.join("target") == m.target_dir)
            {
                entry.size = Some(m.size);
                entry.last_modified = m.last_modified;
            } else if let Some(cache) = &mut self.build_cache
                && cache.project_path == m.target_dir
            {
                cache.size = Some(m.size);
                cache.last_modified = m.last_modified;
            } else if Some(&m.target_dir) == self.build_cache_path.as_ref()
                && m.last_modified.is_some()
            {
                // The build cache arrived after startup, so give it a row now.
                // The poller tracks it from the next reset.
                self.build_cache = Some(TargetEntry {
                    project_path: m.target_dir.clone(),
                    size: Some(m.size),
                    last_modified: m.last_modified,
                });
            }
        }
        self.total_size = self.entries.iter().filter_map(|e| e.size).sum();
        let mut entries = std::mem::take(&mut self.entries);
        self.sort_entries(&mut entries);
        self.entries = entries;
        if !self.navigated && self.loading_start.elapsed() < Self::AUTOTOP_GRACE {
            // Sizes churn the order while the walk runs. Stay glued to the
            // top until the user takes over or the grace window passes.
            self.top_unmarked();
        } else if let Some(path) = selected_path {
            let visible = self.visible_indices();
            let pos = visible
                .iter()
                .position(|&i| self.entries.get(i).is_some_and(|e| e.project_path == path));
            self.table_state
                .select(pos.or_else(|| visible.first().map(|_| 0)));
        }
    }

    /// Start a load and restart the shimmer sweep.
    pub fn begin_scan(&mut self) {
        self.scanning = true;
        self.loading_start = Instant::now();
        self.navigated = false;
    }

    pub fn next(&mut self) {
        self.navigated = true;
        let visible = self.visible_indices().len();
        if visible == 0 {
            return;
        }
        let i = self.table_state.selected().unwrap_or(0);
        self.table_state.select(Some((i + 1) % visible));
    }

    pub fn previous(&mut self) {
        self.navigated = true;
        let visible = self.visible_indices().len();
        if visible == 0 {
            return;
        }
        let i = self.table_state.selected().unwrap_or(0);
        self.table_state
            .select(Some(i.checked_sub(1).unwrap_or(visible - 1)));
    }

    pub fn top(&mut self) {
        self.navigated = true;
        if !self.visible_indices().is_empty() {
            self.table_state.select(Some(0));
        }
    }

    pub fn bottom(&mut self) {
        self.navigated = true;
        let visible = self.visible_indices().len();
        if visible > 0 {
            self.table_state.select(Some(visible - 1));
        }
    }

    /// Select the first row without counting as user navigation, so the
    /// scan can settle on top while the user is idle.
    fn top_unmarked(&mut self) {
        if self.visible_indices().is_empty() {
            self.table_state.select(None);
        } else {
            self.table_state.select(Some(0));
        }
    }

    fn sort_entries(&self, entries: &mut [TargetEntry]) {
        match self.sort {
            // Pending sizes sink below measured ones.
            SortKey::Size => entries.sort_by(|a, b| match (a.size, b.size) {
                (Some(x), Some(y)) => y.cmp(&x),
                (Some(_), None) => std::cmp::Ordering::Less,
                (None, Some(_)) => std::cmp::Ordering::Greater,
                (None, None) => a.project_path.cmp(&b.project_path),
            }),
            SortKey::Modified => {
                entries.sort_by(|a, b| match (&a.last_modified, &b.last_modified) {
                    // Pending and deleted dirs have no timestamp. They sink.
                    (None, None) => a.project_path.cmp(&b.project_path),
                    (None, _) => std::cmp::Ordering::Greater,
                    (_, None) => std::cmp::Ordering::Less,
                    (Some(x), Some(y)) => y.cmp(x).then(a.project_path.cmp(&b.project_path)),
                })
            }
            SortKey::Name => entries.sort_by(|a, b| a.project_path.cmp(&b.project_path)),
        }
    }
}

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

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::SystemTime;

    fn app_with_entries() -> App {
        let mut app = App::new(PathBuf::from("."));
        app.set_discovered(vec![PathBuf::from("proj-big"), PathBuf::from("proj-small")]);
        app.apply_measurements(&[
            Measurement {
                target_dir: PathBuf::from("proj-big/target"),
                size: 100,
                last_modified: Some(SystemTime::UNIX_EPOCH),
            },
            Measurement {
                target_dir: PathBuf::from("proj-small/target"),
                size: 10,
                last_modified: Some(SystemTime::UNIX_EPOCH),
            },
        ]);
        app.finish_scan(None);
        // Settle outside the auto-top grace window so tests exercise
        // steady-state selection instead of scan-start pinning.
        app.loading_start = Instant::now() - Duration::from_secs(3600);
        app
    }

    #[test]
    fn measurements_update_size_and_keep_selection_on_project() {
        let mut app = app_with_entries();
        // Select proj-small (index 1 after size-desc sort).
        app.table_state.select(Some(1));
        // proj-small grows past proj-big. Order flips but selection follows it.
        app.apply_measurements(&[Measurement {
            target_dir: PathBuf::from("proj-small/target"),
            size: 200,
            last_modified: Some(SystemTime::UNIX_EPOCH),
        }]);
        assert_eq!(app.entries[0].project_path, PathBuf::from("proj-small"));
        assert_eq!(app.total_size, 300);
        let selected = app
            .table_state
            .selected()
            .and_then(|i| app.entries.get(i))
            .expect("selection kept");
        assert_eq!(selected.project_path, PathBuf::from("proj-small"));
        assert_eq!(selected.size, Some(200));
    }

    #[test]
    fn unknown_measurement_is_ignored() {
        let mut app = app_with_entries();
        app.apply_measurements(&[Measurement {
            target_dir: PathBuf::from("gone/target"),
            size: 999,
            last_modified: Some(SystemTime::UNIX_EPOCH),
        }]);
        assert_eq!(app.total_size, 110);
    }

    #[test]
    fn discovered_rows_stay_pending_until_measured() {
        let mut app = App::new(PathBuf::from("."));
        app.set_discovered(vec![PathBuf::from("proj-b"), PathBuf::from("proj-a")]);
        assert!(app.scanning);
        assert_eq!(app.total_size, 0);
        assert!(app.entries.iter().all(|e| e.size.is_none()));
        // One measurement fills its row; pending rows sink, totals skip them.
        app.apply_measurements(&[Measurement {
            target_dir: PathBuf::from("proj-a/target"),
            size: 50,
            last_modified: Some(SystemTime::UNIX_EPOCH),
        }]);
        assert!(app.scanning);
        assert_eq!(app.total_size, 50);
        assert_eq!(app.entries[0].project_path, PathBuf::from("proj-a"));
        assert_eq!(app.entries[0].size, Some(50));
        assert_eq!(app.entries[1].size, None);
        // Finishing keeps still-pending rows pending for the poller.
        app.finish_scan(None);
        assert!(!app.scanning);
        assert_eq!(app.entries[1].size, None);
    }
    #[test]
    fn fresh_discovery_jumps_to_top() {
        let mut app = app_with_entries();
        // Sitting mid-list when a rescan lands: the new dirs take over.
        app.table_state.select(Some(1));
        app.begin_scan();
        app.set_discovered(vec![PathBuf::from("proj-small"), PathBuf::from("proj-big")]);
        assert_eq!(app.table_state.selected(), Some(0));
    }

    #[test]
    fn measurements_pin_to_top_until_taken_over() {
        let mut app = App::new(PathBuf::from("."));
        app.set_discovered(vec![PathBuf::from("proj-a"), PathBuf::from("proj-b")]);
        // proj-b measures first and jumps above; selection stays on top
        // instead of sinking with proj-a.
        app.apply_measurements(&[Measurement {
            target_dir: PathBuf::from("proj-b/target"),
            size: 200,
            last_modified: Some(SystemTime::UNIX_EPOCH),
        }]);
        assert_eq!(app.entries[0].project_path, PathBuf::from("proj-b"));
        assert_eq!(app.table_state.selected(), Some(0));
    }

    #[test]
    fn navigation_suppresses_auto_top() {
        let mut app = App::new(PathBuf::from("."));
        app.set_discovered(vec![PathBuf::from("proj-a"), PathBuf::from("proj-b")]);
        app.next();
        assert_eq!(app.table_state.selected(), Some(1));
        // A measurement re-sorts but follows the user's row now.
        app.apply_measurements(&[Measurement {
            target_dir: PathBuf::from("proj-a/target"),
            size: 200,
            last_modified: Some(SystemTime::UNIX_EPOCH),
        }]);
        assert_eq!(app.entries[1].project_path, PathBuf::from("proj-b"));
        assert_eq!(app.table_state.selected(), Some(1));
    }

    #[test]
    fn expired_grace_leaves_selection_alone() {
        let mut app = app_with_entries();
        app.table_state.select(Some(1));
        app.begin_scan();
        // Simulate a slow discovery arriving after the grace window.
        app.loading_start = Instant::now() - Duration::from_secs(3600);
        app.set_discovered(vec![PathBuf::from("proj-small"), PathBuf::from("proj-big")]);
        assert_eq!(app.table_state.selected(), Some(1));
        // Measuring the other row re-sorts around the user's row.
        app.apply_measurements(&[Measurement {
            target_dir: PathBuf::from("proj-big/target"),
            size: 200,
            last_modified: Some(SystemTime::UNIX_EPOCH),
        }]);
        assert_eq!(app.entries[0].project_path, PathBuf::from("proj-big"));
        assert_eq!(app.table_state.selected(), Some(1));
        let selected = app
            .table_state
            .selected()
            .and_then(|i| app.entries.get(i))
            .expect("selection kept");
        assert_eq!(selected.project_path, PathBuf::from("proj-small"));
    }

    #[test]
    fn deleted_and_recreated_dir_zeroes_then_restores_row() {
        use crate::scan::measure_target;
        let root = std::env::temp_dir().join("targeter-test-recreate");
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("proj/target")).unwrap();
        let mut app = App::new(root.clone());
        app.set_discovered(vec![root.join("proj")]);
        app.apply_measurements(&[Measurement {
            target_dir: root.join("proj/target"),
            size: 1,
            last_modified: Some(SystemTime::UNIX_EPOCH),
        }]);
        app.finish_scan(None);
        let target = root.join("proj/target");

        // Deletion zeroes the row and clears its timestamp.
        std::fs::remove_dir_all(&target).unwrap();
        app.apply_measurements(&[measure_target(&target)]);
        assert_eq!(app.entries[0].size, Some(0));
        assert_eq!(app.entries[0].last_modified, None);

        // Recreation restores the row with a real timestamp.
        std::fs::create_dir_all(target.join("debug")).unwrap();
        std::fs::write(target.join("debug/a.bin"), "1234").unwrap();
        app.apply_measurements(&[measure_target(&target)]);
        assert!(app.entries[0].size.is_some_and(|s| s > 0));
        assert!(app.entries[0].last_modified.is_some());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn first_cache_measurement_creates_entry() {
        use crate::scan::measure_target;
        let root = std::env::temp_dir().join("targeter-test-cache-arrival");
        let _ = std::fs::remove_dir_all(&root);
        let mut app = app_with_entries();
        app.build_cache = None;
        app.build_cache_path = Some(root.join("build-cache"));
        assert!(app.build_cache.is_none());

        // Nothing there yet: no row.
        app.apply_measurements(&[measure_target(&root.join("build-cache"))]);
        assert!(app.build_cache.is_none());

        // The cache appears: a row is created.
        std::fs::create_dir_all(root.join("build-cache/content")).unwrap();
        std::fs::write(root.join("build-cache/content/a.bin"), "12345678").unwrap();
        app.apply_measurements(&[measure_target(&root.join("build-cache"))]);
        let cache = app.build_cache.as_ref().expect("cache row created");
        assert!(cache.size.is_some_and(|s| s > 0));
    }

    #[test]
    fn filter_alternation_narrows_to_matches() {
        let mut app = app_with_entries();
        app.set_filter("big|zzz".to_string());
        assert_eq!(app.visible_indices(), vec![0]);
        app.set_filter("proj-".to_string());
        assert_eq!(app.visible_indices().len(), 2);
    }

    #[test]
    fn filter_matches_path_substring() {
        let mut app = App::new(PathBuf::from("."));
        app.set_discovered(vec![PathBuf::from("ws/member-a"), PathBuf::from("other")]);
        app.set_filter("member".to_string());
        assert_eq!(app.visible_indices(), vec![1]);
    }

    #[test]
    fn invalid_filter_keeps_last_good() {
        let mut app = app_with_entries();
        app.set_filter("big".to_string());
        assert_eq!(app.visible_indices(), vec![0]);
        app.set_filter("big(".to_string());
        assert!(app.filter_error.is_some());
        assert_eq!(app.visible_indices(), vec![0]);
        app.set_filter(String::new());
        assert!(app.filter_error.is_none());
        assert!(app.filter_regex.is_none());
        assert_eq!(app.visible_indices().len(), 2);
    }

    #[test]
    fn filter_resets_selection_and_nav_wraps_visible() {
        let mut app = app_with_entries();
        app.table_state.select(Some(1));
        app.set_filter("small".to_string());
        assert_eq!(app.visible_indices(), vec![1]);
        assert_eq!(app.table_state.selected(), Some(0));
        app.next();
        assert_eq!(app.table_state.selected(), Some(0));
        app.previous();
        assert_eq!(app.table_state.selected(), Some(0));
    }
}
