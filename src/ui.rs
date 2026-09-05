use bytefmt::{Unit, format_to};
use chrono::{DateTime, Local};
use ratatui::{
    Frame,
    layout::{Constraint, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Cell, Paragraph, Row, Table},
};

use crate::app::App;

/// Binary-unit sizes matching `du -h` at a glance (MiB, GiB...).
/// `bytefmt::format` is decimal SI, which reads differently for the same bytes.
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

/// Canonicalize for display, falling back to the raw path on error.
fn display_path(app: &App, path: &std::path::Path) -> String {
    let base = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    // Strip the scan root prefix so deep trees stay readable.
    if let Ok(root) = std::fs::canonicalize(&app.root)
        && let Ok(rel) = base.strip_prefix(&root).map(|p| p.to_path_buf())
        && !rel.as_os_str().is_empty()
    {
        return format!("./{}", rel.display());
    }
    base.display().to_string()
}

fn format_modified(last_modified: std::time::SystemTime) -> String {
    let dt: DateTime<Local> = last_modified.into();
    // Same format as `cargo-clean-all`.
    dt.format("%Y-%m-%d %H:%M").to_string()
}

pub fn render(frame: &mut Frame, app: &mut App) {
    let area = frame.area();
    let [header_area, cache_area, table_area, footer_area] = Layout::vertical([
        Constraint::Length(3),
        Constraint::Length(3),
        Constraint::Min(0),
        Constraint::Length(3),
    ])
    .areas(area);

    let title = format!(
        " targeter: {} ",
        std::fs::canonicalize(&app.root)
            .unwrap_or_else(|_| app.root.clone())
            .display()
    );
    let visible = app.visible_indices();
    let visible_size: u64 = visible
        .iter()
        .filter_map(|&i| app.entries.get(i))
        .map(|e| e.size)
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
                title,
                Style::default()
                    .fg(Color::Green)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw(counts),
        ]))
        .block(Block::default().borders(Borders::ALL)),
        header_area,
    );
    render_build_cache(frame, cache_area, app);

    if app.scanning {
        frame.render_widget(
            Paragraph::new("Scanning for target/ directories…")
                .block(Block::default().borders(Borders::ALL).title("projects")),
            table_area,
        );
    } else if app.entries.is_empty() {
        frame.render_widget(
            Paragraph::new("No target/ directories found.")
                .block(Block::default().borders(Borders::ALL).title("projects")),
            table_area,
        );
    } else if visible.is_empty() {
        frame.render_widget(
            Paragraph::new(format!("No match for /{}.", app.filter_text))
                .block(Block::default().borders(Borders::ALL).title("projects")),
            table_area,
        );
    } else {
        let header = Row::new(["Project", "Size", "Modified", "Path"]).height(1);

        let rows = visible.iter().filter_map(|&i| app.entries.get(i)).map(|e| {
            Row::new([
                Cell::from(e.project_name()),
                Cell::from(format_size(e.size)),
                Cell::from(format_modified(e.last_modified)),
                Cell::from(display_path(app, &e.project_path)),
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
            .block(Block::default().borders(Borders::ALL).title("projects"))
            .row_highlight_style(
                Style::default()
                    .bg(Color::DarkGray)
                    .add_modifier(Modifier::BOLD),
            );
        frame.render_stateful_widget(table, table_area, &mut app.table_state);
    }

    render_footer(frame, footer_area, app);
}

/// Pinned row for the unstable cargo build cache. It lives outside the scan
/// root, so it gets its own section instead of a table row.
fn render_build_cache(frame: &mut Frame, area: Rect, app: &App) {
    let line = match &app.build_cache {
        Some(entry) => format!(
            "{} ({})  {}",
            format_size(entry.size),
            format_modified(entry.last_modified),
            std::fs::canonicalize(&entry.project_path)
                .unwrap_or_else(|_| entry.project_path.clone())
                .display()
        ),
        None if app.scanning => "Measuring…".to_string(),
        None => match crate::scan::build_cache_path() {
            Some(path) => format!("not present: {}", path.display()),
            None => "no cargo home found".to_string(),
        },
    };
    frame.render_widget(
        Paragraph::new(line).block(
            Block::default()
                .borders(Borders::ALL)
                .title("cargo build-cache (unstable)"),
        ),
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
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .title("filter (regex)"),
            ),
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
            Span::raw(if app.watching { " · live" } else { "" }),
        ]))
        .block(Block::default().borders(Borders::ALL)),
        area,
    );
}
