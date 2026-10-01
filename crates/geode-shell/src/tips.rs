//! Tooltips show a title, the action's chord from the live keymap, and an
//! optional detail line. The hover closure resolves [`TipModel`] when the
//! tooltip opens, so rebinding an action updates its displayed chord.
//!
//! Call sites retain dynamic strings as `SharedString`s or pass literals
//! through [`tip`]. Attaching a tooltip performs no string formatting in
//! this module; GPUI still allocates its event and tooltip closures. The
//! shown tooltip can render again each frame, so debug selectors remain
//! lazy too. Hover timing uses GPUI's default delay.
//!
//! The shell publishes [`Chords`] at startup and after keymap rebuilds.
//! Modules read this global to label their own controls without accessing
//! `ShellView`.

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

/// Build a tooltip closure from a literal title, full selector (such as
/// `"tip-sidebar-profile"`), and optional action id. Resolve the model on
/// hover. If a test fixture has no [`Chords`] global, omit the chord.
///
/// Use `SharedString::new_static` for both literals so attachment avoids a
/// string allocation regardless of their length. Callers with retained
/// dynamic titles or selectors use [`tip_with`].
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

/// Build a tooltip closure from a retained title and full selector.
/// Callers prepare dynamic strings when their model changes and pass clones
/// here, avoiding formatting or string allocation in the render path.
/// Action ids remain static; the model is resolved on hover as in [`tip`].
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

/// Build a tooltip closure whose chord is a fixed key the surface owns rather than a
/// keymap action, such as a modal's Escape. `key` uses the footer hints' keystroke
/// spelling; an unparsable spelling shows the title without a chord.
pub fn tip_key(
    selector: &'static str,
    title: &'static str,
    key: &'static str,
) -> impl Fn(&mut Window, &mut App) -> AnyView + 'static {
    let title = SharedString::new_static(title);
    let selector = SharedString::new_static(selector);
    move |window, cx| {
        let model = TipModel {
            title: title.clone(),
            chord: crate::keymap::parse_keystroke(key, crate::keymap::Modifiers::NONE)
                .ok()
                .map(|ks| vec![ks]),
            detail: None,
        };
        Tooltip::element({
            let selector = selector.clone();
            move |_window, cx| render_tip(&model, selector.clone(), cx)
        })
        .build(window, cx)
    }
}

/// The content: title, then the chord as `Kbd` chips (one per
/// keystroke of a sequence), then the detail line, muted.
/// Selectors: `{selector}` on the root, `{selector}-title` on the title,
/// `{selector}-chord-{ctrl+k}` on each chip, `{selector}-detail` on the
/// detail line — what the hover tests read. A backtick-quoted key in the
/// detail paints as a chip (`shell::kbd::marked`).
pub(crate) fn render_tip(model: &TipModel, selector: SharedString, cx: &App) -> AnyElement {
    let theme = cx.theme();
    let chord_row = model.chord.as_ref().map(|keys| {
        let mut row = h_flex().gap_1().items_center();
        for ks in keys {
            let text = crate::palette::render_keystroke(ks);
            row = row.child(
                div()
                    .debug_selector({
                        let selector = selector.clone();
                        move || format!("{selector}-chord-{text}")
                    })
                    .child(crate::shell::kbd::chip(ks)),
            );
        }
        row
    });
    v_flex()
        .gap_1()
        .debug_selector({
            let selector = selector.clone();
            move || selector.to_string()
        })
        .child(
            h_flex()
                .gap_2()
                .items_center()
                .child(
                    div()
                        .text_color(theme.popover_foreground)
                        .debug_selector({
                            let selector = selector.clone();
                            move || format!("{selector}-title")
                        })
                        .child(model.title.clone()),
                )
                .when_some(chord_row, |el, row| el.child(row)),
        )
        .when_some(model.detail.clone(), |el, d| {
            // A detail naming keys in backticks paints them as chips.
            let line = div()
                .text_xs()
                .text_color(theme.muted_foreground)
                .debug_selector({
                    let selector = selector.clone();
                    move || format!("{selector}-detail")
                });
            el.child(if d.contains('`') {
                line.child(crate::shell::kbd::marked(&d))
            } else {
                line.child(d)
            })
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
            key_source: keys.to_string(),
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
        // A bare unbind — no replacement key — never carries the real
        // action id (its own action is "none"), so a naive "last binding
        // whose action matches" search skips it entirely and reports the
        // builtin as if still live. Only a search that also checks
        // whether a later binding SHADOWS the candidate's keystroke (this
        // one does: same "ctrl+k", no context) sees the unbind and
        // reports unbound.
        let unbound_only = vec![
            binding("ctrl+k", "palette::toggle", Layer::Builtin, 0),
            binding("ctrl+k", "none", Layer::User, 1),
        ];
        assert_eq!(chord_for(&unbound_only, "palette::toggle"), None);

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
