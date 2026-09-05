use bytefmt::{Unit, format_to};
use ratatui::{
    Frame,
    layout::{Constraint, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Cell, Paragraph, Row, Table},
};

use crate::app::App;

pub mod input;
mod loading;
mod theme;

#[tracing::instrument(skip_all)]
pub fn render(frame: &mut Frame, app: &mut App) {
    let area = frame.area();
    let [top_area, table_area, footer_area] = Layout::vertical([
        Constraint::Length(3),
        Constraint::Min(0),
        Constraint::Length(3),
    ])
    .areas(area);
    // The cache card earns half the top row once a measurement exists.
    let show_cache = app.build_cache.is_some();
    let (summary_area, cache_area) = if show_cache {
        let [left, right] =
            Layout::horizontal([Constraint::Percentage(50), Constraint::Percentage(50)])
                .areas(top_area);
        (left, Some(right))
    } else {
        (top_area, None)
    };

    let root = display_path(&app.root);
    let visible = app.visible_indices();
    let visible_size: u64 = visible
        .iter()
        .filter_map(|&i| app.entries.get(i))
        .filter_map(|e| e.size)
        .sum();
    let counts = if app.filter_regex.is_some() {
        format!(
            "{} of {} projects · {} shown",
            visible.len(),
            app.entries.len(),
            format_size(visible_size)
        )
    } else {
        format!(
            "{} project{} · {} total",
            app.entries.len(),
            if app.entries.len() == 1 { "" } else { "s" },
            format_size(app.total_size)
        )
    };
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(
                root,
                Style::default()
                    .fg(Color::Green)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw(format!(" · {counts}")),
        ]))
        .block(crate::ui::theme::card("Summary")),
        summary_area,
    );
    if let Some(cache_area) = cache_area {
        render_build_cache(frame, cache_area, app);
    }

    if app.scanning && app.entries.is_empty() {
        frame.render_widget(
            Paragraph::new(
                crate::ui::loading::Loading::new(
                    "Scanning for target/ directories…",
                    app.loading_start.elapsed(),
                )
                .line(),
            )
            .block(crate::ui::theme::card_plain()),
            table_area,
        );
    } else if app.entries.is_empty() {
        frame.render_widget(
            Paragraph::new("No target/ directories found.").block(crate::ui::theme::card_plain()),
            table_area,
        );
    } else if visible.is_empty() {
        frame.render_widget(
            Paragraph::new(format!("No match for /{}.", app.filter_text))
                .block(crate::ui::theme::card_plain()),
            table_area,
        );
    } else {
        let header = Row::new(["Project", "Size", "Modified", "Path"])
            .height(1)
            .style(Style::default().fg(crate::ui::theme::MUTED));

        let rows = visible.iter().filter_map(|&i| app.entries.get(i)).map(|e| {
            Row::new([
                Cell::from(e.project_name()),
                Cell::from(format_size_opt(e.size)),
                Cell::from(format_modified_entry(e)),
                Cell::from(display_path(&e.project_path)),
            ])
            .height(1)
        });
        let widths = [
            Constraint::Max(24),
            Constraint::Length(12),
            Constraint::Length(17),
            Constraint::Min(20),
        ];
        let table = Table::new(rows, widths)
            .header(header)
            .block(crate::ui::theme::card_plain())
            .row_highlight_style(crate::ui::theme::selected());
        frame.render_stateful_widget(table, table_area, &mut app.table_state);
    }

    render_footer(frame, footer_area, app);
}

/// Binary-unit sizes matching `du -h`. Pending entries read as `-`.
fn format_size_opt(size: Option<u64>) -> String {
    size.map_or_else(|| "-".to_string(), format_size)
}

/// Binary-unit sizes matching `du -h`.
fn format_size(bytes: u64) -> String {
    const KIB: u64 = 1024;
    const MIB: u64 = 1024 * KIB;
    const GIB: u64 = 1024 * MIB;
    const TIB: u64 = 1024 * GIB;
    if bytes < KIB {
        format_to(bytes, Unit::B)
    } else if bytes < MIB {
        format_to(bytes, Unit::KIB)
    } else if bytes < GIB {
        format_to(bytes, Unit::MIB)
    } else if bytes < TIB {
        format_to(bytes, Unit::GIB)
    } else {
        format_to(bytes, Unit::TIB)
    }
}

/// Absolute path for display, with `$HOME` contracted to `~`.
fn display_path(path: &std::path::Path) -> String {
    let abs = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    if let Ok(home) = std::env::var("HOME")
        && !home.is_empty()
    {
        let home = std::path::PathBuf::from(home);
        // Match the canonicalized home, so a symlinked `$HOME` (macOS
        // `/home` to `/private/home`) still contracts.
        let canon_home = std::fs::canonicalize(&home).unwrap_or(home);
        if let Ok(rel) = abs.strip_prefix(&canon_home) {
            if rel.as_os_str().is_empty() {
                return "~".to_string();
            }
            return format!("~/{}", rel.display());
        }
    }
    abs.display().to_string()
}

