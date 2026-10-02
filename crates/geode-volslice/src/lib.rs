//! A vol slice viewer tile: one underlying's volatility smiles, one curve
//! per expiry and per kind (the published CVI, the link group's draft, the
//! option chain's marks), with an optional difference pane and densities.
//! [`core`] owns the pure model; [`tile`] coordinates requests, frame state
//! and interaction; [`content`] is the shell's door. Every vol, coordinate
//! and density painted comes out of the data tier's vol door: this crate
//! computes none of them.

pub mod commands;
pub mod content;
pub mod core;
mod header;
mod strip;
pub mod tile;

pub use content::VolsliceFactory;

/// The tile kind, its keymap context and its session table name.
pub const KIND: &str = "volslice";

/// Reserve `tab` and `shift-tab` in the underlying picker's field for
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
