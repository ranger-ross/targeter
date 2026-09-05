use std::{
    path::{Path, PathBuf},
    thread,
    time::SystemTime,
};

use crossbeam_channel::Sender;
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
/// Scan `root` recursively for cargo projects with a `target/` dir.
///
/// Mirrors `cargo-clean-all` detection logic:
/// - a directory containing `Cargo.toml` is a project
/// - if that directory also contains a `target/` subdirectory it is reported
/// - `.git` and `.cargo` are never descended into
/// - `target/` itself is never descended into for further project detection
#[tracing::instrument(skip_all, fields(root = %root.display()))]
pub fn scan(root: &Path) -> Vec<TargetEntry> {
    let num_threads = num_cpus::get().max(1);
    let projects: Vec<PathBuf> = thread::scope(|scope| {
        let (job_tx, job_rx) = crossbeam_channel::unbounded::<Job>();
        let (result_tx, result_rx) = crossbeam_channel::unbounded::<PathBuf>();

        for _ in 0..num_threads {
            let job_rx = job_rx.clone();
            let result_tx = result_tx.clone();
            scope.spawn(move || {
                for job in job_rx {
                    find_projects_task(job, &result_tx);
                }
            });
        }

        job_tx
            .send(Job::new(root.to_path_buf(), job_tx.clone()))
            .expect("scan channel alive");
        // Dropping our copy lets the receiver iterator terminate once
        // workers drain the queue. Workers hold the remaining senders.
        drop(job_tx);
        drop(result_tx);

        result_rx.into_iter().collect()
    });

    let mut entries: Vec<TargetEntry> = projects
        .iter()
        .map(|project_path| {
            let (size, last_modified) = recursive_scan_target(project_path.join("target"));
            TargetEntry {
                project_path: project_path.clone(),
                size,
                last_modified,
            }
        })
        .collect();

    // Biggest offenders first: most useful for a disk monitor.
    entries.sort_by_key(|a| std::cmp::Reverse(a.size));
    entries
}

/// The path scanned for the unstable cargo build cache (`$CARGO_HOME/build-cache`).
///
/// `CARGO_HOME` wins when set. It falls back to `$HOME/.cargo`.
/// Returns `None` when neither variable resolves to a usable path.
pub fn build_cache_path() -> Option<PathBuf> {
    if let Ok(home) = std::env::var("CARGO_HOME")
        && !home.trim().is_empty()
    {
        return Some(PathBuf::from(home).join("build-cache"));
    }
    std::env::var("HOME")
        .ok()
        .filter(|home| !home.trim().is_empty())
        .map(|home| PathBuf::from(home).join(".cargo").join("build-cache"))
}

/// Measure the unstable cargo build cache, if present.
///
/// A missing directory means the cache is disabled or unused, so there is
/// nothing to report rather than a zero-size entry.
#[tracing::instrument]
pub fn build_cache_entry() -> Option<TargetEntry> {
    build_cache_path().and_then(|path| build_cache_entry_at(&path))
}

/// Measure the build cache at an explicit path. Separate helper so tests
/// avoid the real `$CARGO_HOME`.
pub fn build_cache_entry_at(path: &Path) -> Option<TargetEntry> {
    if !path.is_dir() || path.is_symlink() {
        return None;
    }
    let (size, last_modified) = recursive_scan_target(path);
    Some(TargetEntry {
        project_path: path.to_path_buf(),
        size,
        last_modified,
    })
}

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

/// Work item for the scanner thread pool.
struct Job {
    path: PathBuf,
    sender: Sender<Job>,
}

impl Job {
    fn new(path: PathBuf, sender: Sender<Job>) -> Self {
        Self { path, sender }
    }

    fn explore_recursive(&self, path: PathBuf) {
        // Receiver may be gone during shutdown; a failed send just drops the job.
        let _ = self.sender.send(Job::new(path, self.sender.clone()));
    }
}

