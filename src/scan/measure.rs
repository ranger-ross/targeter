use std::{
    path::{Path, PathBuf},
    time::SystemTime,
};

use parallel_disk_usage::{
    data_tree::DataTree,
    device::DeviceBoundary,
    fs_tree_builder::FsTreeBuilder,
    hardlink::DeduplicateSharedSize,
    os_string_display::OsStringDisplay,
    reporter::{ErrorOnlyReporter, ErrorReport},
    size::Bytes,
};

#[cfg(not(unix))]
use parallel_disk_usage::{get_size::GetApparentSize, hardlink::HardlinkIgnorant};
#[cfg(unix)]
use parallel_disk_usage::{get_size::GetBlockSize, hardlink::HardlinkAware};

/// A fresh size reading for one watched directory, without a full rescan.
#[derive(Clone, Debug)]
pub struct Measurement {
    /// The `target/` dir (or build-cache dir) that was re-measured.
    pub target_dir: PathBuf,
    pub size: u64,
    pub last_modified: SystemTime,
}

/// Re-measure one known directory. Uses the same math as the full scan for one path.
#[tracing::instrument(skip_all, fields(target = %target_dir.display()))]
pub fn measure_target(target_dir: &Path) -> Measurement {
    let (size, last_modified) = recursive_scan_target(target_dir);
    Measurement {
        target_dir: target_dir.to_path_buf(),
        size,
        last_modified,
    }
}

/// Recursively measure disk usage and track the newest mtime.
///
/// Size matches `du`: parallel walk with allocated-block sizes, each inode
/// counted once. Missing paths and symlinks count as empty. Unreadable
/// subtrees add nothing.
#[tracing::instrument(skip_all, fields(path = %path.as_ref().display()))]
pub(super) fn recursive_scan_target<T: AsRef<Path>>(path: T) -> (u64, SystemTime) {
    let path = path.as_ref();
    if !path.exists() || path.is_symlink() {
        return (0, SystemTime::UNIX_EPOCH);
    }
    (dir_size(path), newest_mtime(path))
}

/// Disk usage of one directory tree via `parallel-disk-usage`.
#[cfg(unix)]
fn dir_size(path: &Path) -> u64 {
    let reporter = ErrorOnlyReporter::new(ErrorReport::SILENT);
    let recorder = HardlinkAware::new();
    let mut tree: DataTree<OsStringDisplay, Bytes> = FsTreeBuilder {
        root: path.to_path_buf(),
        size_getter: GetBlockSize,
        hardlinks_recorder: &recorder,
        reporter: &reporter,
        device_boundary: DeviceBoundary::Cross,
        max_depth: u64::MAX,
    }
    .into();
    // Count the first link of each inode only, like `du`.
    let _ = recorder.deduplicate(&mut tree);
    tree.size().into()
}

/// Allocated-block sizes are POSIX-only. Elsewhere fall back to apparent length.
#[cfg(not(unix))]
fn dir_size(path: &Path) -> u64 {
    let reporter = ErrorOnlyReporter::new(ErrorReport::SILENT);
    let recorder = HardlinkIgnorant;
    let mut tree: DataTree<OsStringDisplay, Bytes> = FsTreeBuilder {
        root: path.to_path_buf(),
        size_getter: GetApparentSize,
        hardlinks_recorder: &recorder,
        reporter: &reporter,
        device_boundary: DeviceBoundary::Cross,
        max_depth: u64::MAX,
    }
    .into();
    let _ = recorder.deduplicate(&mut tree);
    tree.size().into()
}

/// Newest mtime under a directory. Missing paths and symlinks count as
/// epoch. Unreadable subtrees add nothing.
fn newest_mtime(path: &Path) -> SystemTime {
    let default = SystemTime::UNIX_EPOCH;
    if !path.exists() || path.is_symlink() {
        return default;
    }
    let md = match path.metadata() {
        Ok(md) => md,
        Err(_) => return default,
    };
    let newest = md.modified().unwrap_or(default);
    if !md.is_dir() {
        return if md.is_file() { newest } else { default };
    }
    let mut latest = newest;
    if let Ok(rd) = path.read_dir() {
        for child in rd.filter_map(|it| it.ok().map(|it| it.path())) {
            latest = latest.max(newest_mtime(&child));
        }
    }
    latest
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::time::SystemTime;

    // Dedup relies on inode identity, which only Unix exposes.
    #[cfg(unix)]
    #[test]
    fn hardlinked_file_counts_once_like_du() {
        let root = std::env::temp_dir().join("targeter-test-hardlink");
        let _ = fs::remove_dir_all(&root);
        let target = root.join("target");
        fs::create_dir_all(&target).unwrap();
        fs::write(target.join("a.bin"), "1234").unwrap();
        fs::write(target.join("b.bin"), "123456").unwrap();

        let (alone, _) = recursive_scan_target(&target);
        assert!(alone > 0);
        // Second link to the same inode: `du` counts nothing extra.
        fs::hard_link(target.join("a.bin"), target.join("a-link.bin")).unwrap();
        let (deduped, mtime) = recursive_scan_target(&target);
        assert_eq!(deduped, alone);
        assert!(mtime > SystemTime::UNIX_EPOCH);
        // Missing path degrades to empty.
        assert_eq!(
            recursive_scan_target(root.join("nope")),
            (0, SystemTime::UNIX_EPOCH)
        );
        let _ = fs::remove_dir_all(&root);
    }
}
