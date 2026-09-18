//! Tooltips (spec 2026-09-17 §5.1): what a hover says about a mouse
//! affordance that has a keyboard twin — the title, the chord that does
//! the same thing (from the LIVE keymap, so a trader's rebinding shows),
//! and an optional detail line. Pure above the render section; the
//! render helper builds the view only inside the hover closure so an
//! idle window pays nothing per frame (charter: per-frame churn is a
//! defect).
//!
//! `Chords` is the second gpui global in the workspace, beside
//! `linenumbers::UiSettings`, for the same reason: a module (the
//! market-data tile's `⋯` button) has no path to `ShellView` and needs
//! the keymap's bindings to name its own chord. The shell writes it at
//! startup and after every keymap rebuild; modules only read it.

use std::sync::Arc;

use gpui::SharedString;

use crate::actions::ActionId;
use crate::keymap::{Binding, Keystroke, effective_binding};

/// The keymap's bindings as a module-visible snapshot. `Arc` so a
/// reload swaps one pointer; readers clone the `Arc`, never the `Vec`.
#[derive(Clone, Debug, Default)]
pub struct Chords(pub Arc<Vec<Binding>>);

impl gpui::Global for Chords {}

/// The chord that dispatches `action` under the current keymap, or
/// `None` when it is unbound (or shadowed by a later `"none"`). The
/// same display-only resolution the keybindings dialog lists — see
/// `keymap::effective_binding`'s doc for the documented context
/// approximation.
pub fn chord_for(bindings: &[Binding], action: &str) -> Option<Vec<Keystroke>> {
    // One String allocation for the ActionId — fine here: this runs only
    // inside a hover closure, never per frame.
    let id = ActionId(action.to_string());
    effective_binding(bindings, &id).map(|b| b.keystrokes.clone())
}

/// What one tooltip says. Built inside the hover closure, never per
/// frame.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TipModel {
    pub title: SharedString,
    pub chord: Option<Vec<Keystroke>>,
    pub detail: Option<SharedString>,
}

impl TipModel {
    pub fn resolve(
        title: impl Into<SharedString>,
        action: Option<&str>,
        detail: Option<SharedString>,
        bindings: &[Binding],
    ) -> Self {
        Self {
            title: title.into(),
            chord: action.and_then(|a| chord_for(bindings, a)),
            detail,
        }
    }
}

// ───────────────────────── render (the only gpui-touching part) ────────

use gpui::{
    AnyElement, AnyView, App, InteractiveElement, IntoElement, ParentElement, Styled, Window, div,
    prelude::FluentBuilder,
};
use gpui_component::{ActiveTheme as _, h_flex, tooltip::Tooltip, v_flex};

/// The tooltip closure for a site whose name and action id are
/// literals. The model is resolved INSIDE the closure — on hover, never
/// per frame. `try_global`: a module test fixture that never installed
/// `Chords` paints a chord-less tooltip rather than panicking.
///
/// `selector` is the FULL, already-prefixed selector (`"tip-sidebar-
/// profile"`, not `"sidebar-profile"`) — fix round 1: this function used
/// to `format!("tip-{site}")` every call, and since `.tooltip(tips::
/// tip(..))` is invoked inline in a render path, that `format!` ran once
/// per chip per render rather than once per hover (charter: per-frame
/// heap churn is a defect).
///
/// `selector` and `title` are both `&'static str`, built via
/// `SharedString::new_static` rather than `.into()` — fix round 2: a
/// literal is always spelled through `new_static`, never `.into()`
/// (`From<&str>` inlines ≤ 23 bytes and heap-allocates above, so a site
/// must not depend on a title's length to stay allocation-free; this
/// crate's own impossible-chip title is 75 bytes and its selector is 25,
/// both well past the inline cap). Every `tip()` caller passes literals
/// (the sidebar's consts, the profile icon, the impossible chip); a
/// caller with an owned title uses [`tip_with`] instead.
pub fn tip(
    selector: &'static str,
    title: &'static str,
    action: Option<&'static str>,
    detail: Option<SharedString>,
) -> impl Fn(&mut Window, &mut App) -> AnyView + 'static {
    let title = SharedString::new_static(title);
    let selector = SharedString::new_static(selector);
    move |window, cx| {
        let empty = Vec::new();
        let bindings = cx
            .try_global::<Chords>()
            .map(|c| c.0.as_slice())
            .unwrap_or(&empty);
        let model = TipModel::resolve(title.clone(), action, detail.clone(), bindings);
        Tooltip::element({
            let selector = selector.clone();
            move |_window, cx| render_tip(&model, selector.clone(), cx)
        })
        .build(window, cx)
    }
}

