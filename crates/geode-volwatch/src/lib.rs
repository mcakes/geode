//! The VolMonitor tile: a watchlist's members as rows of a vol board.
//!
//! [`core`] owns the pure parts (views, their columns and the board ask);
//! [`tile`] holds the entity; [`content`] is the shell's door and factory.

pub mod content;
pub mod core;
pub mod tile;

pub use core::views::KIND;

/// Bind the keys gpui-component's DataTable would otherwise swallow, so
/// the tile's own motions see them. Call once at startup, after component
/// initialization.
pub fn init(cx: &mut gpui::App) {
    // The table must never take the arrows, tab or escape: the tile's keymap owns them.
    const CONTEXT: Option<&str> = Some("DataTable");
    cx.bind_keys(
        [
            "escape",
            "up",
            "down",
            "left",
            "right",
            "home",
            "end",
            "pageup",
            "pagedown",
            "tab",
            "shift-tab",
        ]
        .into_iter()
        .map(|key| gpui::KeyBinding::new(key, gpui::NoAction, CONTEXT)),
    );
}
