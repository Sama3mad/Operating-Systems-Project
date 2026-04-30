use iced::widget::{button, container, text};
use iced::{Background, Border, Color, Element, Font, Length};

use crate::data::{Message, SortMode};

// ─── View ─────────────────────────────────────────────────────────────────────

pub fn fmt_bytes(b: u64) -> String {
    if b >= 1_000_000 {
        format!("{:.1} MB/s", b as f64 / 1_000_000.0)
    } else if b >= 1_000 {
        format!("{:.1} KB/s", b as f64 / 1_000.0)
    } else {
        format!("{b} B/s")
    }
}

// Colors
pub const BG: Color = Color { r: 0.05, g: 0.07, b: 0.10, a: 1.0 };
pub const SURFACE: Color = Color { r: 0.09, g: 0.11, b: 0.16, a: 1.0 };
pub const SURFACE2: Color = Color { r: 0.12, g: 0.15, b: 0.21, a: 1.0 };
pub const BORDER: Color = Color { r: 0.18, g: 0.22, b: 0.30, a: 1.0 };
pub const CYAN: Color = Color { r: 0.20, g: 0.80, b: 1.0, a: 1.0 };
pub const AMBER: Color = Color { r: 1.0, g: 0.75, b: 0.20, a: 1.0 };
pub const GREEN: Color = Color { r: 0.25, g: 0.85, b: 0.55, a: 1.0 };
pub const RED: Color = Color { r: 1.0, g: 0.30, b: 0.30, a: 1.0 };
pub const MUTED: Color = Color { r: 0.45, g: 0.52, b: 0.62, a: 1.0 };
pub const WHITE: Color = Color::WHITE;

pub fn cell<'a>(content: impl Into<String>, color: Color) -> Element<'a, Message> {
    container(
        text(content.into())
            .size(13)
            .color(color)
            .font(Font::MONOSPACE),
    )
    .padding([4, 8])
    .width(Length::Fill)
    .into()
}

pub fn header_cell<'a>(label: &'a str) -> Element<'a, Message> {
    container(
        text(label)
            .size(12)
            .color(MUTED)
            .font(Font::MONOSPACE),
    )
    .padding([4, 8])
    .width(Length::Fill)
    .into()
}

pub fn sort_btn<'a>(label: &'a str, mode: SortMode, current: SortMode) -> Element<'a, Message> {
    let active = mode == current;
    let bg = if active { CYAN } else { SURFACE2 };
    let fg = if active { BG } else { MUTED };
    button(text(label).size(12).color(fg).font(Font::MONOSPACE))
        .on_press(Message::SetSort(mode))
        .style(move |_, _| button::Style {
            background: Some(Background::Color(bg)),
            border: Border {
                color: BORDER,
                width: 1.0,
                radius: 4.0.into(),
            },
            ..Default::default()
        })
        .padding([4, 10])
        .into()
}