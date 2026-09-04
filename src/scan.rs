use std::{
    path::{Path, PathBuf},
    thread,
    time::SystemTime,
};

use crossbeam_channel::Sender;

/// A Rust project with a `target/` directory on disk.
#[derive(Clone, Debug)]
pub struct TargetEntry {
    /// Project root (parent of `target/`).
    pub project_path: PathBuf,
    /// Recursive size of `target/` in bytes.
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

/// The path scanned for the unstable cargo build cache (`$CARGO_HOME/build-cache`).
///
/// `CARGO_HOME` wins when set; otherwise `$HOME/.cargo` is used.
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
/// Missing directory means the cache is disabled or unused, so there is
/// nothing to report rather than a zero-size entry.
pub fn build_cache_entry() -> Option<TargetEntry> {
    build_cache_path().and_then(|path| build_cache_entry_at(&path))
}

/// Measure the build cache at an explicit path. Split out for tests so they
/// do not depend on the real `$CARGO_HOME`.
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

/// Re-measure one known directory. Same math as the full scan, one path.
pub fn measure_target(target_dir: &Path) -> Measurement {
    let (size, last_modified) = recursive_scan_target(target_dir);
    Measurement {
        target_dir: target_dir.to_path_buf(),
        size,
        last_modified,
    }
}

/// Scan `root` recursively for cargo projects with a `target/` dir.
///
/// Mirrors `cargo-clean-all` detection logic:
/// - a directory containing `Cargo.toml` is a project
/// - if that directory also contains a `target/` subdirectory it is reported
/// - `.git` and `.cargo` are never descended into
/// - `target/` itself is never descended into for further project detection
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

/// Check one directory: report it if it is a project with a `target/`,
/// otherwise queue subdirectories for scanning.
fn find_projects_task(job: Job, results: &Sender<PathBuf>) {
    let read_dir = match job.path.read_dir() {
        Ok(it) => it,
        Err(_) => return,
    };

    // Partition is over `DirEntry`s that read cleanly; unreadable entries are skipped,
    // matching `cargo-clean-all` (which hides access errors unless verbose).
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
            // Never descend here; same exclusion as `cargo-clean-all` (see its issue #2).
            ".git" | ".cargo" => {}
            // Don't recurse into `target/`; just record its presence on projects.
            "target" if has_cargo_toml => has_target = true,
            // A bare `target/` without `Cargo.toml` beside it is not a project
            // target dir; still recurse (harmless, finds nested workspaces).
            _ => job.explore_recursive(dir.path()),
        }
    }

    if has_cargo_toml && has_target {
        let _ = results.send(job.path);
    }
}

/// Recursively sum file sizes and track the newest mtime.
///
/// Same semantics as `cargo-clean-all`: missing paths and symlinks count as
/// empty/epoch; unreadable subtrees contribute nothing.
fn recursive_scan_target<T: AsRef<Path>>(path: T) -> (u64, SystemTime) {
    let path = path.as_ref();
    let default = (0, SystemTime::UNIX_EPOCH);

    if !path.exists() || path.is_symlink() {
        return default;
    }

    match (path.is_file(), path.metadata()) {
        (true, Ok(md)) => (md.len(), md.modified().unwrap_or(default.1)),
        _ => path
            .read_dir()
            .map(|rd| {
                rd.filter_map(|it| it.ok().map(|it| it.path()))
                    .map(recursive_scan_target)
                    .fold(default, |(size, newest), (s, m)| (size + s, newest.max(m)))
            })
            .unwrap_or(default),
    }
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
        assert_eq!(entries[0].size, 5);
        assert!(entries[0].last_modified > SystemTime::UNIX_EPOCH);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn size_sums_files_and_newest_mtime_wins() {
        let root = std::env::temp_dir().join("targeter-test-size");
        let _ = fs::remove_dir_all(&root);
        let target = root.join("target");
        fs::create_dir_all(&target).unwrap();
        fs::write(target.join("a.bin"), "1234").unwrap();
        fs::write(target.join("b.bin"), "123456").unwrap();

        let (size, mtime) = recursive_scan_target(&target);
        assert_eq!(size, 10);
        assert!(mtime > SystemTime::UNIX_EPOCH);
        // Missing path degrades to empty, mirroring cargo-clean-all.
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
        assert_eq!(entry.size, 8);
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
