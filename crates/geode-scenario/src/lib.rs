//! Scenario tiles: five tile kinds (vega K/T, vega by maturity, spot ladder,
//! by maturity, rho) that paint aggregated scenario risk for whatever the
//! frame or a followed link group selects. [`core`] is the pure half (the
//! builtin `scenarios` shapes, the pivot, the `:` grammar, the session table
//! and cell preparation), tested without gpui; [`tile`] holds the entity,
//! `delegate` its table, `header` its chrome, and [`content`] the
//! factory the app registers once per accepted shape.

pub mod content;
pub mod core;
mod delegate;
mod header;
pub mod tile;

pub use content::ScenarioFactory;

/// The keymap context every scenario kind shares, and the action prefix.
pub const CONTEXT: &str = "scenario";

/// Bind the keys gpui-component's DataTable would otherwise swallow, so
/// the tile's own motions see them (the same set as the classifications
/// and watchlist tiles' `init`). Call once at startup, after component
/// initialization.
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
