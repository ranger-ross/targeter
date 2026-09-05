use std::{
    path::{Path, PathBuf},
    thread,
};

use crossbeam_channel::Sender;

use super::{TargetEntry, measure::recursive_scan_target};

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
}
