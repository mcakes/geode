//! The market-data panel module: one tile per
//! `PanelSpec`, painting one document of a document dataset as a grid —
//! pivoted on two axes, or a row per document row with the value columns
//! laid flat — with a draft of unsent edits over the top.
//!
//! `core` is the pure half: the panel spec, the matrix
//! model a frame paints from, the draft, and the cell parser. It names no
//! element, entity or window, so its tests run without one — the sole
//! `gpui` type it borrows is `SharedString`, a refcounted string, so that
//! a prepared cell hands a frame its text without allocating.
//! [`commands`] is the second pure half: the `:` vocabulary.
//!
//! [`tile`], [`delegate`] and [`content`] are the gpui half: the entity
//! that requests its document through `DataHandle`, the `TableDelegate`
//! its body is painted through, and the `TileContent`/`ModuleFactory` pair
//! the shell hosts it through. Cell editing (insert mode, `:bump`, `:revert`) and the
//! draft states (`Behind`, `:rebase`) are both here, and so is `:upload`:
//! a y/n confirm, submission through `DataHandle::upload`, and the
//! `Sent`/failure outcome.

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

/// Reclaim `DataTable`'s own key bindings, exactly as `geode_blotter::init`
/// does and for the same reason: this panel's body is a gpui-component table
/// which is never given focus, but a click inside it moves gpui focus there
/// for one frame, and
/// the component's own `escape`/arrows/`tab` actions must not act during
/// it — they would fight the panel's `h j k l` and its cell editor.
///
/// Deliberately a second copy rather than a call into `geode-blotter`:
/// this crate must not depend on that one, and binding the same keys to
/// `NoAction` twice is harmless (the later `bind_keys` simply wins with
/// the same answer).
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
