//! The calibration graph tile: one pipeline of the `calibration` document
//! over one underlying, drawn as a DAG of stages. [`content`] is the
//! shell's door and the factory the app pushes the read document through;
//! [`tile`] is the hosted entity. The tile computes no fit itself: every
//! run goes through the data tier's calibration door and comes back as a
//! delivery.

pub mod content;
pub mod core;
pub mod tile;

pub use content::{ACTIONS, CalibrationConfig, CalibrationFactory, DEFAULT_KEYMAP};

/// The tile kind, its keymap context and its session table name.
pub const KIND: &str = "calibration";

/// Reserve `tab` and `shift-tab` in the tile's picker fields for
/// completion. Call once at startup, after component initialization.
///
/// GPUI dispatches matched actions before key listeners, so without a
/// `NoAction` binding in the picker's context the component root's focus
/// cycling would take `tab` before the picker's listener sees it.
pub fn init(cx: &mut gpui::App) {
    cx.bind_keys([
        gpui::KeyBinding::new("tab", gpui::NoAction, Some(tile::PICKER_CONTEXT)),
        gpui::KeyBinding::new("shift-tab", gpui::NoAction, Some(tile::PICKER_CONTEXT)),
    ]);
}
