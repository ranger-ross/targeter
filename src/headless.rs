use std::{
    io::{self, Write},
    path::Path,
    time::{Duration, SystemTime},
};

use eyre::Result;
use rayon::prelude::*;

use crate::{
    app::App,
    scan::{self, TargetEntry},
    ui::{display_path, format_modified_entry, format_size, format_size_opt, output_suffix},
};

/// Blocking scan that reuses the TUI state for sorting and totals.
fn collect(root: &Path) -> App {
    let mut app = App::new(root.to_path_buf());
    app.set_discovered(scan::discover(root));
    let measurements: Vec<_> = app
        .entries
        .par_iter()
        .map(|e| scan::measure_target(&e.target_dir))
        .collect();
    app.apply_measurements(&measurements);
    app.finish_scan(scan::build_cache_entry());
    app
}

fn project_label(entry: &TargetEntry, entries: &[TargetEntry]) -> String {
    let suffix = output_suffix(entry, entries)
        .map(|(label, _)| label)
        .unwrap_or("");
    format!("{}{}", entry.project_name(), suffix)
}

/// Print the TUI table rows as aligned text.
pub fn run_list(root: &Path) -> Result<()> {
    let app = collect(root);
    let mut out = io::stdout().lock();
    if app.entries.is_empty() {
        writeln!(out, "No target/ directories found.")?;
        return Ok(());
    }
    let visible = app.visible_indices();
    let shown: Vec<&TargetEntry> = visible.iter().filter_map(|&i| app.entries.get(i)).collect();
    render_table(&mut out, &shown, &app.entries)?;
    let total: u64 = shown.iter().filter_map(|e| e.size).sum();
    writeln!(
        out,
        "\n{} project{} · {} total",
        shown.len(),
        if shown.len() == 1 { "" } else { "s" },
        format_size(total)
    )?;
    Ok(())
}

/// Aligned PROJECT/SIZE/MODIFIED/PATH table shared by `list` and the
/// `clean` confirmation report.
fn render_table(out: &mut impl Write, shown: &[&TargetEntry], all: &[TargetEntry]) -> Result<()> {
    let rows: Vec<(String, String, String, String)> = shown
        .iter()
        .map(|e| {
            (
                project_label(e, all),
                format_size_opt(e.size),
                format_modified_entry(e),
                display_path(&e.target_dir),
            )
        })
        .collect();
    let name_w = rows
        .iter()
        .map(|r| r.0.len())
        .max()
        .unwrap_or(7)
        .max("PROJECT".len());
    let size_w = rows
        .iter()
        .map(|r| r.1.len())
        .max()
        .unwrap_or(4)
        .max("SIZE".len());
    let mod_w = rows
        .iter()
        .map(|r| r.2.len())
        .max()
        .unwrap_or(8)
        .max("MODIFIED".len());
    writeln!(
        out,
        "{:<name_w$} {:>size_w$} {:<mod_w$} PATH",
        "PROJECT", "SIZE", "MODIFIED"
    )?;
    for (name, size, modified, path) in &rows {
        writeln!(
            out,
            "{name:<name_w$} {size:>size_w$} {modified:<mod_w$} {path}"
        )?;
    }
    Ok(())
}

/// Delete entries older than `older_than` and larger than `larger_than`.
pub fn run_clean(root: &Path, older_than: &str, larger_than: &str, yes: bool) -> Result<()> {
    let min_age = parse_age(older_than)?;
    let min_size = parse_size(larger_than)?;
    let now = SystemTime::now();
    let app = collect(root);
    let candidates: Vec<&TargetEntry> = app
        .entries
        .iter()
        .filter(|e| is_candidate(e, now, min_age, min_size))
        .collect();
    if candidates.is_empty() {
        println!(
            "Nothing to clean: no target/ dirs older than {older_than} and larger than {larger_than}."
        );
        return Ok(());
    }
    let reclaim: u64 = candidates.iter().filter_map(|e| e.size).sum();
    let mut out = io::stdout().lock();
    writeln!(
        out,
        "Candidates older than {older_than} and larger than {larger_than}:"
    )?;
    render_table(&mut out, &candidates, &app.entries)?;
    writeln!(
        out,
        "\n{} director{} · {} reclaimable",
        candidates.len(),
        if candidates.len() == 1 { "y" } else { "ies" },
        format_size(reclaim)
    )?;
    if !yes && !confirm(&format!("Delete {} directories?", candidates.len()))? {
        println!("Aborted.");
        return Ok(());
    }
    let mut deleted = 0;
    let mut freed = 0u64;
    let mut failures = Vec::new();
    for e in &candidates {
        match std::fs::remove_dir_all(&e.target_dir) {
            Ok(()) => {
                deleted += 1;
                freed += e.size.unwrap_or(0);
            }
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                deleted += 1;
                freed += e.size.unwrap_or(0);
            }
            Err(err) => failures.push(format!("{}: {err}", e.target_dir.display())),
        }
    }
    println!(
        "Deleted {deleted} directories, freed {}.",
        format_size(freed)
    );
    if !failures.is_empty() {
        for f in &failures {
            eprintln!("failed to delete {f}");
        }
        eyre::bail!("failed to delete {} directories", failures.len());
    }
    Ok(())
}

