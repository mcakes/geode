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