fn format_modified(last_modified: std::time::SystemTime) -> String {
    format_modified_at(last_modified, std::time::SystemTime::now())
}

/// Injected `now` frees boundary tests from the clock.
fn format_modified_at(last_modified: std::time::SystemTime, now: std::time::SystemTime) -> String {
    format_age(now.duration_since(last_modified).unwrap_or_default())
}

/// Render a fixed age so tests stay deterministic.
fn format_age(age: std::time::Duration) -> String {
    if age.as_secs() < 5 {
        return "now".to_string();
    }
    if (86_400..172_800).contains(&age.as_secs()) {
        return "yesterday".to_string();
    }
    timeago::Formatter::new().convert(age)
}

/// Timestamp for display; pending entries read as `-`, deleted as "deleted".
fn format_modified_entry(entry: &crate::scan::TargetEntry) -> String {
    if entry.size.is_none() {
        return "-".to_string();
    }
    format_modified_opt(entry.last_modified)
}

/// Timestamp for display; deleted dirs read as "deleted".
fn format_modified_opt(last_modified: Option<std::time::SystemTime>) -> String {
    match last_modified {
        Some(t) => format_modified(t),
        None => "deleted".to_string(),
    }
}

fn render_build_cache(frame: &mut Frame, area: Rect, app: &App) {
    let line = match &app.build_cache {
        Some(entry) => format!(
            "{} ({})  {}",
            format_size_opt(entry.size),
            format_modified_entry(entry),
            display_path(&entry.project_path)
        ),
        None if app.scanning => "Measuring…".to_string(),
        None => match crate::scan::build_cache_path() {
            Some(path) => format!("not present: {}", display_path(&path)),
            None => "no cargo home found".to_string(),
        },
    };
    frame.render_widget(
        Paragraph::new(line).block(crate::ui::theme::card("Build Cache")),
        area,
    );
}

fn render_footer(frame: &mut Frame, area: Rect, app: &App) {
    if app.filtering {
        let line = match &app.filter_error {
            Some(err) => format!("/{}/ ! {}", app.filter_text, err),
            None => format!("/{}/", app.filter_text),
        };
        frame.render_widget(
            Paragraph::new(Line::from(vec![
                Span::raw(line),
                Span::raw(" · enter done · esc done · ^U clear"),
            ]))
            .block(crate::ui::theme::card("filter (regex)")),
            area,
        );
        return;
    }
    let status = if app.scanning {
        "scanning…".to_string()
    } else {
        format!("sort: {} (s)", app.sort.label())
    };
    let filter = if app.filter_text.is_empty() {
        String::new()
    } else {
        format!(" · filter: /{}/", app.filter_text)
    };
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::raw("↑/↓ navigate · g/G top/bottom · s "),
            Span::raw(status),
            Span::raw(" · / filter · r rescan · q quit"),
            Span::raw(filter),
        ]))
        .block(crate::ui::theme::card_plain()),
        area,
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn relative_ages_use_exact_vocab() {
        assert_eq!(format_age(Duration::ZERO), "now");
        assert_eq!(format_age(Duration::from_secs(4)), "now");
        assert_eq!(format_age(Duration::from_secs(5)), "5 seconds ago");
        assert_eq!(format_age(Duration::from_secs(30)), "30 seconds ago");
        assert_eq!(format_age(Duration::from_secs(60)), "1 minute ago");
        assert_eq!(format_age(Duration::from_secs(3600)), "1 hour ago");
        assert_eq!(format_age(Duration::from_secs(23 * 3600)), "23 hours ago");
        assert_eq!(format_age(Duration::from_secs(36 * 3600)), "yesterday");
        assert_eq!(format_age(Duration::from_secs(3 * 86_400)), "3 days ago");
    }

    #[test]
    fn yesterday_window_edges() {
        assert_eq!(format_age(Duration::from_secs(86_399)), "23 hours ago");
        assert_eq!(format_age(Duration::from_secs(86_400)), "yesterday");
        assert_eq!(format_age(Duration::from_secs(172_799)), "yesterday");
        assert_eq!(format_age(Duration::from_secs(172_800)), "2 days ago");
    }

    #[test]
    fn future_timestamp_reads_as_now() {
        let now = std::time::SystemTime::now();
        // Clock skew or just-written files read as now, never panic.
        assert_eq!(
            format_modified_at(now + Duration::from_secs(60), now),
            "now"
        );
    }

    #[test]
    fn deleted_dir_reads_as_deleted() {
        assert_eq!(format_modified_opt(None), "deleted");
        assert_eq!(
            format_modified_opt(Some(std::time::SystemTime::now())),
            "now"
        );
    }

    #[test]
    fn pending_entries_read_as_dash() {
        use crate::scan::TargetEntry;
        assert_eq!(format_size_opt(None), "-");
        assert_eq!(format_size_opt(Some(0)), "0 B");
        let pending = TargetEntry {
            project_path: std::path::PathBuf::from("proj-a"),
            size: None,
            last_modified: None,
        };
        assert_eq!(format_modified_entry(&pending), "-");
    }
}