/// A measured entry matches when strictly older and strictly larger.
fn is_candidate(entry: &TargetEntry, now: SystemTime, min_age: Duration, min_size: u64) -> bool {
    match (entry.size, entry.last_modified) {
        (Some(size), Some(modified)) => {
            size > min_size && now.duration_since(modified).unwrap_or_default() > min_age
        }
        _ => false,
    }
}

fn confirm(prompt: &str) -> Result<bool> {
    print!("{prompt} [y/N] ");
    io::stdout().flush()?;
    let mut line = String::new();
    io::stdin().read_line(&mut line)?;
    Ok(matches!(line.trim().to_lowercase().as_str(), "y" | "yes"))
}

/// Parse sizes like `100MB`, `1GiB`, `512` (bytes). Binary units.
pub fn parse_size(raw: &str) -> Result<u64> {
    let s = raw.trim().to_uppercase().replace(' ', "");
    if s.is_empty() {
        eyre::bail!("empty size");
    }
    let split = s.find(|c: char| c.is_alphabetic()).unwrap_or(s.len());
    let (num, unit) = s.split_at(split);
    let num: f64 = num
        .parse()
        .map_err(|_| eyre::eyre!("invalid size: {raw}"))?;
    if num < 0.0 {
        eyre::bail!("invalid size: {raw}");
    }
    let mult: u64 = match unit {
        "" | "B" => 1,
        "K" | "KB" | "KIB" => 1024,
        "M" | "MB" | "MIB" => 1024 * 1024,
        "G" | "GB" | "GIB" => 1024 * 1024 * 1024,
        "T" | "TB" | "TIB" => 1024 * 1024 * 1024 * 1024,
        _ => eyre::bail!("invalid size unit in {raw:?}: use B, KB, MB, GB, or TB"),
    };
    Ok((num * mult as f64).round() as u64)
}

/// Parse ages like `30d`, `12h`, `90m`. A bare number means days.
pub fn parse_age(raw: &str) -> Result<Duration> {
    let s = raw.trim().to_lowercase().replace(' ', "");
    if s.is_empty() {
        eyre::bail!("empty age");
    }
    let split = s.find(|c: char| c.is_alphabetic()).unwrap_or(s.len());
    let (num, unit) = s.split_at(split);
    let num: f64 = num.parse().map_err(|_| eyre::eyre!("invalid age: {raw}"))?;
    if num < 0.0 {
        eyre::bail!("invalid age: {raw}");
    }
    let secs_per: f64 = match unit {
        "" | "d" | "day" | "days" => 86_400.0,
        "s" | "sec" | "secs" | "second" | "seconds" => 1.0,
        "m" | "min" | "mins" | "minute" | "minutes" => 60.0,
        "h" | "hour" | "hours" => 3600.0,
        "w" | "week" | "weeks" => 7.0 * 86_400.0,
        _ => eyre::bail!("invalid age unit in {raw:?}: use s, m, h, d, or w"),
    };
    Ok(Duration::from_secs((num * secs_per).round() as u64))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sizes_use_binary_units() {
        assert_eq!(parse_size("512").unwrap(), 512);
        assert_eq!(parse_size("100MB").unwrap(), 100 * 1024 * 1024);
        assert_eq!(parse_size("1gib").unwrap(), 1024 * 1024 * 1024);
        assert_eq!(parse_size("2K").unwrap(), 2048);
        assert_eq!(
            parse_size("1.5GB").unwrap(),
            (1.5 * 1024.0 * 1024.0 * 1024.0) as u64
        );
    }

    #[test]
    fn bad_sizes_fail() {
        assert!(parse_size("").is_err());
        assert!(parse_size("10XB").is_err());
        assert!(parse_size("-5MB").is_err());
    }

    #[test]
    fn ages_parse_with_day_default() {
        assert_eq!(parse_age("30d").unwrap(), Duration::from_secs(30 * 86_400));
        assert_eq!(parse_age("30").unwrap(), Duration::from_secs(30 * 86_400));
        assert_eq!(parse_age("12h").unwrap(), Duration::from_secs(12 * 3600));
        assert_eq!(parse_age("1w").unwrap(), Duration::from_secs(7 * 86_400));
    }

    #[test]
    fn bad_ages_fail() {
        assert!(parse_age("").is_err());
        assert!(parse_age("10y").is_err());
    }

    fn entry(size: Option<u64>, age: Option<Duration>) -> TargetEntry {
        TargetEntry {
            project_path: "proj".into(),
            target_dir: "proj/target".into(),
            kind: crate::scan::OutputKind::Target,
            size,
            last_modified: age.map(|a| SystemTime::now() - a),
        }
    }

    #[test]
    fn candidates_need_both_age_and_size() {
        let now = SystemTime::now();
        let min_age = Duration::from_secs(30 * 86_400);
        let min_size = 100 * 1024 * 1024;
        let old_big = entry(Some(min_size + 1), Some(min_age + Duration::from_secs(1)));
        assert!(is_candidate(&old_big, now, min_age, min_size));
        // Boundary values do not match: strictly older and strictly larger.
        let edge = entry(Some(min_size), Some(min_age));
        assert!(!is_candidate(&edge, now, min_age, min_size));
        let fresh = entry(Some(min_size + 1), Some(Duration::from_secs(1)));
        assert!(!is_candidate(&fresh, now, min_age, min_size));
        let small = entry(Some(1), Some(min_age + Duration::from_secs(1)));
        assert!(!is_candidate(&small, now, min_age, min_size));
        assert!(!is_candidate(&entry(None, None), now, min_age, min_size));
    }
}
