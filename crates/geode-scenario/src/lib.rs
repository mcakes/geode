//! Scenario tiles: five tile kinds (vega K/T, vega by maturity, spot ladder,
//! by maturity, rho) that paint aggregated scenario risk for whatever the
//! frame or a followed link group selects. [`core`] is the pure half —
//! the builtin `scenarios` shapes and the pivot that turns a grouped
//! snapshot into cells, Σ, diff and heat domain — tested without gpui.

pub mod core;
