//! The blotter (foundation §9.2, Phase 3 spec §6): any view definition
//! as a collapsible, keyboard-driven hierarchy with honest markers. The
//! pure core in `core` has no `gpui`; `delegate` adapts it to
//! gpui-component's `DataTable`; `tile` is the entity per tile;
//! `content` is what the shell hosts.

pub mod content;
pub mod core;
pub mod delegate;
pub mod tile;

pub use content::BlotterFactory;

/// Reclaim `DataTable`'s own key bindings (Phase 3 §3.3): the blotter
/// never gives the table focus, but a row click moves gpui focus there
/// for one frame, and these must not act during it. Same door and same
/// reasoning as `geode_shell::shell::dialog::init_reclaimed_keybindings`.
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
