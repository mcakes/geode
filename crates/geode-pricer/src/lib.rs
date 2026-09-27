//! The line-pricer module: a tile whose rows are
//! option lines and packages, priced through the data tier's pricing
//! request. [`core`] is the pure half — it names no element, entity,
//! window, data service or pricing implementation; the rest is the tile.

pub mod core;

pub mod content;
pub mod delegate;
pub mod grid;
pub mod header;
pub mod paint;
pub mod popup;
pub mod session;
pub mod store;
pub mod tile;

/// Suppress component navigation so the tile's keymap owns grid movement.
/// Call once after component initialization. These bindings remain active
/// when a pointer press temporarily moves focus into the table.
pub fn init(cx: &mut gpui::App) {
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
