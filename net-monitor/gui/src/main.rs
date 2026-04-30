use iced::{
    Theme,
    window,
};

mod data;
mod chart;
mod app;
mod view;
mod export;

use crate::app::NetMonitor;

// ─── Main ─────────────────────────────────────────────────────────────────────

fn main() -> iced::Result {
    // On WSL there is no Wayland compositor; force X11 so winit doesn't panic.
    // This is a no-op on native Linux desktops that already have DISPLAY set.
    if std::env::var("WAYLAND_DISPLAY").is_err() {
        // Only set if not already overridden by the user
        if std::env::var("WINIT_UNIX_BACKEND").is_err() {
            std::env::set_var("WINIT_UNIX_BACKEND", "x11");
        }
    }

    iced::application("Net Monitor", NetMonitor::update, NetMonitor::view)
        .subscription(NetMonitor::subscription)
        .window(window::Settings {
            size: iced::Size::new(1100.0, 720.0),
            min_size: Some(iced::Size::new(800.0, 500.0)),
            ..Default::default()
        })
        .theme(|_| Theme::Dark)
        .antialiasing(true)
        .run_with(NetMonitor::new)
}