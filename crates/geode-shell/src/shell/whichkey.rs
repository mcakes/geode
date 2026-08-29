//! The which-key hint (Task 8, spec §3): while a keystroke sequence is
//! pending (`Matcher::pending()` non-empty — e.g. mid-way through a
//! desk/user-layer sequence like `g g`; the builtin keymap has no
//! sequences of its own anymore), show a small overlay listing every
//! keystroke that would continue
//! some binding from here, next to the title of what it would do.
//!
//! Split the same way `palette.rs` is: `continuations` is the pure core
//! (TDD'd without gpui, below) computing which keys continue the pending
//! sequence; `render`, at the bottom, is the only part that touches
//! `gpui`/`gpui_component`. Display-only: the overlay never steals focus or
//! changes key routing — `ShellView` keeps feeding keystrokes to
//! `self.matcher` exactly as it does today, this module only reads the
//! result. No delay timer (YAGNI until it annoys someone): the overlay
//! appears the same frame the matcher goes pending.

use crate::actions::{ActionId, ActionRegistry};
use crate::keymap::{KeyContext, Keymap, Keystroke, UNBOUND_ACTION};
use crate::palette::render_keystroke;
use std::collections::HashMap;

/// For every binding whose keystroke sequence strictly extends `pending`
/// and whose predicate passes against `stack`, the keystroke immediately
/// following `pending` paired with the action that binding resolves to.
///
/// Bindings are deduped by that next keystroke, keeping the *shortest*
/// binding when more than one shares it. That "shortest wins" outcome
/// rests on two different justifications depending on how short the
/// survivor is — this is not one rule mirroring `Matcher` throughout:
/// - **Exactly one keystroke past `pending`:** this genuinely mirrors
///   `Matcher` itself (its own "exact match beats longer candidate",
///   `matcher.rs` `exact_match_beats_longer_candidate`) — pressing that key
///   would fire this binding's action immediately rather than extend the
///   sequence further, so it's the *only* action `Matcher` could ever
///   resolve to for that key. No ambiguity, nothing conventional about it.
/// - **Two (or more) candidates all longer than one keystroke** — e.g.
///   `"ctrl+w h x"` (len 3) and `"ctrl+w h y z"` (len 4) both sharing next
///   keystroke `h` from `pending = ["ctrl+w"]`: here `Matcher` has *no*
///   analogous precedent — every one of them would leave `Matcher` merely
///   `Pending` at that key, with no opinion between them until further
///   keys arrive. Preferring the shortest here is a **deliberate display
///   convention** of this hint alone (shorter sequences are likelier to be
///   what the user is about to complete), pinned by
///   `both_longer_collision_keeps_the_shorter_sequence_as_a_convention`
///   below so a future change to it is a conscious one, not an accident of
///   `HashMap` iteration order.
///
/// Independently of shortest-wins: among bindings tied for the very same
/// length at this next keystroke, last-wins by layer-then-declaration
/// order — so a user-layer rebind (or unbind, via the `none` action) of
/// the same extended sequence shadows whatever a lower layer bound there.
///
/// Only after that resolution are entries whose *final* action is `none`
/// dropped: an unbound continuation must not be advertised as one, but an
/// unbind must still be able to shadow a real lower-layer binding on its
/// way to being dropped.
///
/// Sorted by rendered keystroke text (`render_keystroke`, shared with the
/// palette's own binding-text rendering rather than a third copy of it).
pub fn continuations(
    keymap: &Keymap,
    pending: &[Keystroke],
    stack: &[KeyContext],
) -> Vec<(Keystroke, ActionId)> {
    let mut next: HashMap<Keystroke, (usize, ActionId)> = HashMap::new();
    for binding in keymap.bindings() {
        let len = binding.keystrokes.len();
        if len <= pending.len() || !binding.keystrokes.starts_with(pending) {
            continue;
        }
        if binding.predicate.as_ref().is_some_and(|p| !p.eval(stack)) {
            continue;
        }
        let key = binding.keystrokes[pending.len()].clone();
        // Skip only when a strictly shorter binding already claims this
        // next keystroke; a tie (or a new shorter binding) overwrites.
        if next
            .get(&key)
            .is_some_and(|(existing_len, _)| *existing_len < len)
        {
            continue;
        }
        next.insert(key, (len, binding.action.clone()));
    }
    let mut result: Vec<(Keystroke, ActionId)> = next
        .into_iter()
        .filter(|(_, (_, action))| action.0 != UNBOUND_ACTION)
        .map(|(key, (_, action))| (key, action))
        .collect();
    result.sort_by_key(|(key, _)| render_keystroke(key));
    result
}

