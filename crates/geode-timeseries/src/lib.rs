//! The timeseries viewer module: a tile that plots
//! series fetched on demand, composed by arithmetic expression, managed
//! through header chips and a popup, painted by `geode-chart`.

pub mod commands;
pub mod core;

pub mod content;
pub mod header;
pub mod popup;
pub mod tile;

/// Reclaim `tab`/`shift+tab` inside the range popup, the
/// same door and the same mechanism as `geode_blotter::init` and
/// `geode_marketdata::init`: gpui-component's `Root` binds both keys
/// window-wide to its own focus cycling (`root::Tab`), and gpui
/// dispatches a matched ACTION before any `on_key_down` listener — so
/// without this the popup's own listener never sees the keystroke that
/// moves between its two date fields.
///
/// `NoAction` is gpui's own mechanism for exactly this: the deepest
/// matching context wins, and a `NoAction` there suppresses every
/// out-ranked binding, leaving the keystroke to the key listeners
/// (`geode_shell::shell::dialog::init_reclaimed_keybindings` has the
/// long form of the argument). It is scoped to the popup's own context,
/// so `tab` keeps cycling focus everywhere else — the shell's own
/// scoping rule for this key.
///
/// Must be called once at startup, beside the other modules' inits.
pub fn init(cx: &mut gpui::App) {
    let context = Some(popup::RANGE_CONTEXT);
    cx.bind_keys([
        gpui::KeyBinding::new("tab", gpui::NoAction, context),
        gpui::KeyBinding::new("shift-tab", gpui::NoAction, context),
    ]);
}
