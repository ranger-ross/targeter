//! omp-inspired widget chrome: gray cards with light gray borders.
//!
//! Values from omp's dark theme: card fill `#161a1f` (`toolSuccessBg`),
//! borders `#777d88` (`muted`), selection `#31363f` (`selectedBg`).

use ratatui::{
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, BorderType, Borders},
};
pub const CARD_BG: Color = Color::Rgb(0x16, 0x1a, 0x1f);
pub const BORDER: Color = Color::Rgb(0x77, 0x7d, 0x88);
pub const SELECTED_BG: Color = Color::Rgb(0x31, 0x36, 0x3f);
pub const MUTED: Color = Color::Rgb(0x77, 0x7d, 0x88);
pub const DIM: Color = Color::Rgb(0x5f, 0x66, 0x73);

/// Gray card with rounded borders. Titles render as dim `─` plus bold label.
pub fn card<'a>(title: &'a str) -> Block<'a> {
    base().title(Line::from(vec![
        Span::styled("─", Style::default().fg(DIM)),
        Span::styled(title, Style::default().add_modifier(Modifier::BOLD)),
    ]))
}

pub fn card_plain() -> Block<'static> {
    base()
}

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
