//! The settings dialog (Task 5): a gpui-component `Dialog` wrapping the
//! crate's own `setting` module composite.
//!
//! ## Inventory findings (pinned checkout rev `0e2fb7a`,
//! `crates/ui/src/setting/{fields,group,item,page,settings}.rs`, plus the
//! reference example `crates/story/src/stories/settings_story.rs`)
//!
//! - The hierarchy is `Settings` (the whole panel: search + page sidebar +
//!   active page) -> `SettingPage` -> `SettingGroup` -> `SettingItem` ->
//!   `SettingField` (typed get/set closures over `&App`/`&mut App`, with a
//!   built-in renderer per field kind: `switch`/`checkbox`, `dropdown`,
//!   `input`, `number_input`, or a fully custom `element`/`render`).
//! - **No fallback needed, and none is available**: `SettingGroup::render`
//!   and `SettingPage::render` are `pub(crate)` in the pinned checkout —
//!   reachable only from inside the `gpui_component` crate itself. The
//!   *only* public rendering entry point into this module, from an outside
//!   crate like this one, is `Settings`'s own `RenderOnce` impl (public,
//!   since `Settings: IntoElement`). So `Settings::new(id).page(...)` is not
//!   a stylistic choice among several ways to reach this UI; it is the only
//!   one the crate exposes. (Confirmed by reading `setting/mod.rs`'s
//!   `pub use` list against each file's own visibility, not just by
//!   inference from the story example using it.)
//! - `settings_story.rs`'s "Dark Mode" field calls `Theme::global_mut(cx)`/
//!   `Theme::change` directly, because that story owns no equivalent
//!   service. This module's fields never do that: every get/set closure
//!   routes through the same `ThemeService` methods `theme::toggle_mode`
//!   already uses (`apply`, `set_mode`), via `Entity<ShellView>::update` —
//!   see [`set_theme`]/[`set_dark_mode`] — so `ThemeService`'s own
//!   `active_name`/`active_mode`/`active_family` bookkeeping (what the
//!   status bar and session-save read) never goes stale behind a
//!   side-channel `Theme::change` call.
//!
//! Content v1 (brief): one page, two groups — **Appearance** (theme family
//! dropdown, light/dark switch, both live-applying) and **Keyboard**
//! (read-only mod-key display). Neither group is resettable: there is no
//! meaningful "default" to reset *to* here (the config file is the real
//! default, and writing it back is the config-editor phase's job, not
//! this dialog's) — a reset button implying otherwise would be misleading.
//!
//! **Persistence**: changes apply live (through `ThemeService`, same as
//! `theme::toggle_mode`) but are never written to `app.toml` — that is the
//! config-editor phase's job (brief). The muted caption under the theme
//! controls says so, honestly, rather than silently discarding the
//! expectation that a UI change usually persists.

use gpui::{App, Context, Entity, ParentElement as _, SharedString, Styled as _, Window, div, px};
use gpui_component::{
    ActiveTheme as _, WindowExt as _,
    label::Label,
    setting::{SettingField, SettingGroup, SettingItem, SettingPage, Settings},
    v_flex,
};

use crate::keymap::Modifiers;
use crate::shell::ShellView;
use crate::shell::dialog::open_shell_dialog;
use crate::theme::Mode;

/// Open the settings dialog (`settings::open`: `mod+,`, the palette entry,
/// and the sidebar profile icon all reach this). A no-op if a dialog is
/// already open — `window.open_dialog` stacks a fresh overlay layer on
/// every call, and re-triggering the action while the dialog is already up
/// (e.g. a second `mod+,`) should not pile up duplicate dialogs.
///
/// Goes through [`open_shell_dialog`] (Task 9) rather than calling
/// `window.open_dialog` itself — the crate's one standard door, so this
/// dialog gets the same pending-sequence/palette hygiene as every other one.
/// `view`'s `Entity` handle is grabbed via `cx.entity()` before the call,
/// since the content closure below needs a clone to read/update
/// `ShellView`'s services later, when the dialog actually renders — not
/// `&mut ShellView` itself, which `open_shell_dialog` already borrows for
/// its own hygiene.
pub fn open(view: &mut ShellView, window: &mut Window, cx: &mut Context<ShellView>) {
    if window.has_active_dialog(cx) {
        return;
    }
    let entity = cx.entity();
    open_shell_dialog(view, window, cx, move |dialog, _window, _cx| {
        let entity = entity.clone();
        dialog
            .title("Settings")
            .w(px(720.))
            .content(move |content, window, cx| content.child(build(entity.clone(), window, cx)))
    });
}

