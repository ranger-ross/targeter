use std::{path::PathBuf, time::Instant};

use ratatui::widgets::TableState;
use regex::Regex;

use crate::scan::{Measurement, TargetEntry, build_cache_path};

pub struct App {
    pub root: PathBuf,
    pub entries: Vec<TargetEntry>,
    pub build_cache: Option<TargetEntry>,
    /// Unstable cargo build cache location. Set even with no entry yet
    /// so polling covers its arrival.
    pub build_cache_path: Option<PathBuf>,
    pub total_size: u64,
    pub table_state: TableState,
    pub scanning: bool,
    pub sort: SortKey,
    /// Load start. The shimmer band reads wall-clock time.
    pub loading_start: Instant,
    /// True while the user is typing a filter pattern.
    pub filtering: bool,
    /// Raw filter text. Compiles live. Invalid input keeps the last good pattern.
    pub filter_text: String,
    /// Last filter that compiled. Matches against name and path.
    pub filter_regex: Option<Regex>,
    /// First line of the latest regex error, if the text does not compile.
    pub filter_error: Option<String>,
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
            loading_start: Instant::now(),
            filtering: false,
            filter_text: String::new(),
            filter_regex: None,
            filter_error: None,
        }
    }

    #[tracing::instrument(skip_all)]
    pub fn set_entries(&mut self, mut entries: Vec<TargetEntry>, build_cache: Option<TargetEntry>) {
        self.sort_entries(&mut entries);
        self.total_size = entries.iter().map(|e| e.size).sum();
        self.entries = entries;
        self.build_cache = build_cache;
        self.scanning = false;
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

    /// Replace the filter text and compile it live. An invalid pattern
    /// keeps the last good one and records the error instead.
    /// Selection restarts at the top of the narrowed list.
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

    /// Indices into `entries` that pass the filter, in order.
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

    /// Apply fresh measurements. Selection stays on the same project even
    /// when the new sizes reorder the table.
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
                entry.size = m.size;
                entry.last_modified = m.last_modified;
            } else if let Some(cache) = &mut self.build_cache
                && cache.project_path == m.target_dir
            {
                cache.size = m.size;
                cache.last_modified = m.last_modified;
            } else if Some(&m.target_dir) == self.build_cache_path.as_ref()
                && m.last_modified.is_some()
            {
                // The build cache arrived after startup, so give it a row now.
                // The poller tracks it from the next reset.
                self.build_cache = Some(TargetEntry {
                    project_path: m.target_dir.clone(),
                    size: m.size,
                    last_modified: m.last_modified,
                });
            }
        }
        self.total_size = self.entries.iter().map(|e| e.size).sum();
        let mut entries = std::mem::take(&mut self.entries);
        self.sort_entries(&mut entries);
        self.entries = entries;
        if let Some(path) = selected_path {
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
    }

    pub fn next(&mut self) {
        let visible = self.visible_indices().len();
        if visible == 0 {
            return;
        }
        let i = self.table_state.selected().unwrap_or(0);
        self.table_state.select(Some((i + 1) % visible));
    }

    pub fn previous(&mut self) {
        let visible = self.visible_indices().len();
        if visible == 0 {
            return;
        }
        let i = self.table_state.selected().unwrap_or(0);
        self.table_state
            .select(Some(i.checked_sub(1).unwrap_or(visible - 1)));
    }

    pub fn top(&mut self) {
        if !self.visible_indices().is_empty() {
            self.table_state.select(Some(0));
        }
    }

    pub fn bottom(&mut self) {
        let visible = self.visible_indices().len();
        if visible > 0 {
            self.table_state.select(Some(visible - 1));
        }
    }

    fn sort_entries(&self, entries: &mut [TargetEntry]) {
        match self.sort {
            SortKey::Size => entries.sort_by_key(|a| std::cmp::Reverse(a.size)),
            SortKey::Modified => {
                entries.sort_by(|a, b| match (&a.last_modified, &b.last_modified) {
                    // Deleted dirs have no timestamp. They always sink.
                    (None, None) => std::cmp::Ordering::Equal,
                    (None, _) => std::cmp::Ordering::Greater,
                    (_, None) => std::cmp::Ordering::Less,
                    (Some(x), Some(y)) => y.cmp(x),
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
    use crate::scan::TargetEntry;
    use std::time::SystemTime;

    fn entry(path: &str, size: u64) -> TargetEntry {
        TargetEntry {
            project_path: PathBuf::from(path),
            size,
            last_modified: Some(SystemTime::UNIX_EPOCH),
        }
    }

    fn app_with_entries() -> App {
        let mut app = App::new(PathBuf::from("."));
        app.set_entries(vec![entry("proj-big", 100), entry("proj-small", 10)], None);
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
        assert_eq!(selected.size, 200);
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
    fn deleted_and_recreated_dir_zeroes_then_restores_row() {
        use crate::scan::measure_target;
        let root = std::env::temp_dir().join("targeter-test-recreate");
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("proj/target")).unwrap();
        let mut app = App::new(root.clone());
        app.set_entries(
            vec![TargetEntry {
                project_path: root.join("proj"),
                size: 1,
                last_modified: Some(SystemTime::UNIX_EPOCH),
            }],
            None,
        );
        let target = root.join("proj/target");

        // Deletion zeroes the row and clears its timestamp.
        std::fs::remove_dir_all(&target).unwrap();
        app.apply_measurements(&[measure_target(&target)]);
        assert_eq!(app.entries[0].size, 0);
        assert_eq!(app.entries[0].last_modified, None);

        // Recreation restores the row with a real timestamp.
        std::fs::create_dir_all(target.join("debug")).unwrap();
        std::fs::write(target.join("debug/a.bin"), "1234").unwrap();
        app.apply_measurements(&[measure_target(&target)]);
        assert!(app.entries[0].size > 0);
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
        assert!(cache.size > 0);
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
        app.set_entries(vec![entry("ws/member-a", 5), entry("other", 6)], None);
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
