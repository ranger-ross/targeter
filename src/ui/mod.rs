use bytefmt::{Unit, format_to};
use ratatui::{
    Frame,
    layout::{Alignment, Constraint, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span, Text},
    widgets::{Cell, HighlightSpacing, Paragraph, Row, Table},
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
        let selected = app.table_state.selected();
        let header = Row::new([
            Cell::from("PROJECT"),
            Cell::from(right_text("SIZE")),
            Cell::from("MODIFIED"),
            Cell::from("PATH"),
        ])
        .height(1)
        .style(
            Style::default()
                .fg(crate::ui::theme::MUTED)
                .add_modifier(Modifier::BOLD),
        );

        let rows = visible.iter().enumerate().filter_map(|(pos, &i)| {
            app.entries.get(i).map(|e| {
                let name = if Some(pos) == selected {
                    Text::styled(
                        e.project_name(),
                        Style::default().add_modifier(Modifier::BOLD),
                    )
                } else {
                    Text::from(e.project_name())
                };
                let mut size_text = Text::styled(format_size_opt(e.size), size_style(e.size));
                size_text.alignment = Some(Alignment::Right);
                let mut row = Row::new([
                    Cell::from(name),
                    Cell::from(size_text),
                    Cell::from(format_modified_entry(e)),
                    Cell::from(Text::styled(
                        display_path(&e.target_dir),
                        Style::default().fg(crate::ui::theme::DIM),
                    )),
                ])
                .height(1);
                // Deleted rows sink visually too.
                if e.size.is_some() && e.last_modified.is_none() {
                    row = row.style(Style::default().fg(crate::ui::theme::DIM));
                }
                row
            })
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
            .column_spacing(2)
            .row_highlight_style(crate::ui::theme::selected())
            .highlight_symbol("▶ ")
            .highlight_spacing(HighlightSpacing::Always);
        frame.render_stateful_widget(table, table_area, &mut app.table_state);
    }
    render_footer(frame, footer_area, app);
}
/// Right-aligned cell text, for the Size column and its header.
fn right_text(text: impl Into<String>) -> Text<'static> {
    let mut t = Text::from(text.into());
    t.alignment = Some(Alignment::Right);
    t
}

/// Heat color for the Size cell. Tiny and pending read dim, large
/// yellow, huge red.
fn size_style(size: Option<u64>) -> Style {
    const SMALL: u64 = 50 * 1024 * 1024;
    const LARGE: u64 = 1024 * 1024 * 1024;
    const HUGE: u64 = 5 * 1024 * 1024 * 1024;
    match size {
        None => Style::default().fg(crate::ui::theme::DIM),
        Some(s) if s < SMALL => Style::default().fg(crate::ui::theme::DIM),
        Some(s) if s < LARGE => Style::default(),
        Some(s) if s < HUGE => Style::default().fg(crate::ui::theme::AMBER),
        _ => Style::default().fg(crate::ui::theme::ROSE),
    }
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
            display_path(&entry.target_dir)
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
    let block = crate::ui::theme::card_plain();
    let inner = block.inner(area);
    frame.render_widget(block, area);
    let (left, right) = if app.filtering {
        footer_filter(app)
    } else {
        footer_actions(app)
    };
    // Right side keeps its width; the hints take the rest.
    let right_width: u16 = right
        .iter()
        .map(|s: &Span| s.content.chars().count() as u16)
        .sum();
    let right_width = right_width.min(inner.width);
    let [left_area, right_area] =
        Layout::horizontal([Constraint::Min(0), Constraint::Length(right_width)]).areas(inner);
    frame.render_widget(Paragraph::new(Line::from(left)), left_area);
    frame.render_widget(
        Paragraph::new(Line::from(right)).alignment(Alignment::Right),
        right_area,
    );
}

