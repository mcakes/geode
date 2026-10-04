//! The Classifications tile: one derived dimension per tile — every source
//! value with its label, edited in place and written through the config
//! door. See docs/current/features.md#classifications.
//!
//! [`core`] owns the pure parts (the session table); [`tile`] holds the
//! entity, its header and its switcher; [`content`] is the shell's door and
//! the factory the app pushes each configuration snapshot through.

pub mod content;
pub mod core;
pub mod tile;

pub use content::{ClassificationsConfig, ClassificationsFactory};

/// The module's tile kind, its keymap context and its session table name.
pub const KIND: &str = "classifications";

/// Bind the keys gpui-component's DataTable would otherwise swallow, so
/// the tile's own motions see them (the same set as the pricer's `init`).
/// Call once at startup, after component initialization.
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
