use super::context::COUNTS;
use super::{KeyContext, Keymap, Keystroke, Modifiers, UNBOUND_ACTION};
use crate::actions::ActionId;
use gpui::SharedString;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MatchResult {
    Matched {
        action: ActionId,
        /// The count prefix typed before the binding, if any.
        count: Option<u32>,
    },
    /// The keystrokes so far are a prefix of at least one binding — or a
    /// count is being typed — awaiting more.
    Pending,
    NoMatch,
}

/// Maximum count prefix; further digits saturate at this value.
pub const MAX_COUNT: u32 = 9999;

/// Sequence and count state for the shell's active context stack.
///
/// The caller supplies the current contexts on every press. An exact match fires
/// immediately, even if a longer binding shares its prefix. A failed continuation
/// clears both sequence and count without retrying the last key as a fresh start.
/// There is no timeout in this engine; callers cancel pending input explicitly.
#[derive(Debug, Default)]
pub struct Matcher {
    pending: Vec<Keystroke>,
    count: Option<u32>,
    /// `count` as status-bar text, rebuilt only by [`Self::set_count`].
    count_label: Option<SharedString>,
}

impl Matcher {
    pub fn press(
        &mut self,
        keymap: &Keymap,
        keystroke: Keystroke,
        stack: &[KeyContext],
    ) -> MatchResult {
        // A bare digit with nothing pending, under a counting context,
        // is a count digit — except a leading `0`, which vim keeps as a
        // motion. Once a sequence has begun, digits are keys again.
        if self.pending.is_empty()
            && keystroke.mods == Modifiers::NONE
            && stack.last().is_some_and(|c| c.has_flag(COUNTS))
            && let Some(digit) = count_digit(&keystroke.key)
            && (digit != 0 || self.count.is_some())
        {
            let so_far = self.count.unwrap_or(0);
            self.set_count(Some(
                so_far
                    .saturating_mul(10)
                    .saturating_add(digit)
                    .min(MAX_COUNT),
            ));
            return MatchResult::Pending;
        }

        self.pending.push(keystroke);
        let mut exact: Option<&super::Binding> = None;
        let mut has_longer_candidate = false;
        for binding in keymap.bindings() {
            if binding.predicate.as_ref().is_some_and(|p| !p.eval(stack)) {
                continue;
            }
            if binding.keystrokes == self.pending {
                // Bindings are in layer-then-definition order; keep the last.
                exact = Some(binding);
            } else if binding.keystrokes.len() > self.pending.len()
                && binding.keystrokes.starts_with(&self.pending)
            {
                has_longer_candidate = true;
            }
        }
        if let Some(binding) = exact {
            self.pending.clear();
            let count = self.count;
            self.set_count(None);
            if binding.action.0 == UNBOUND_ACTION {
                return MatchResult::NoMatch;
            }
            return MatchResult::Matched {
                action: binding.action.clone(),
                count,
            };
        }
        if has_longer_candidate {
            return MatchResult::Pending;
        }
        self.pending.clear();
        self.set_count(None);
        MatchResult::NoMatch
    }

    pub fn pending(&self) -> &[Keystroke] {
        &self.pending
    }

    /// The count typed so far, while one is in flight.
    pub fn count(&self) -> Option<u32> {
        self.count
    }

    /// The count in flight as status-bar text.
    pub fn count_label(&self) -> Option<&SharedString> {
        self.count_label.as_ref()
    }

    /// The one writer of `count`, keeping its painted label in step.
    fn set_count(&mut self, count: Option<u32>) {
        if self.count != count {
            self.count = count;
            self.count_label = count.map(|n| SharedString::from(n.to_string()));
        }
    }

    pub fn cancel(&mut self) {
        self.pending.clear();
        self.set_count(None);
    }
}

