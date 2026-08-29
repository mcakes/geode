//! The settings modal (Task 5, migrated onto Task 9's instant-modal chrome
//! — see `dialog`'s module doc): Geode's own `dialog::render_modal` wrapping
//! the crate's own `setting` module composite. No gpui-component `Dialog`
//! import lives here anymore — `open` hands its content straight to
//! [`open_shell_dialog`], which is the only place that still knows anything
//! about how a modal gets painted.
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
//! `theme::toggle_mode`), then persist into the user config layer —
//! [`set_theme`]/[`set_dark_mode`] both call `ShellView::persist_theme`
//! right after applying, which writes `<user_dir>/app.toml`'s `[theme]`
//! table via `theme::persist_to_user_config` (`toml_edit`, format- and
//! comment-preserving). The muted caption under the theme controls reflects
//! this now: "saved to your app.toml".

use gpui::{
    App, Context, Entity, InteractiveElement as _, IntoElement as _, ParentElement as _,
    SharedString, Styled as _, Window, div, px,
};
use gpui_component::{
    ActiveTheme as _,
    label::Label,
    setting::{SettingField, SettingGroup, SettingItem, SettingPage, Settings},
    v_flex,
};

use crate::keymap::Modifiers;
use crate::shell::ShellView;
use crate::shell::dialog;
use crate::shell::dialog::open_shell_dialog;
use crate::theme::Mode;

/// Open the settings modal (`settings::open`: `mod+,`, the palette entry,
/// and the sidebar profile icon all reach this). A no-op if a modal is
/// already open — `open_shell_dialog` unconditionally sets `view.modal`,
/// and re-triggering the action while one is already up (e.g. a second
/// `mod+,`) should not clobber whatever's currently open with a fresh
/// settings modal.
///
/// Goes through [`open_shell_dialog`] (Task 9) rather than touching `view.
/// modal` itself — the crate's one standard door, so this modal gets the
/// same pending-sequence/palette hygiene as every other one (see
/// `dialog`'s module doc: Task 9's instant-modal redesign keeps that rule
/// even though opening no longer means `window.open_dialog`). `view`'s
/// `Entity` handle is grabbed via `cx.entity()` before the call, since the
/// content closure below needs a clone to read/update `ShellView`'s
/// services later, when the modal actually renders — not `&mut ShellView`
/// itself, which `open_shell_dialog` already borrows for its own hygiene.
pub fn open(view: &mut ShellView, window: &mut Window, cx: &mut Context<ShellView>) {
    if view.modal.is_some() {
        return;
    }
    let entity = cx.entity();
    // The fixed 720px width the old `Dialog` set on itself (`.w(px(720.))`)
    // — `Settings`' own root (`h_resizable`, pinned checkout `crates/ui/
    // src/setting/settings.rs`) has no intrinsic width of its own (its
    // sidebar is sized as `w(relative(1.))`, 100% of whatever ancestor
    // hands it one), so this crate's own modal chrome — generic over
    // arbitrary content, with no width opinion of its own — needs this
    // call site to keep providing one, same as before.
    open_shell_dialog(view, window, cx, "Settings", move |shell, window, cx| {
        // Content-collapse fix (root cause): `Settings`' own root
        // (`ResizablePanelGroup::render`, pinned checkout `crates/base/src/
        // resizable/panel.rs`) is `.size_full()` — a *percentage* height,
        // which only resolves against a parent whose own height was
        // explicitly specified, not one that is itself sized from its
        // content (directly, or transitively through a `flex_auto`/`max_h`
        // ancestor with no explicit height of its own — confirmed
        // empirically: giving `dialog::render_modal`'s content wrapper a
        // `flex_1()` (flex-basis 0%) instead of `flex_auto()` collapsed it
        // right back to a sliver too, because `flex_1`'s zero basis, with
        // no definite space on its own un-sized ancestor to grow into,
        // discards this div's own explicit height from consideration
        // entirely — the wrapper needs `flex_auto()`, which sizes from its
        // content, i.e. from *this* div's real height). So this wrapper —
        // the direct, immediate parent of `Settings` — needs its own
        // explicit height, not `h_full()` (still just a percentage, still
        // 0 against this div's un-sized parent). Reading `window.
        // viewport_size()` here, applying the *same* `dialog::
        // MODAL_MAX_HEIGHT_RATIO` the panel caps itself at, minus
        // `dialog::MODAL_CHROME_ALLOWANCE` for the title row/paddings the
        // panel also has to fit inside that same cap, keeps this content
        // comfortably within the panel's cap in the common case; when it
        // doesn't (a very short window), `render_modal`'s content wrapper
        // (`flex_auto()` + `overflow_y_scrollbar()`) simply scrolls the
        // excess instead of pushing the panel past its own `max_h`.
        let content_height = (f32::from(window.viewport_size().height)
            * dialog::MODAL_MAX_HEIGHT_RATIO
            - dialog::MODAL_CHROME_ALLOWANCE)
            .max(200.);
        div()
            .w(px(720.))
            .h(px(content_height))
            // Test-only hook (no-op outside test/test-support builds, see
            // gpui's own `debug_selector` doc comment): lets a
            // `#[gpui::test]` recover this wrapper's painted bounds via
            // `VisualTestContext::debug_bounds`, to click into the
            // `Settings` composite's own search input (Task 9 review fix:
            // there is no public way to reach that input's `FocusHandle`
            // directly — `SettingsState`/`search_input` are `pub(super)` in
            // the pinned gpui-component checkout, reachable only from
            // inside that crate's own `setting` module — so a real click at
            // its on-screen position, inside this wrapper's bounds, is the
            // only way an external test can drive focus into it).
            .debug_selector(|| "settings-content".to_string())
            .child(build(shell, entity.clone(), window, cx))
            .into_any_element()
    });
}

