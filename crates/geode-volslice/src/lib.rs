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
pub mod tile;

pub use content::VolsliceFactory;

/// The tile kind, its keymap context and its session table name.
pub const KIND: &str = "volslice";

/// Startup hook beside the other modules' `init`. The tile reserves no key
/// beyond its fragment yet; the hook exists so the app's startup sequence
/// names every module once.
pub fn init(_cx: &mut gpui::App) {}
