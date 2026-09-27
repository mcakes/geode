//! Configured views rendered as collapsible, keyboard-driven hierarchies.
//!
//! [`core`] owns column planning, navigation, selection, and formatting
//! without GPUI. [`delegate`] adapts that model to gpui-component's
//! `DataTable`; [`tile`] owns requests and frame synchronization;
//! [`content`] exposes the tile and its factory to the shell.

pub mod colour_cache;
pub mod content;
pub mod core;
pub mod delegate;
pub mod tile;

pub use content::BlotterFactory;

/// Suppress `DataTable` navigation while the shell owns blotter key routing.
/// Call after `gpui_component::init` so these bindings take precedence.
/// A row click can focus the table for one frame; its component actions
/// must remain inactive until the shell restores focus.
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
