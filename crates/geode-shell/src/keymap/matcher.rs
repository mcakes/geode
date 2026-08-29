use super::{KeyContext, Keymap, Keystroke, UNBOUND_ACTION};
use crate::actions::ActionId;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MatchResult {
    Matched(ActionId),
    /// The keystrokes so far are a prefix of at least one binding; awaiting more.
    Pending,
    NoMatch,
}

/// Sequence-aware key matcher. One per focus target is unnecessary — the
/// shell holds one and feeds it the active context stack per press.
#[derive(Debug, Default)]
pub struct Matcher {
    pending: Vec<Keystroke>,
}

impl Matcher {
    pub fn press(
        &mut self,
        keymap: &Keymap,
        keystroke: Keystroke,
        stack: &[KeyContext],
    ) -> MatchResult {
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
            if binding.action.0 == UNBOUND_ACTION {
                return MatchResult::NoMatch;
            }
            return MatchResult::Matched(binding.action.clone());
        }
        if has_longer_candidate {
            return MatchResult::Pending;
        }
        self.pending.clear();
        MatchResult::NoMatch
    }

    pub fn pending(&self) -> &[Keystroke] {
        &self.pending
    }

    pub fn cancel(&mut self) {
        self.pending.clear();
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
            MatchResult::Matched(ActionId("a::left".into()))
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
            MatchResult::Matched(ActionId("a::right".into()))
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
            MatchResult::Matched(ActionId("a::top".into()))
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
            MatchResult::Matched(ActionId("a::x".into()))
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
            MatchResult::Matched(ActionId("a::g".into()))
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
            MatchResult::Matched(ActionId("b::down".into()))
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
}
