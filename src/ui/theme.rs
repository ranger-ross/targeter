//! omp-inspired widget chrome: gray cards with light gray borders.
//!
//! Values from omp's dark theme: card fill `#161a1f` (`toolSuccessBg`),
//! borders `#777d88` (`muted`), selection `#31363f` (`selectedBg`).

use ratatui::{
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, BorderType, Borders},
};
/// Card fill.
pub const CARD_BG: Color = Color::Rgb(0x16, 0x1a, 0x1f);
/// Light gray card borders.
pub const BORDER: Color = Color::Rgb(0x77, 0x7d, 0x88);
/// Selected row fill.
pub const SELECTED_BG: Color = Color::Rgb(0x31, 0x36, 0x3f);
/// Muted text (header row, secondary labels).
pub const MUTED: Color = Color::Rgb(0x77, 0x7d, 0x88);
/// Dim frame prefix.
pub const DIM: Color = Color::Rgb(0x5f, 0x66, 0x73);

/// Gray card with light gray rounded borders. The title echoes omp's
/// compact `╭─ Label` frame: dim prefix, bold label.
pub fn card<'a>(title: &'a str) -> Block<'a> {
    base().title(Line::from(vec![
        Span::styled("─", Style::default().fg(DIM)),
        Span::styled(title, Style::default().add_modifier(Modifier::BOLD)),
    ]))
}

/// Title-less variant for the projects table.
pub fn card_plain() -> Block<'static> {
    base()
}

/// Selected-row highlight matching omp's `selectedBg`.
pub fn selected() -> Style {
    Style::default()
        .bg(SELECTED_BG)
        .add_modifier(Modifier::BOLD)
}

fn base() -> Block<'static> {
    Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(BORDER))
        .style(Style::default().bg(CARD_BG))
}
