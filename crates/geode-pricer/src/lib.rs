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
    // The entry bar's completion owns tab/shift-tab: gpui-component's Root
    // binds both to focus cycling, and a matched action runs before the
    // bar's key listener. Suppressing them lets the entry listener cycle suggestions.
    // The sheet picker's `tab` completes to the highlighted name, the same
    // way; the rename field's does nothing, keeping the keyboard in it.
    for context in [
        header::ENTRY_CONTEXT,
        popup::SHEET_PICKER_CONTEXT,
        header::RENAME_CONTEXT,
    ] {
        cx.bind_keys(
            ["tab", "shift-tab"]
                .map(|key| gpui::KeyBinding::new(key, gpui::NoAction, Some(context))),
        );
    }
}