/// Build the `Settings` composite content described in the module doc.
///
/// Takes *both* `shell: &ShellView` and `view: Entity<ShellView>` — not
/// redundant, see `ShellModal::build`'s doc comment for the full story:
/// `shell` is this exact call's plain-borrow read of whatever's needed
/// *right now* (`theme_names`, `mod_alias`, both read once per build to
/// seed the dropdown/mod-key display), safe because it's a Rust borrow, not
/// an entity-handle access, even though this runs nested inside `ShellView
/// ::render` itself; `view` is the `Entity` clone every get/set closure
/// below captures for its OWN, later, read/update (fetching a field's
/// current value at that field's own layout/paint time, or applying a
/// user's edit at click/change time) — both safely outside `ShellView::
/// render`'s call frame by the time they actually run, so `Entity::read`/
/// `update` there carries none of the reentrancy risk a synchronous
/// `view.read(cx)` right here would.
fn build(
    shell: &ShellView,
    view: Entity<ShellView>,
    _window: &mut Window,
    _cx: &mut App,
) -> Settings {
    let theme_names = shell.services.theme.names();
    let mod_alias = shell.services.mod_alias;

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
                .child("saved to your app.toml")
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
/// `ThemeService::apply`, then persist it (`ShellView::persist_theme`) —
/// the theme dropdown's setter. Kept as a standalone function (rather than
/// inlined only in the closure above) so a `#[gpui::test]` can drive
/// exactly this path directly: the dropdown's own popup menu is a
/// pinned-rev `dropdown_menu_with_anchor` overlay this crate has no direct
/// handle to simulate a click into, so the test-plan choice (recorded in
/// the task report) is to call this same handler the control invokes
/// rather than simulate the click.
pub(crate) fn set_theme(view: &Entity<ShellView>, name: &str, cx: &mut App) {
    view.update(cx, |shell, cx| {
        let mode = shell.services.theme.active_mode();
        shell.services.theme.apply(name, mode, cx);
        shell.persist_theme(cx);
        cx.notify();
    });
}

/// Apply the mode a dark-mode switch's `checked` value implies via
/// `ThemeService::set_mode`, then persist it — same reasoning as
/// [`set_theme`].
pub(crate) fn set_dark_mode(view: &Entity<ShellView>, checked: bool, cx: &mut App) {
    let mode = if checked { Mode::Dark } else { Mode::Light };
    view.update(cx, |shell, cx| {
        shell.services.theme.set_mode(mode, cx);
        shell.persist_theme(cx);
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
