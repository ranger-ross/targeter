use std::{
    collections::HashSet,
    path::{Path, PathBuf},
    time::{Duration, SystemTime},
};

use ignore::{DirEntry, WalkBuilder, WalkState};

#[cfg(unix)]
use std::os::unix::fs::MetadataExt;

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

/// Recursively measure disk usage and track the newest mtime in one parallel walk.
///
/// Size matches `du`: allocated-block sizes, each inode counted once. Missing
/// paths count as empty. Unreadable subtrees add nothing. Symlinks contribute
/// their own inode blocks but are never followed, so linked trees cannot
/// loop or double-count.
#[tracing::instrument(skip_all, fields(path = %path.as_ref().display()))]
pub(super) fn recursive_scan_target<T: AsRef<Path>>(path: T) -> (u64, SystemTime) {
    let path = path.as_ref();
    if !path.exists() || path.is_symlink() {
        return (0, SystemTime::UNIX_EPOCH);
    }
    // One traversal feeds size, mtime, and inode identity. Per-entry records
    // stream over a channel; the serial fold dedups hardlinks like `du`.
    let (tx, rx) = crossbeam_channel::unbounded();
    WalkBuilder::new(path)
        .hidden(false)
        .require_git(false)
        .standard_filters(false)
        .threads(num_cpus::get().max(1))
        .build_parallel()
        .run(|| {
            let tx = tx.clone();
            Box::new(move |result: Result<DirEntry, ignore::Error>| {
                if let Ok(entry) = result
                    && let Some(rec) = record_entry(&entry)
                {
                    let _ = tx.send(rec);
                }
                WalkState::Continue
            })
        });
    drop(tx);
    fold_records(rx)
}

/// One entry's contribution. `mtime_ns` saturates pre-epoch times to zero,
/// matching the old `modified().unwrap_or(EPOCH)`.
struct EntryRec {
    #[cfg(unix)]
    dev: u64,
    #[cfg(unix)]
    ino: u64,
    size: u64,
    mtime_ns: u64,
}

fn record_entry(entry: &DirEntry) -> Option<EntryRec> {
    // lstat: a symlink records its own inode but is never followed.
    let md = std::fs::symlink_metadata(entry.path()).ok()?;
    Some(EntryRec {
        #[cfg(unix)]
        dev: md.dev(),
        #[cfg(unix)]
        ino: md.ino(),
        #[cfg(unix)]
        size: md.blocks() * 512,
        #[cfg(not(unix))]
        size: md.len(),
        mtime_ns: md
            .modified()
            .ok()
            .and_then(|st| st.duration_since(SystemTime::UNIX_EPOCH).ok())
            .map(|d| d.as_nanos() as u64)
            .unwrap_or(0),
    })
}

fn fold_records(rx: crossbeam_channel::Receiver<EntryRec>) -> (u64, SystemTime) {
    #[cfg(unix)]
    let mut seen = HashSet::new();
    let mut total = 0u64;
    let mut newest_ns = 0u64;
    for rec in rx {
        #[cfg(unix)]
        if !seen.insert((rec.dev, rec.ino)) {
            continue;
        }
        total += rec.size;
        newest_ns = newest_ns.max(rec.mtime_ns);
    }
    (
        total,
        SystemTime::UNIX_EPOCH + Duration::from_nanos(newest_ns),
    )
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
    #[cfg(unix)]
    #[test]
    fn symlinks_and_ignores_do_not_skew_measure() {
        let root = std::env::temp_dir().join("targeter-test-measure-parity");
        let _ = fs::remove_dir_all(&root);
        let target = root.join("target");
        fs::create_dir_all(target.join("sub")).unwrap();
        fs::write(target.join("sub/a.bin"), "12345678").unwrap();
        let (baseline, _) = recursive_scan_target(&target);
        assert!(baseline > 0);
        // Valid link, dangling link, and linked dir: never followed, but a
        // link inode counts its own blocks like `du` (long targets spill
        // out of the inode, so this is `>=`, not `==`).
        std::os::unix::fs::symlink(target.join("sub/a.bin"), target.join("link.bin")).unwrap();
        std::os::unix::fs::symlink(root.join("nope"), target.join("dangling.bin")).unwrap();
        std::os::unix::fs::symlink(target.join("sub"), target.join("linkdir")).unwrap();
        let (with_links, _) = recursive_scan_target(&target);
        assert!(with_links >= baseline);

        // Ignore files never prune: gitignored outputs still count.
        fs::write(target.join(".gitignore"), "*.bin\n").unwrap();
        let (with_ignore, _) = recursive_scan_target(&target);
        assert!(with_ignore >= baseline);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn newest_mtime_tracks_deep_writes() {
        let root = std::env::temp_dir().join("targeter-test-measure-mtime");
        let _ = fs::remove_dir_all(&root);
        let deep = root.join("target/a/b");
        fs::create_dir_all(&deep).unwrap();
        fs::write(deep.join("a.bin"), "1234").unwrap();
        fs::write(deep.join("b.bin"), "12345678").unwrap();
        // Exact inode mtime, not wall clock: fs timestamps can skew
        // sub-ms against `SystemTime::now` on tmpfs.
        let expected = fs::symlink_metadata(deep.join("b.bin"))
            .unwrap()
            .modified()
            .unwrap();
        let (_, mtime) = recursive_scan_target(root.join("target"));
        assert_eq!(mtime, expected);
        let _ = fs::remove_dir_all(&root);
    }
}
