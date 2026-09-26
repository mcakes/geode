//! Display-only hints for a pending keymap sequence. The shell shows this
//! panel while the matcher has pending keystrokes; it neither takes focus nor
//! changes routing. Continuations are computed from active predicates and the
//! compiled keymap. No delay timer is used.

use super::scale;
use crate::actions::{ActionId, ActionRegistry};
use crate::keymap::{KeyContext, Keymap, Keystroke, UNBOUND_ACTION};
use crate::palette::render_keystroke;
use std::collections::HashMap;

/// Return one representative action per next key after `pending`, restricted
/// to bindings whose predicates match the supplied context stack.
///
/// Prefer the shortest extension, then the last equal-length binding in compiled
/// order. A one-key extension mirrors the matcher's immediate exact resolution.
/// Longer alternatives share a display convention: the shown action may require
/// additional keys, and other tails sharing that next key are not listed.
///
/// Resolve ties before dropping `none`, so an unbind can hide a lower-layer hint.
/// Sort results by rendered key spelling. An empty prefix is accepted by this
/// helper; the shell controls when the hint is visible.
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
    // Cached: `render_keystroke` returns an owned `String`, and `sort_by_key`
    // calls its key function O(n log n) times — on every frame a chord prefix
    // is held.
    result.sort_by_cached_key(|(key, _)| render_keystroke(key));
    result
}

/// Use a registered action title, falling back to its ID when registration is absent.
fn title_for(registry: &ActionRegistry, action: &ActionId) -> String {
    registry
        .get(action)
        .map(|def| def.title.clone())
        .unwrap_or_else(|| action.0.clone())
}

// -- render ---------------------------------------------------------------

use gpui::prelude::*;
use gpui::{App, IntoElement, Pixels, div, px};
use gpui_component::{ActiveTheme as _, h_flex, v_flex};

use crate::fonts;

/// Overlay panel width, in pixels.
const WIDTH: f32 = 220.0;
/// Gap kept from the right/bottom edges of the viewport / status bar.
const MARGIN: f32 = 8.0;

/// Render continuation keys and action titles above the status bar, with an
/// optional pending count. The caller shows this only for a nonempty sequence;
/// a count alone does not make it visible.
pub fn render(
    continuations: &[(Keystroke, ActionId)],
    count: Option<u32>,
    registry: &ActionRegistry,
    viewport_width: f32,
    status_bar_height: f32,
    rem_size: Pixels,
    cx: &App,
) -> impl IntoElement {
    let theme = cx.theme();
    // Width and margin on the rem scale (`shell::scale`); the viewport
    // clamp stays in window pixels.
    let margin = scale::design_px(MARGIN, rem_size);
    let width = scale::design_px(WIDTH, rem_size).min((viewport_width - 2.0 * margin).max(120.0));

    let mut list = v_flex().w_full().gap_1();
    if let Some(count) = count {
        list = list.child(
            div()
                .text_color(theme.muted_foreground)
                .child(format!("count {count}")),
        );
    }
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

    // Keep an empty panel measurable in tests. Overlay visibility depends on
    // pending matcher input, not the number of displayable continuations.
    div()
        .absolute()
        .right(px(margin))
        .bottom(px(status_bar_height + margin))
        .w(px(width))
        .flex()
        .flex_col()
        .gap_1()
        .p_2()
        .bg(theme.popover)
        .text_color(theme.popover_foreground)
        .border_1()
        .border_color(theme.border)
        .rounded(theme.radius_lg)
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
                "[[bindings]]\n[bindings.keys]\n\"ctrl+w h\" = \"a::left\"\n\"ctrl+w l\" = \"a::right\"\n\"ctrl+t\" = \"a::other\"\n",
            )],
            &["a::left", "a::right", "a::other"],
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
                "[[bindings]]\n[bindings.keys]\n\"ctrl+w h\" = \"a::left\"\n\"ctrl+t\" = \"a::other\"\n\"g\" = \"a::g\"\n",
            )],
            &["a::left", "a::other", "a::g"],
        );
        let got = continuations(&km, &[ks("ctrl+w")], &[]);
        assert_eq!(got, vec![(ks("h"), action("a::left"))]);
    }

    #[test]
    fn empty_pending_still_matches_length_one_bindings() {
        // An empty prefix includes all bindings. When they share a next key,
        // a one-key binding takes precedence over a longer sequence.
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
        // Neither candidate resolves at the next key. The shorter sequence is
        // a representative display choice; dispatch still waits for further input.
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
