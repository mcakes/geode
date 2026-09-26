//! Document-panel module: typed pivot or flat grids with retained drafts,
//! editing, generation conflict handling, per-underlying draft parking, and upload.
//!
//! Core and commands own pure interpretation and state. The tile requests data,
//! handles edits and delivery, and shares prepared models with the table delegate
//! and header. Content exposes the module factory and shell integration.
//!
//! Upload requires confirmation, then bounded submission through DataHandle.
//! A transport outcome and a later document echo are distinct: success marks only
//! an unchanged Editing draft Sent; the echo determines whether its edits clear.

pub mod commands;
pub mod content;
pub mod core;
pub mod delegate;
pub(crate) mod header;
mod popup;
pub mod tile;

pub use content::{ACTIONS, DEFAULT_KEYMAP, MarketDataFactory};
pub use delegate::MatrixDelegate;
pub use tile::MarketDataTile;

/// Unbind the component table's navigation actions in DataTable context so
/// transient table focus cannot compete with the panel's cursor and editor keys.
/// This module installs its own bindings to avoid depending on a sibling feature;
/// repeated NoAction bindings for the same keys preserve the same behavior.
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
