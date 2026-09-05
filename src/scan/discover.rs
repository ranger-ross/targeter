use std::{
    path::{Path, PathBuf},
    sync::Mutex,
};

use ignore::{DirEntry, WalkBuilder, WalkState};
use rayon::prelude::*;

use super::{TargetEntry, measure::recursive_scan_target};

/// Scan `root` recursively for cargo projects with a `target/` dir.
///
/// A directory is a project when it holds both `Cargo.toml` and a real
/// `target/` subdirectory. `.git` and `.cargo` are never descended into, and
/// neither is a project's own `target/`. Everything else honors ignore
/// files (`.ignore`, `.gitignore`, ...), so ignored subtrees are pruned
/// without a single `stat`.
///
/// `target/` itself stays visible through direct filesystem probes: it is
/// usually gitignored, but a project is still reported and measured when its
/// `target/` exists on disk.
#[tracing::instrument(skip_all, fields(root = %root.display()))]
pub fn scan(root: &Path) -> Vec<TargetEntry> {
    let projects = Mutex::new(Vec::new());
    WalkBuilder::new(root)
        // Hidden dirs may hold projects; `.git` and `.cargo` are pruned below.
        .hidden(false)
        // Apply gitignores even outside a git checkout.
        .require_git(false)
        .threads(num_cpus::get().max(1))
        .filter_entry(keep_entry)
        .build_parallel()
        .run(|| {
            let projects = &projects;
            Box::new(move |result: Result<DirEntry, ignore::Error>| visit_entry(result, projects))
        });
    let projects = projects.into_inner().unwrap_or_default();
    tracing::info!(count = projects.len(), "discovery complete");

    // Each target measures on the shared rayon pool. Nested inside pdu's own
    // parallel walk this just feeds one work queue, so the 1621ms outlier
    // can't stall the other 100.
    let mut entries: Vec<TargetEntry> = projects
        .par_iter()
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

/// Whether the walker yields an entry and descends into it.
/// Rejecting a directory prunes its whole subtree.
fn keep_entry(entry: &DirEntry) -> bool {
    // Never prune the scan root itself.
    if entry.depth() == 0 {
        return true;
    }
    match entry.file_name().to_str() {
        Some(".git") | Some(".cargo") => false,
        // A bare `target/` without a sibling `Cargo.toml` may hide nested
        // workspaces, so only a project's own `target/` is pruned.
        Some("target") => !is_project_target(entry.path()),
        _ => true,
    }
}

/// Whether `dir` is a `target/` with a sibling `Cargo.toml`.
fn is_project_target(dir: &Path) -> bool {
    dir.parent()
        .map(|parent| parent.join("Cargo.toml").is_file())
        .unwrap_or(false)
}

/// Record `entry` when it is a project dir. Never blocks the walk.
fn visit_entry(
    result: Result<DirEntry, ignore::Error>,
    projects: &Mutex<Vec<PathBuf>>,
) -> WalkState {
    let Ok(entry) = result else {
        return WalkState::Continue;
    };
    // Symlinked dirs are never projects, and the walker never descends into them.
    if entry.path_is_symlink() || !entry.file_type().is_some_and(|kind| kind.is_dir()) {
        return WalkState::Continue;
    }
    let dir = entry.path();
    if dir.join("Cargo.toml").is_file()
        && is_target_dir(&dir.join("target"))
        && let Ok(mut projects) = projects.lock()
    {
        projects.push(dir.to_path_buf());
    }
    WalkState::Continue
}

/// Whether `path` is a real `target/` dir. Probed directly so gitignored
/// build output still measures. Symlinks never count.
fn is_target_dir(path: &Path) -> bool {
    std::fs::symlink_metadata(path).is_ok_and(|md| md.is_dir())
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
        assert!(entries[0].last_modified.is_some_and(|t| t > SystemTime::UNIX_EPOCH));
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
        let mtime = entries[0].last_modified.expect("target exists");
        assert!(
            SystemTime::now().duration_since(mtime).unwrap_or(Duration::MAX)
                < Duration::from_secs(3600)
        );
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn gitignored_dirs_are_skipped_but_gitignored_targets_still_measure() {
        let root = std::env::temp_dir().join("targeter-test-gitignore");
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        fs::write(root.join(".gitignore"), "ignored/\ntarget/\n").unwrap();
        // Real project whose `target/` matches the ignore file.
        fs::create_dir_all(root.join("proj-b/target")).unwrap();
        fs::write(root.join("proj-b/Cargo.toml"), "[package]\n").unwrap();
        fs::write(root.join("proj-b/target/blob.bin"), "hello").unwrap();
        // Fake project under a dir the ignore file prunes. Never even walked.
        fs::create_dir_all(root.join("ignored/ghost/target")).unwrap();
        fs::write(root.join("ignored/ghost/Cargo.toml"), "[package]\n").unwrap();
        fs::write(root.join("ignored/ghost/target/blob.bin"), "hello").unwrap();

        let entries = scan(&root);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].project_path, root.join("proj-b"));
        assert!(entries[0].size >= 5);
        let _ = fs::remove_dir_all(&root);
    }
}