fn count_digit(key: &str) -> Option<u32> {
    let mut chars = key.chars();
    match (chars.next(), chars.next()) {
        (Some(c), None) => c.to_digit(10),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::actions::{ActionDef, ActionRegistry};
    use crate::keymap::{Modifiers, build_keymap, parse_keystroke};
    use geode_core::config::{Layer, LayerDoc};

    fn registry(ids: &[&str]) -> ActionRegistry {
        let mut reg = ActionRegistry::default();
        for id in ids {
            reg.register(ActionDef {
                id: ActionId(id.to_string()),
                title: id.to_string(),
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

    fn ws() -> Vec<KeyContext> {
        vec![KeyContext::new("workspace")]
    }

    fn counting() -> Vec<KeyContext> {
        vec![
            KeyContext::new("workspace"),
            KeyContext::new("blotter").pair("mode", "normal").counts(),
        ]
    }

    fn km_counts() -> Keymap {
        keymap(
            &[(
                Layer::Builtin,
                "[[bindings]]\ncontext = \"blotter\"\n[bindings.keys]\n\"j\" = \"b::down\"\n\"g g\" = \"b::top\"\n\"0\" = \"b::first_col\"\n",
            )],
            &["b::down", "b::top", "b::first_col"],
        )
    }

    #[test]
    fn simple_match() {
        let km = keymap(
            &[(
                Layer::Builtin,
                "[[bindings]]\ncontext = \"workspace\"\n[bindings.keys]\n\"mod+h\" = \"a::left\"\n",
            )],
            &["a::left"],
        );
        let mut m = Matcher::default();
        assert_eq!(
            m.press(&km, ks("mod+h"), &ws()),
            MatchResult::Matched {
                action: ActionId("a::left".into()),
                count: None
            }
        );
        assert!(m.pending().is_empty());
    }

    #[test]
    fn later_layer_wins() {
        let km = keymap(
            &[
                (
                    Layer::Builtin,
                    "[[bindings]]\n[bindings.keys]\n\"mod+h\" = \"a::left\"\n",
                ),
                (
                    Layer::User,
                    "[[bindings]]\n[bindings.keys]\n\"mod+h\" = \"a::right\"\n",
                ),
            ],
            &["a::left", "a::right"],
        );
        let mut m = Matcher::default();
        assert_eq!(
            m.press(&km, ks("mod+h"), &ws()),
            MatchResult::Matched {
                action: ActionId("a::right".into()),
                count: None
            }
        );
    }

    #[test]
    fn unbind_swallows_key() {
        let km = keymap(
            &[
                (
                    Layer::Builtin,
                    "[[bindings]]\n[bindings.keys]\n\"mod+h\" = \"a::left\"\n",
                ),
                (
                    Layer::User,
                    "[[bindings]]\n[bindings.keys]\n\"mod+h\" = \"none\"\n",
                ),
            ],
            &["a::left"],
        );
        let mut m = Matcher::default();
        assert_eq!(m.press(&km, ks("mod+h"), &ws()), MatchResult::NoMatch);
        assert!(m.pending().is_empty());
    }

    #[test]
    fn sequence_pending_then_match() {
        let km = keymap(
            &[(
                Layer::Builtin,
                "[[bindings]]\n[bindings.keys]\n\"g g\" = \"a::top\"\n",
            )],
            &["a::top"],
        );
        let mut m = Matcher::default();
        assert_eq!(m.press(&km, ks("g"), &ws()), MatchResult::Pending);
        assert_eq!(m.pending().len(), 1);
        assert_eq!(
            m.press(&km, ks("g"), &ws()),
            MatchResult::Matched {
                action: ActionId("a::top".into()),
                count: None
            }
        );
        assert!(m.pending().is_empty());
    }

    #[test]
    fn sequence_dead_end_clears_without_retry() {
        let km = keymap(
            &[(
                Layer::Builtin,
                "[[bindings]]\n[bindings.keys]\n\"g g\" = \"a::top\"\n\"x\" = \"a::x\"\n",
            )],
            &["a::top", "a::x"],
        );
        let mut m = Matcher::default();
        assert_eq!(m.press(&km, ks("g"), &ws()), MatchResult::Pending);
        // "g x" is a dead end; the "x" is not retried as a fresh start.
        assert_eq!(m.press(&km, ks("x"), &ws()), MatchResult::NoMatch);
        assert!(m.pending().is_empty());
        // But a fresh "x" now matches.
        assert_eq!(
            m.press(&km, ks("x"), &ws()),
            MatchResult::Matched {
                action: ActionId("a::x".into()),
                count: None
            }
        );
    }

    #[test]
    fn exact_match_beats_longer_candidate() {
        let km = keymap(
            &[(
                Layer::Builtin,
                "[[bindings]]\n[bindings.keys]\n\"g\" = \"a::g\"\n\"g g\" = \"a::gg\"\n",
            )],
            &["a::g", "a::gg"],
        );
        let mut m = Matcher::default();
        assert_eq!(
            m.press(&km, ks("g"), &ws()),
            MatchResult::Matched {
                action: ActionId("a::g".into()),
                count: None
            }
        );
    }

    #[test]
    fn context_gates_bindings() {
        let km = keymap(
            &[(
                Layer::Builtin,
                "[[bindings]]\ncontext = \"blotter\"\n[bindings.keys]\n\"j\" = \"b::down\"\n",
            )],
            &["b::down"],
        );
        let mut m = Matcher::default();
        assert_eq!(m.press(&km, ks("j"), &ws()), MatchResult::NoMatch);
        let blotter = vec![KeyContext::new("workspace"), KeyContext::new("blotter")];
        assert_eq!(
            m.press(&km, ks("j"), &blotter),
            MatchResult::Matched {
                action: ActionId("b::down".into()),
                count: None
            }
        );
    }

    #[test]
    fn cancel_clears_pending() {
        let km = keymap(
            &[(
                Layer::Builtin,
                "[[bindings]]\n[bindings.keys]\n\"g g\" = \"a::top\"\n",
            )],
            &["a::top"],
        );
        let mut m = Matcher::default();
        assert_eq!(m.press(&km, ks("g"), &ws()), MatchResult::Pending);
        m.cancel();
        assert!(m.pending().is_empty());
        assert_eq!(m.press(&km, ks("g"), &ws()), MatchResult::Pending);
    }

    #[test]
    fn digits_accumulate_under_a_counting_context_and_ride_the_action() {
        let km = km_counts();
        let mut m = Matcher::default();
        assert_eq!(m.press(&km, ks("1"), &counting()), MatchResult::Pending);
        assert_eq!(m.count(), Some(1));
        assert_eq!(m.press(&km, ks("2"), &counting()), MatchResult::Pending);
        assert_eq!(m.count(), Some(12));
        assert_eq!(
            m.press(&km, ks("j"), &counting()),
            MatchResult::Matched {
                action: ActionId("b::down".into()),
                count: Some(12)
            }
        );
        assert_eq!(m.count(), None, "consumed by the action");
        assert!(m.pending().is_empty());
    }

    #[test]
    fn digits_are_ordinary_keys_outside_a_counting_context() {
        let km = km_counts();
        let mut m = Matcher::default();
        assert_eq!(m.press(&km, ks("5"), &ws()), MatchResult::NoMatch);
        assert_eq!(m.count(), None);
        // The innermost context decides: a counting frame below a
        // non-counting one does not count.
        let stack = vec![
            KeyContext::new("blotter").counts(),
            KeyContext::new("palette"),
        ];
        assert_eq!(m.press(&km, ks("5"), &stack), MatchResult::NoMatch);
    }

    #[test]
    fn a_leading_zero_is_a_key_and_a_later_zero_is_a_digit() {
        // vim: `0` is a motion unless a count has begun.
        let km = km_counts();
        let mut m = Matcher::default();
        assert_eq!(
            m.press(&km, ks("0"), &counting()),
            MatchResult::Matched {
                action: ActionId("b::first_col".into()),
                count: None
            }
        );
        assert_eq!(m.press(&km, ks("1"), &counting()), MatchResult::Pending);
        assert_eq!(m.press(&km, ks("0"), &counting()), MatchResult::Pending);
        assert_eq!(m.count(), Some(10));
        assert_eq!(
            m.press(&km, ks("j"), &counting()),
            MatchResult::Matched {
                action: ActionId("b::down".into()),
                count: Some(10)
            }
        );
    }

    #[test]
    fn a_count_prefix_prepares_its_label_once() {
        let km = km_counts();
        let mut m = Matcher::default();
        assert!(m.count_label().is_none());
        m.press(&km, ks("1"), &counting());
        assert_eq!(m.count_label().map(|l| l.as_ref()), Some("1"));
        m.press(&km, ks("2"), &counting());
        // Held on the matcher: a read borrows the prepared text. (Short
        // `SharedString`s are inline, so no pointer identity is asserted.)
        let label = m.count_label().cloned().expect("a count in flight");
        assert_eq!(label.as_ref(), "12");
        m.press(&km, ks("j"), &counting());
        assert!(m.count_label().is_none(), "consumed with the count");
    }

    #[test]
    fn a_count_survives_a_pending_sequence_and_dies_with_a_dead_end() {
        let km = km_counts();
        let mut m = Matcher::default();
        assert_eq!(m.press(&km, ks("3"), &counting()), MatchResult::Pending);
        assert_eq!(m.press(&km, ks("g"), &counting()), MatchResult::Pending);
        assert_eq!(m.count(), Some(3), "still counting through the sequence");
        assert_eq!(
            m.press(&km, ks("g"), &counting()),
            MatchResult::Matched {
                action: ActionId("b::top".into()),
                count: Some(3)
            }
        );

        assert_eq!(m.press(&km, ks("4"), &counting()), MatchResult::Pending);
        assert_eq!(m.press(&km, ks("x"), &counting()), MatchResult::NoMatch);
        assert_eq!(m.count(), None, "a dead end clears the count");
    }

    #[test]
    fn a_digit_inside_a_pending_sequence_is_a_key_not_a_count() {
        // `g 1` is not `1g`: once a sequence has begun, digits are keys.
        let km = km_counts();
        let mut m = Matcher::default();
        assert_eq!(m.press(&km, ks("g"), &counting()), MatchResult::Pending);
        assert_eq!(m.press(&km, ks("1"), &counting()), MatchResult::NoMatch);
        assert_eq!(m.count(), None);
    }

    #[test]
    fn escape_cancel_and_a_modified_digit() {
        let km = km_counts();
        let mut m = Matcher::default();
        assert_eq!(m.press(&km, ks("7"), &counting()), MatchResult::Pending);
        assert_eq!(
            m.press(&km, ks("escape"), &counting()),
            MatchResult::NoMatch
        );
        assert_eq!(m.count(), None);

        assert_eq!(m.press(&km, ks("7"), &counting()), MatchResult::Pending);
        m.cancel();
        assert_eq!(m.count(), None);

        // ctrl+1 is a chord, never a count digit.
        assert_eq!(
            m.press(&km, ks("ctrl+1"), &counting()),
            MatchResult::NoMatch
        );
        assert_eq!(m.count(), None);
    }

    #[test]
    fn the_count_is_capped() {
        let km = km_counts();
        let mut m = Matcher::default();
        for _ in 0..8 {
            assert_eq!(m.press(&km, ks("9"), &counting()), MatchResult::Pending);
        }
        assert_eq!(m.count(), Some(MAX_COUNT));
    }
}