/// Build the `Settings` composite content described in the module doc.
fn build(view: Entity<ShellView>, _window: &mut Window, cx: &mut App) -> Settings {
    let theme_names = view.read(cx).services.theme.names();
    let mod_alias = view.read(cx).services.mod_alias;

    let dropdown_options: Vec<(SharedString, SharedString)> = theme_names
        .into_iter()
        .map(|name| (SharedString::from(name.clone()), SharedString::from(name)))
        .collect();

    let appearance = SettingGroup::new().title("Appearance").items(vec![
        SettingItem::new(
            "Theme",
            SettingField::dropdown(
                dropdown_options,
                {
                    let view = view.clone();
                    move |cx: &App| {
                        SharedString::from(view.read(cx).services.theme.active_name().to_string())
                    }
                },
                {
                    let view = view.clone();
                    move |value: SharedString, cx: &mut App| set_theme(&view, value.as_ref(), cx)
                },
            ),
        )
        .description("Theme family, from the bundled set (a lens, not a brand exercise)."),
        SettingItem::new(
            "Dark mode",
            SettingField::switch(
                {
                    let view = view.clone();
                    move |cx: &App| view.read(cx).services.theme.active_mode().is_dark()
                },
                {
                    let view = view.clone();
                    move |checked: bool, cx: &mut App| set_dark_mode(&view, checked, cx)
                },
            ),
        )
        .description("Light/dark variant of the active theme family."),
        SettingItem::render(|_, _, cx| {
            div()
                .text_sm()
                .text_color(cx.theme().muted_foreground)
                .child("set [theme] in app.toml to persist")
        }),
    ]);

    let keyboard = SettingGroup::new()
        .title("Keyboard")
        .item(SettingItem::render(move |_, _, cx| {
            v_flex()
                .gap_1()
                .child(Label::new(format!(
                    "Mod key: {}",
                    mod_alias_label(mod_alias)
                )))
                .child(
                    div()
                        .text_sm()
                        .text_color(cx.theme().muted_foreground)
                        .child("set via [keymap] mod in config"),
                )
        }));

    let page = SettingPage::new("Settings")
        .default_open(true)
        .resettable(false)
        .groups(vec![appearance, keyboard]);

    Settings::new("geode-settings")
        .sidebar_width(px(200.))
        .page(page)
}

/// The `[keymap] mod` value that produces `mods` (see
/// `defaults::mod_alias_from_config`) — falls back to `"alt"`, matching
/// `defaults::default_mod`, for anything that isn't exactly one of the
/// three named aliases.
fn mod_alias_label(mods: Modifiers) -> &'static str {
    if mods == Modifiers::CTRL {
        "ctrl"
    } else if mods == Modifiers::CMD {
        "cmd"
    } else {
        "alt"
    }
}

/// Apply `name` at the theme's currently active mode via
/// `ThemeService::apply` — the theme dropdown's setter. Kept as a
/// standalone function (rather than inlined only in the closure above) so
/// a `#[gpui::test]` can drive exactly this path directly: the dropdown's
/// own popup menu is a pinned-rev `dropdown_menu_with_anchor` overlay this
/// crate has no direct handle to simulate a click into, so the test-plan
/// choice (recorded in the task report) is to call this same handler the
/// control invokes rather than simulate the click.
pub(crate) fn set_theme(view: &Entity<ShellView>, name: &str, cx: &mut App) {
    view.update(cx, |shell, cx| {
        let mode = shell.services.theme.active_mode();
        shell.services.theme.apply(name, mode, cx);
        cx.notify();
    });
}

/// Apply the mode a dark-mode switch's `checked` value implies via
/// `ThemeService::set_mode` — same reasoning as [`set_theme`].
pub(crate) fn set_dark_mode(view: &Entity<ShellView>, checked: bool, cx: &mut App) {
    let mode = if checked { Mode::Dark } else { Mode::Light };
    view.update(cx, |shell, cx| {
        shell.services.theme.set_mode(mode, cx);
        cx.notify();
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mod_alias_label_matches_each_named_alias() {
        assert_eq!(mod_alias_label(Modifiers::CTRL), "ctrl");
        assert_eq!(mod_alias_label(Modifiers::CMD), "cmd");
        assert_eq!(mod_alias_label(Modifiers::ALT), "alt");
    }

    #[test]
    fn mod_alias_label_falls_back_to_alt_for_an_unnamed_combination() {
        assert_eq!(
            mod_alias_label(Modifiers::NONE),
            "alt",
            "an unrecognized alias should read as the same default \
             defaults::default_mod uses, not silently mislabel"
        );
    }
}
