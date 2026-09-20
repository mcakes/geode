//! The tile's one overlay (spec §9.5–§9.8): the add picker, the
//! expression editor, the series list, the range dialog. Tasks 8–10
//! build them.
//!
//! Declared here, empty, in Task 6 so [`crate::tile::TimeseriesTile`]'s
//! `key_context` and `holds_focus` are written ONCE — against the shape
//! they will keep — rather than being retrofitted when the first variant
//! lands. An uninhabited enum makes both bodies total: `match *self {}`
//! is the exhaustive match over no variants, and the tile's
//! `popup: Option<Popup>` is therefore always `None` in this task.

/// Uninhabited until Task 8. Every method below is the empty match, so
/// adding a variant makes the compiler name each one.
pub(crate) enum Popup {}

impl Popup {
    /// Whether this popup holds the keyboard as a text field — what puts
    /// the tile's key context into `insert` mode.
    pub(crate) fn is_insert(&self) -> bool {
        match *self {}
    }

    /// Whether one of this popup's own inputs holds WINDOW focus right
    /// now (`TileContent::holds_focus`'s ownership half) — answered off
    /// the focus handle, never off the mode.
    pub(crate) fn holds_focus(&self, _window: &gpui::Window, _cx: &gpui::App) -> bool {
        match *self {}
    }
}
