use chrono::{DateTime, Local};
use ratatui::{
    Frame,
    layout::{Constraint, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Cell, Paragraph, Row, Table},
};

use crate::app::App;

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
    let [header_area, table_area, footer_area] = Layout::vertical([
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
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(
                title,
                Style::default()
                    .fg(Color::Green)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw(format!(
                "{} project{} · {} total",
                app.entries.len(),
                if app.entries.len() == 1 { "" } else { "s" },
                bytefmt::format(app.total_size)
            )),
        ]))
        .block(Block::default().borders(Borders::ALL)),
        header_area,
    );

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
    } else {
        let header = Row::new(["Project", "Size", "Modified", "Path"])
            .style(Style::default().add_modifier(Modifier::BOLD))
            .height(1);

        let rows = app.entries.iter().map(|e| {
            Row::new([
                Cell::from(e.project_name()),
                Cell::from(bytefmt::format(e.size)),
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

fn render_footer(frame: &mut Frame, area: Rect, app: &App) {
    let status = if app.scanning {
        "scanning…".to_string()
    } else {
        format!("sort: {} (s)", app.sort.label())
    };
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::raw("↑/↓ navigate · g/G top/bottom · s "),
            Span::raw(status),
            Span::raw(" · r rescan · q quit"),
        ]))
        .block(Block::default().borders(Borders::ALL)),
        area,
    );
}
