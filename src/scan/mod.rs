//! Find Rust projects and measure their `target/` dirs. Discovery walks in
//! parallel and honors ignore files, except gitignored `target/` dirs,
//! which still measure.

mod cache;
mod discover;
mod measure;

pub use cache::{build_cache_entry, build_cache_path};
pub use discover::scan;
pub use measure::{Measurement, measure_target};

use std::{
    path::{Path, PathBuf},
    time::SystemTime,
};

#[derive(Clone, Debug)]
pub struct TargetEntry {
    /// Project root (parent of `target/`).
    pub project_path: PathBuf,
    /// Disk usage of `target/` in bytes, `du` semantics.
    pub size: u64,
    /// Newest mtime under `target/`, or `None` while deleted.
    pub last_modified: Option<SystemTime>,
}

impl TargetEntry {
    pub fn project_name(&self) -> String {
        self.project_path
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned()
    }
}

pub fn resolve_root(raw: &Path) -> PathBuf {
    std::fs::canonicalize(raw).unwrap_or_else(|_| raw.to_path_buf())
}