/// As [`tip`], for a site whose selector and title are built at render
/// time (`"tip-scope-chip-{column}"`) rather than being `&'static`. Both
/// arrive as `SharedString`s the caller already holds (a model field
/// built once in `build_model`/equivalent, not `format!`ed here) —
/// fix round 1: this function used to take `site: String`/`action:
/// Option<String>` and both `format!("tip-{site}")` and `title.into()`
/// itself fresh every call; every action id in this codebase is a
/// literal, so `Option<&'static str>` is the honest type and there is
/// nothing left to build here but a clone (a refcount bump, or an
/// inline-string copy for anything under `SmolStr`'s cap) of what the
/// caller already owns.
pub fn tip_with(
    selector: SharedString,
    title: SharedString,
    action: Option<&'static str>,
    detail: Option<SharedString>,
) -> impl Fn(&mut Window, &mut App) -> AnyView + 'static {
    move |window, cx| {
        let empty = Vec::new();
        let bindings = cx
            .try_global::<Chords>()
            .map(|c| c.0.as_slice())
            .unwrap_or(&empty);
        let model = TipModel::resolve(title.clone(), action, detail.clone(), bindings);
        Tooltip::element({
            let selector = selector.clone();
            move |_window, cx| render_tip(&model, selector.clone(), cx)
        })
        .build(window, cx)
    }
}

/// The content: title, then the chord as `key_chip`s (one per
/// keystroke of a sequence), then the detail line, muted. Chip colours
/// are the keybindings dialog's own (`muted_foreground` on `muted`).
/// Selectors: `{selector}` on the root, `{selector}-title` on the title,
/// `{selector}-chord-{ctrl+k}` on each chip — what the hover tests read.
pub(crate) fn render_tip(model: &TipModel, selector: SharedString, cx: &App) -> AnyElement {
    let theme = cx.theme();
    let chip_fg = theme.muted_foreground;
    let chip_bg = theme.muted;
    let chord_row = model.chord.as_ref().map(|keys| {
        let mut row = h_flex().gap_1().items_center();
        for ks in keys {
            let text = crate::palette::render_keystroke(ks);
            row = row.child(
                div()
                    .debug_selector({
                        let s = format!("{selector}-chord-{text}");
                        move || s.clone()
                    })
                    .child(crate::shell::keybindings_view::key_chip(
                        ks, chip_fg, chip_bg,
                    )),
            );
        }
        row
    });
    v_flex()
        .gap_1()
        .debug_selector({
            let s = selector.to_string();
            move || s.clone()
        })
        .child(
            h_flex()
                .gap_2()
                .items_center()
                .child(
                    div()
                        .text_color(theme.popover_foreground)
                        .debug_selector({
                            let s = format!("{selector}-title");
                            move || s.clone()
                        })
                        .child(model.title.clone()),
                )
                .when_some(chord_row, |el, row| el.child(row)),
        )
        .when_some(model.detail.clone(), |el, d| {
            el.child(div().text_xs().text_color(theme.muted_foreground).child(d))
        })
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::actions::ActionId;
    use crate::keymap::{Binding, Modifiers, parse_binding};
    use geode_core::config::Layer;

    fn binding(keys: &str, action: &str, layer: Layer, index: usize) -> Binding {
        Binding {
            keystrokes: parse_binding(keys, Modifiers::NONE).unwrap(),
            predicate: None,
            action: ActionId(action.to_string()),
            layer,
            index,
            context_source: None,
        }
    }

    #[test]
    fn chord_for_returns_the_effective_binding_for_an_action() {
        let bindings = vec![binding("ctrl+k", "palette::toggle", Layer::Builtin, 0)];
        let chord = chord_for(&bindings, "palette::toggle").expect("bound");
        assert_eq!(chord, parse_binding("ctrl+k", Modifiers::NONE).unwrap());
    }

    #[test]
    fn chord_for_follows_a_user_layer_rebind() {
        // The builtin says ctrl+k; the user layer rebinds to ctrl+space
        // and unbinds ctrl+k with a "none" shadow, exactly what
        // keymap_edit writes. The tooltip must show ctrl+space.
        let bindings = vec![
            binding("ctrl+k", "palette::toggle", Layer::Builtin, 0),
            binding("ctrl+k", "none", Layer::User, 1),
            binding("ctrl+space", "palette::toggle", Layer::User, 2),
        ];
        let chord = chord_for(&bindings, "palette::toggle").expect("bound");
        assert_eq!(chord, parse_binding("ctrl+space", Modifiers::NONE).unwrap());
    }

    #[test]
    fn chord_for_is_none_for_an_unbound_action() {
        let bindings = vec![binding("ctrl+k", "palette::toggle", Layer::Builtin, 0)];
        assert_eq!(chord_for(&bindings, "frame::as_of"), None);
    }

    #[test]
    fn resolve_carries_title_chord_and_detail() {
        let bindings = vec![binding("mod+t", "frame::as_of", Layer::Builtin, 0)];
        let m = TipModel::resolve(
            "As-of selector",
            Some("frame::as_of"),
            Some("live".into()),
            &bindings,
        );
        assert_eq!(m.title.as_ref(), "As-of selector");
        assert_eq!(
            m.chord,
            Some(parse_binding("mod+t", Modifiers::NONE).unwrap())
        );
        assert_eq!(m.detail.as_deref(), Some("live"));
    }

    #[test]
    fn resolve_without_an_action_has_no_chord() {
        let m = TipModel::resolve("Remove book", None, None, &[]);
        assert_eq!(m.chord, None);
        assert_eq!(m.detail, None);
    }
}
