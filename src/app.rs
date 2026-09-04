use std::path::PathBuf;

use ratatui::widgets::TableState;

use crate::scan::TargetEntry;

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
    pub total_size: u64,
    pub table_state: TableState,
    pub scanning: bool,
    pub sort: SortKey,
}

impl App {
    pub fn new(root: PathBuf) -> Self {
        let mut table_state = TableState::default();
        table_state.select(Some(0));
        Self {
            root,
            entries: Vec::new(),
            total_size: 0,
            table_state,
            scanning: true,
            sort: SortKey::default(),
        }
    }

    pub fn set_entries(&mut self, mut entries: Vec<TargetEntry>) {
        self.sort_entries(&mut entries);
        self.total_size = entries.iter().map(|e| e.size).sum();
        self.entries = entries;
        self.scanning = false;
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