/// Bright key plus dim action, with a trailing gap.
fn hint(key: &'static str, action: String) -> Vec<Span<'static>> {
    vec![
        Span::styled(key, Style::default().add_modifier(Modifier::BOLD)),
        Span::raw(" "),
        Span::styled(action, Style::default().fg(crate::ui::theme::MUTED)),
        Span::raw("  "),
    ]
}

fn footer_actions(app: &App) -> (Vec<Span<'static>>, Vec<Span<'static>>) {
    let mut left = Vec::new();
    left.extend(hint("↑↓", "Navigate".to_string()));
    left.extend(hint("g/G", "Top/Bottom".to_string()));
    left.extend(hint("/", "Filter".to_string()));
    left.extend(hint("s", format!("Sort: {}", app.sort.label())));
    left.extend(hint("r", "Rescan".to_string()));
    left.extend(hint("d", "Delete".to_string()));
    left.extend(hint("q", "Quit".to_string()));
    left.pop();
    let mut right = Vec::new();
    if app.scanning {
        right.push(Span::styled(
            "Scanning…",
            Style::default()
                .fg(crate::ui::theme::MINT)
                .add_modifier(Modifier::BOLD),
        ));
    }
    if !app.filter_text.is_empty() {
        if !right.is_empty() {
            right.push(Span::raw("  "));
        }
        right.push(Span::styled(
            "Filter: ",
            Style::default().fg(crate::ui::theme::MUTED),
        ));
        right.push(Span::styled(
            format!("/{}/", app.filter_text),
            Style::default().add_modifier(Modifier::BOLD),
        ));
    }
    if let Some(err) = &app.delete_error {
        if !right.is_empty() {
            right.push(Span::raw("  "));
        }
        right.push(Span::styled(
            format!("Delete failed: {err}"),
            Style::default().fg(crate::ui::theme::ROSE),
        ));
    }
    (left, right)
}

fn footer_filter(app: &App) -> (Vec<Span<'static>>, Vec<Span<'static>>) {
    let mut left = vec![Span::styled(
        format!("/{}/", app.filter_text),
        Style::default().add_modifier(Modifier::BOLD),
    )];
    if let Some(err) = &app.filter_error {
        left.push(Span::raw("  "));
        left.push(Span::styled(
            err.clone(),
            Style::default().fg(crate::ui::theme::ROSE),
        ));
    }
    let mut right = Vec::new();
    right.extend(hint("Enter", "Done".to_string()));
    right.extend(hint("Esc", "Clear".to_string()));
    right.extend(hint("^U", "Clear".to_string()));
    right.pop();
    (left, right)
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
            target_dir: std::path::PathBuf::from("proj-a/target"),
            size: None,
            last_modified: None,
        };
        assert_eq!(format_modified_entry(&pending), "-");
    }

    #[test]
    fn footer_bar_shows_hints_and_sort_without_dup_key() {
        use ratatui::{Terminal, backend::TestBackend};
        let mut app = App::new(std::path::PathBuf::from("."));
        app.finish_scan(None);
        let backend = TestBackend::new(100, 24);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|f| render(f, &mut app)).unwrap();
        let buf = terminal.backend().buffer().clone();
        let mut text = String::new();
        for y in 0..buf.area.height {
            for x in 0..buf.area.width {
                text.push_str(buf[(x, y)].symbol());
            }
            text.push('\n');
        }
        for want in [
            "Navigate",
            "Top/Bottom",
            "Filter",
            "Sort: size",
            "Rescan",
            "Delete",
            "Quit",
        ] {
            assert!(text.contains(want), "missing {want}");
        }
        assert!(!text.contains("(s)"), "duplicated sort key hint");
    }

    #[test]
    fn table_shows_uppercase_header_and_selection_marker() {
        use ratatui::{Terminal, backend::TestBackend};
        let mut app = App::new(std::path::PathBuf::from("."));
        app.set_discovered(vec![
            std::path::PathBuf::from("proj-a"),
            std::path::PathBuf::from("proj-b"),
        ]);
        app.finish_scan(None);
        let backend = TestBackend::new(100, 24);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|f| render(f, &mut app)).unwrap();
        let buf = terminal.backend().buffer().clone();
        let mut text = String::new();
        for y in 0..buf.area.height {
            for x in 0..buf.area.width {
                text.push_str(buf[(x, y)].symbol());
            }
            text.push('\n');
        }
        for want in ["PROJECT", "SIZE", "MODIFIED", "PATH", "▶"] {
            assert!(text.contains(want), "missing {want}");
        }
    }

    #[test]
    fn size_cells_heat_with_magnitude() {
        const MIB: u64 = 1024 * 1024;
        const GIB: u64 = 1024 * MIB;
        let fg = |s: Option<u64>| size_style(s).fg;
        assert_eq!(fg(None), Some(crate::ui::theme::DIM));
        assert_eq!(fg(Some(0)), Some(crate::ui::theme::DIM));
        assert_eq!(fg(Some(50 * MIB - 1)), Some(crate::ui::theme::DIM));
        assert_eq!(fg(Some(50 * MIB)), None);
        assert_eq!(fg(Some(GIB - 1)), None);
        assert_eq!(fg(Some(GIB)), Some(crate::ui::theme::AMBER));
        assert_eq!(fg(Some(5 * GIB - 1)), Some(crate::ui::theme::AMBER));
        assert_eq!(fg(Some(5 * GIB)), Some(crate::ui::theme::ROSE));
    }
}
