//! Find Rust projects and measure their `target/` dirs.
//! Discovery walks in parallel and honors ignore files, except that
//! gitignored `target/` dirs still measure.

mod cache;
mod discover;
mod measure;

pub use cache::{build_cache_entry, build_cache_path};
pub use discover::scan;
pub use measure::{Measurement, measure_target};

use std::{path::{Path, PathBuf}, time::SystemTime};

/// A Rust project with a `target/` directory on disk.
#[derive(Clone, Debug)]
pub struct TargetEntry {
    /// Project root (parent of `target/`).
    pub project_path: PathBuf,
    /// Disk usage of `target/` in bytes, `du` semantics.
    pub size: u64,
    /// Most recently modified mtime found under `target/`.
    pub last_modified: SystemTime,
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

/// Resolve the scan root to an absolute path, so displayed paths and
/// poller measurements stay absolute however the program was invoked.
pub fn resolve_root(raw: &Path) -> PathBuf {
    std::fs::canonicalize(raw).unwrap_or_else(|_| raw.to_path_buf())
}