/// The row title for one continuation: the registry's own title for the
/// action, falling back to the bare action id when it isn't registered.
/// `build_keymap` already drops bindings to unknown actions (spec §10.1),
/// so this fallback shouldn't fire for a real keymap — but this is display
/// code, not a place to unwrap and panic on the rare stale-registry case.
fn title_for(registry: &ActionRegistry, action: &ActionId) -> String {
    registry
        .get(action)
        .map(|def| def.title.clone())
        .unwrap_or_else(|| action.0.clone())
}

// -- render ---------------------------------------------------------------

use gpui::prelude::*;
use gpui::{App, IntoElement, div, px};
use gpui_component::{ActiveTheme as _, h_flex, v_flex};

use crate::fonts;

/// Overlay panel width, in pixels.
const WIDTH: f32 = 220.0;
/// Gap kept from the right/bottom edges of the viewport / status bar.
const MARGIN: f32 = 8.0;

/// Render the which-key overlay: a small `popover`-toned panel anchored to
/// the bottom-right, sitting just above the status bar, listing each
/// continuation as `key → title`. Caller (`ShellView::render`) only calls
/// this when `matcher.pending()` is non-empty.
pub fn render(
    continuations: &[(Keystroke, ActionId)],
    registry: &ActionRegistry,
    viewport_width: f32,
    status_bar_height: f32,
    cx: &App,
) -> impl IntoElement {
    let theme = cx.theme();
    let width = WIDTH.min((viewport_width - 2.0 * MARGIN).max(120.0));

    let mut list = v_flex().w_full().gap_1();
    for (keystroke, action) in continuations {
        list = list.child(
            h_flex()
                .w_full()
                .justify_between()
                .gap_3()
                .child(
                    div()
                        .font_family(fonts::MONO)
                        .text_color(theme.muted_foreground)
                        .child(render_keystroke(keystroke)),
                )
                .child(div().child(title_for(registry, action))),
        );
    }

    // The panel div still paints (with test-hook debug_selector below) even
    // for an empty `continuations` — a matcher.pending()-non-empty state
    // should always carry at least one continuation in practice (a
    // `Matcher` only goes `Pending` when it already found a longer
    // candidate), so this stays a thin, honest wrapper rather than adding
    // a branch this crate's tests can't exercise for real.
    div()
        .absolute()
        .right(px(MARGIN))
        .bottom(px(status_bar_height + MARGIN))
        .w(px(width))
        .flex()
        .flex_col()
        .gap_1()
        .p_2()
        .bg(theme.popover)
        .text_color(theme.popover_foreground)
        .border_1()
        .border_color(theme.border)
        .rounded(px(8.))
        // Test-only hook (no-op outside test/test-support builds, same
        // pattern as the empty-workspace hint's "empty-hint" selector) so a
        // `#[gpui::test]` can confirm this overlay actually painted.
        .debug_selector(|| "whichkey-overlay".to_string())
        .child(list)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::actions::ActionDef;
    use crate::keymap::{Modifiers, build_keymap, parse_keystroke};
    use geode_core::config::{Layer, LayerDoc};

    fn registry(ids: &[&str]) -> ActionRegistry {
        let mut reg = ActionRegistry::default();
        for id in ids {
            reg.register(ActionDef {
                id: ActionId(id.to_string()),
                title: format!("Title: {id}"),
                category: "Test".to_string(),
            })
            .unwrap();
        }
        reg
    }

    fn keymap(docs: &[(Layer, &str)], ids: &[&str]) -> Keymap {
        let docs: Vec<LayerDoc> = docs
            .iter()
            .map(|(layer, text)| LayerDoc {
                layer: *layer,
                name: "keymap".to_string(),
                file: format!("{}/keymap.toml", layer.name()).into(),
                table: text.parse().unwrap(),
            })
            .collect();
        let (keymap, diags) = build_keymap(&docs, Modifiers::ALT, &registry(ids));
        assert!(diags.is_empty(), "{diags:?}");
        keymap
    }

    fn ks(s: &str) -> Keystroke {
        parse_keystroke(s, Modifiers::ALT).unwrap()
    }

    fn action(id: &str) -> ActionId {
        ActionId(id.to_string())
    }

    #[test]
    fn lists_every_binding_strictly_extending_pending() {
        let km = keymap(
            &[(
                Layer::Builtin,
                "[[bindings]]\n[bindings.keys]\n\"ctrl+w h\" = \"a::left\"\n\"ctrl+w l\" = \"a::right\"\n\"ctrl+v\" = \"a::split\"\n",
            )],
            &["a::left", "a::right", "a::split"],
        );
        let got = continuations(&km, &[ks("ctrl+w")], &[]);
        assert_eq!(
            got,
            vec![(ks("h"), action("a::left")), (ks("l"), action("a::right"))]
        );
    }

    #[test]
    fn excludes_bindings_that_do_not_extend_pending() {
        let km = keymap(
            &[(
                Layer::Builtin,
                "[[bindings]]\n[bindings.keys]\n\"ctrl+w h\" = \"a::left\"\n\"ctrl+v\" = \"a::split\"\n\"g\" = \"a::g\"\n",
            )],
            &["a::left", "a::split", "a::g"],
        );
        let got = continuations(&km, &[ks("ctrl+w")], &[]);
        assert_eq!(got, vec![(ks("h"), action("a::left"))]);
    }

    #[test]
    fn empty_pending_still_matches_length_one_bindings() {
        // Pure-core contract: `continuations` doesn't special-case an empty
        // `pending` — every binding "strictly extends" it. The caller
        // (`ShellView::render`) is what only invokes this while
        // `matcher.pending()` is non-empty. "g" and "g g" share the same
        // next keystroke ("g") from an empty pending; the exact one-key
        // binding wins (see `exact_binding_beats_a_longer_one_sharing_the_
        // same_next_key`) since pressing "g" alone fires it immediately.
        let km = keymap(
            &[(
                Layer::Builtin,
                "[[bindings]]\n[bindings.keys]\n\"g\" = \"a::g\"\n\"g g\" = \"a::gg\"\n",
            )],
            &["a::g", "a::gg"],
        );
        let got = continuations(&km, &[], &[]);
        assert_eq!(got, vec![(ks("g"), action("a::g"))]);
    }

    #[test]
    fn exact_binding_beats_a_longer_one_sharing_the_same_next_key() {
        // Mirrors `matcher.rs`'s `exact_match_beats_longer_candidate`:
        // pressing "g" would fire `a::g` immediately rather than extend
        // toward `a::gg`, so that's the action worth advertising for "g" —
        // regardless of which one is declared first.
        let km = keymap(
            &[(
                Layer::Builtin,
                "[[bindings]]\n[bindings.keys]\n\"g g\" = \"a::gg\"\n\"g\" = \"a::g\"\n",
            )],
            &["a::g", "a::gg"],
        );
        assert_eq!(
            continuations(&km, &[], &[]),
            vec![(ks("g"), action("a::g"))]
        );
    }

    #[test]
    fn both_longer_collision_keeps_the_shorter_sequence_as_a_convention() {
        // Unlike `exact_binding_beats_a_longer_one_sharing_the_same_next_
        // key`, *neither* binding here resolves at the next keystroke:
        // "ctrl+w h x" (len 3) and "ctrl+w h y z" (len 4) both extend past
        // it. `Matcher` has no precedent to mirror for this case — every
        // candidate would leave it merely `Pending` at "h", with no
        // opinion between them until further keys arrive. Preferring the
        // shorter one is this hint's own display convention (see
        // `continuations`'s doc comment); this test pins that choice so a
        // future change to it is deliberate, not an accident of `HashMap`
        // iteration order.
        let km = keymap(
            &[(
                Layer::Builtin,
                "[[bindings]]\n[bindings.keys]\n\"ctrl+w h x\" = \"a::short\"\n\"ctrl+w h y z\" = \"a::long\"\n",
            )],
            &["a::short", "a::long"],
        );
        assert_eq!(
            continuations(&km, &[ks("ctrl+w")], &[]),
            vec![(ks("h"), action("a::short"))]
        );
    }

    #[test]
    fn multi_key_pending_extends_from_where_it_left_off() {
        let km = keymap(
            &[(
                Layer::Builtin,
                "[[bindings]]\n[bindings.keys]\n\"g g x\" = \"a::deep\"\n\"g g y\" = \"a::deep2\"\n\"g h\" = \"a::shallow\"\n",
            )],
            &["a::deep", "a::deep2", "a::shallow"],
        );
        let got = continuations(&km, &[ks("g"), ks("g")], &[]);
        assert_eq!(
            got,
            vec![(ks("x"), action("a::deep")), (ks("y"), action("a::deep2"))]
        );
    }

    #[test]
    fn predicate_gates_continuations() {
        let km = keymap(
            &[(
                Layer::Builtin,
                "[[bindings]]\ncontext = \"blotter\"\n[bindings.keys]\n\"ctrl+w h\" = \"a::left\"\n",
            )],
            &["a::left"],
        );
        let workspace = vec![KeyContext::new("workspace")];
        assert!(continuations(&km, &[ks("ctrl+w")], &workspace).is_empty());
        let blotter = vec![KeyContext::new("workspace"), KeyContext::new("blotter")];
        assert_eq!(
            continuations(&km, &[ks("ctrl+w")], &blotter),
            vec![(ks("h"), action("a::left"))]
        );
    }

    #[test]
    fn dedup_last_wins_a_user_rebind_shadows_the_builtin() {
        let km = keymap(
            &[
                (
                    Layer::Builtin,
                    "[[bindings]]\n[bindings.keys]\n\"ctrl+w h\" = \"a::left\"\n",
                ),
                (
                    Layer::User,
                    "[[bindings]]\n[bindings.keys]\n\"ctrl+w h\" = \"a::other\"\n",
                ),
            ],
            &["a::left", "a::other"],
        );
        assert_eq!(
            continuations(&km, &[ks("ctrl+w")], &[]),
            vec![(ks("h"), action("a::other"))]
        );
    }

    #[test]
    fn dedup_last_wins_a_user_unbind_shadows_and_is_excluded() {
        let km = keymap(
            &[
                (
                    Layer::Builtin,
                    "[[bindings]]\n[bindings.keys]\n\"ctrl+w h\" = \"a::left\"\n\"ctrl+w l\" = \"a::right\"\n",
                ),
                (
                    Layer::User,
                    "[[bindings]]\n[bindings.keys]\n\"ctrl+w h\" = \"none\"\n",
                ),
            ],
            &["a::left", "a::right"],
        );
        // "h" was unbound by the user layer: it must not appear at all, not
        // even as an inert/disabled row.
        assert_eq!(
            continuations(&km, &[ks("ctrl+w")], &[]),
            vec![(ks("l"), action("a::right"))]
        );
    }

    #[test]
    fn none_action_declared_directly_is_excluded_without_a_shadow() {
        let km = keymap(
            &[(
                Layer::Builtin,
                "[[bindings]]\n[bindings.keys]\n\"ctrl+w h\" = \"none\"\n\"ctrl+w l\" = \"a::right\"\n",
            )],
            &["a::right"],
        );
        assert_eq!(
            continuations(&km, &[ks("ctrl+w")], &[]),
            vec![(ks("l"), action("a::right"))]
        );
    }

    #[test]
    fn sorted_by_rendered_keystroke_text() {
        let km = keymap(
            &[(
                Layer::Builtin,
                "[[bindings]]\n[bindings.keys]\n\"ctrl+w l\" = \"a::right\"\n\"ctrl+w shift+h\" = \"a::move_left\"\n\"ctrl+w h\" = \"a::left\"\n",
            )],
            &["a::right", "a::move_left", "a::left"],
        );
        let got = continuations(&km, &[ks("ctrl+w")], &[]);
        let rendered: Vec<String> = got.iter().map(|(k, _)| render_keystroke(k)).collect();
        let mut sorted = rendered.clone();
        sorted.sort();
        assert_eq!(rendered, sorted);
        assert_eq!(rendered, vec!["h", "l", "shift+h"]);
    }

    #[test]
    fn no_continuations_is_an_empty_vec() {
        let km = keymap(
            &[(
                Layer::Builtin,
                "[[bindings]]\n[bindings.keys]\n\"g\" = \"a::g\"\n",
            )],
            &["a::g"],
        );
        assert!(continuations(&km, &[ks("g")], &[]).is_empty());
    }

    #[test]
    fn title_for_falls_back_to_action_id_when_unregistered() {
        let reg = registry(&["a::known"]);
        assert_eq!(title_for(&reg, &action("a::known")), "Title: a::known");
        assert_eq!(title_for(&reg, &action("a::missing")), "a::missing");
    }
}
