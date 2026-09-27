//! A timeseries tile for on-demand source data and arithmetic expressions.
//! [`core`] owns the pure model and chart preparation; [`tile`] coordinates
//! requests, frame state, and interactions. [`header`] and [`popup`] render
//! controls around the chart painted by `geode-chart`.

pub mod commands;
pub mod core;

pub mod content;
pub mod header;
pub mod popup;
pub mod tile;

/// Reserve Tab and Shift+Tab for date-field switching and expression completion.
/// Call once during startup after component initialization.
///
/// GPUI dispatches matched actions before key listeners. Binding `NoAction`
/// in the editors' contexts suppresses the component root's focus cycling,
/// allowing each editor's listener to receive these keys. The focused
/// single-line input's deeper Tab binding has no indent handler and falls
/// through to that listener.
///
/// The shell also reserves these keys in `GeodeShell`; these local bindings
/// support independently hosted tiles without that context.
pub fn init(cx: &mut gpui::App) {
    for context in [popup::RANGE_CONTEXT, popup::EXPR_CONTEXT] {
        cx.bind_keys([
            gpui::KeyBinding::new("tab", gpui::NoAction, Some(context)),
            gpui::KeyBinding::new("shift-tab", gpui::NoAction, Some(context)),
        ]);
    }
}
