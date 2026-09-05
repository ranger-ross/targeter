//! Find Rust projects and measure their `target/` dirs. Discovery walks in
//! parallel and honors ignore files, except gitignored `target/` dirs,
//! which still measure.

mod cache;
mod cargo_config;
mod discover;
mod measure;

pub use cache::build_cache_path;
pub use cargo_config::DiscoveredEntry;
pub use discover::{ScanEvent, scan_stream};
pub use measure::{Measurement, measure_target};

use std::{
    path::{Path, PathBuf},
    time::SystemTime,
};

#[derive(Clone, Debug)]
pub struct TargetEntry {
    /// Dir holding `Cargo.toml`.
    pub project_path: PathBuf,
    /// Measured artifact dir. Defaults to `project_path/target`; a
    /// `build.target-dir` / `build.build-dir` config points elsewhere.
    pub target_dir: PathBuf,
    /// Disk usage of `target_dir` in bytes, `du` semantics. `None` while
    /// the size walk has not measured this entry yet.
    pub size: Option<u64>,
    /// Newest mtime under `target_dir`, or `None` while pending or deleted.
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