/// Check one directory. Report it if it is a project with a `target/`.
/// Otherwise queue subdirectories for scanning.
fn find_projects_task(job: Job, results: &Sender<PathBuf>) {
    let read_dir = match job.path.read_dir() {
        Ok(it) => it,
        Err(_) => return,
    };

    // Only `DirEntry`s that read cleanly reach the partition. Unreadable entries
    // are skipped, matching `cargo-clean-all`.
    let (dirs, files): (Vec<_>, Vec<_>) = read_dir
        .filter_map(|it| it.ok())
        .partition(|it| it.file_type().is_ok_and(|t| t.is_dir()));

    let has_cargo_toml = files
        .iter()
        .any(|it| it.file_name().to_string_lossy() == "Cargo.toml");

    let mut has_target = false;
    for dir in &dirs {
        let file_name = dir.file_name().to_string_lossy().into_owned();
        match file_name.as_str() {
            // Never descend here. Same exclusion as `cargo-clean-all`.
            ".git" | ".cargo" => {}
            // Do not recurse into `target/`. Just record it on projects.
            "target" if has_cargo_toml => has_target = true,
            // A bare `target/` without `Cargo.toml` beside it is not a project dir.
            // Still recurse to find nested workspaces.
            _ => job.explore_recursive(dir.path()),
        }
    }

    if has_cargo_toml && has_target {
        let _ = results.send(job.path);
    }
}

/// Recursively measure disk usage and track the newest mtime.
///
/// Size matches `du`: parallel walk with allocated-block sizes, each inode
/// counted once. Missing paths and symlinks count as empty. Unreadable
/// subtrees add nothing.
#[tracing::instrument(skip_all, fields(path = %path.as_ref().display()))]
fn recursive_scan_target<T: AsRef<Path>>(path: T) -> (u64, SystemTime) {
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
    use std::time::{Duration, SystemTime};

    fn setup_tree(name: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!("targeter-test-{name}"));
        let _ = fs::remove_dir_all(&root);
        // Real project with a target dir.
        fs::create_dir_all(root.join("proj-a/target")).unwrap();
        fs::write(root.join("proj-a/Cargo.toml"), "[package]\n").unwrap();
        fs::write(root.join("proj-a/target/blob.bin"), "hello").unwrap();
        // Cargo.toml nested under .git must not be reported.
        fs::create_dir_all(root.join("proj-a/.git")).unwrap();
        fs::write(root.join("proj-a/.git/Cargo.toml"), "[package]\n").unwrap();
        // Bare target/ without Cargo.toml is not a project.
        fs::create_dir_all(root.join("loose-target/target")).unwrap();
        // Plain dir with no Cargo.toml at all.
        fs::create_dir_all(root.join("plain")).unwrap();
        root
    }

    #[test]
    fn finds_only_projects_with_cargo_toml_and_target() {
        let root = setup_tree("find");
        let mut entries = scan(&root);
        entries.sort_by(|a, b| a.project_path.cmp(&b.project_path));

        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].project_path, root.join("proj-a"));
        // Disk usage, not apparent length: blocks for the file plus its dirs.
        assert!(entries[0].size >= 5);
        assert!(entries[0].last_modified > SystemTime::UNIX_EPOCH);
        let _ = fs::remove_dir_all(&root);
    }
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

    #[test]
    fn nested_workspace_member_is_found() {
        let root = std::env::temp_dir().join("targeter-test-nested");
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("workspace/member/target")).unwrap();
        fs::write(root.join("workspace/Cargo.toml"), "[workspace]\n").unwrap();
        fs::write(root.join("workspace/member/Cargo.toml"), "[package]\n").unwrap();
        fs::write(root.join("workspace/member/target/blob.bin"), "xy").unwrap();

        let entries = scan(&root);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].project_path, root.join("workspace/member"));
        // Sanity: fixture mtime is recent (within the last hour).
        assert!(
            SystemTime::now()
                .duration_since(entries[0].last_modified)
                .unwrap_or(Duration::MAX)
                < Duration::from_secs(3600)
        );
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn build_cache_entry_measures_present_dir() {
        let root = std::env::temp_dir().join("targeter-test-build-cache");
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("build-cache/content")).unwrap();
        fs::write(root.join("build-cache/content/blob.bin"), "12345678").unwrap();

        let entry = build_cache_entry_at(&root.join("build-cache")).expect("cache dir exists");
        assert_eq!(entry.project_path, root.join("build-cache"));
        assert!(entry.size >= 8);
        assert!(entry.last_modified > SystemTime::UNIX_EPOCH);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn build_cache_entry_missing_dir_is_none() {
        let root = std::env::temp_dir().join("targeter-test-build-cache-missing");
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        assert!(build_cache_entry_at(&root.join("build-cache")).is_none());
        // A file at the cache path is not a cache either.
        fs::write(root.join("build-cache"), "not a dir").unwrap();
        assert!(build_cache_entry_at(&root.join("build-cache")).is_none());
        let _ = fs::remove_dir_all(&root);
    }
}
