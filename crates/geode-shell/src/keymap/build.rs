use crate::actions::{ActionId, ActionRegistry};
use geode_core::config::{Diagnostic, Layer, LayerDoc};

use super::{Keystroke, Modifiers, Predicate, parse_binding, parse_predicate};

/// Binding an action to this id unbinds the key: a higher layer silences a
/// lower layer's binding.
pub const UNBOUND_ACTION: &str = "none";

#[derive(Debug, Clone)]
pub struct Binding {
    pub keystrokes: Vec<Keystroke>,
    pub predicate: Option<Predicate>,
    pub action: ActionId,
    pub layer: Layer,
    /// Definition order across all input documents. Informational only: the
    /// matcher resolves ties by list order, not by this field.
    pub index: usize,
    /// Original context spelling, retained so persistence can find the source
    /// entry by exact string equality. `None` denotes an absent context.
    pub context_source: Option<String>,
    /// The raw key spelling from the source doc (`"mod+h"`, not the
    /// parsed keystrokes rendered back). A user-layer removal is
    /// `keys.remove(spelling)` on the very document this was read from,
    /// so it must name the key the way the file does — `render_binding`
    /// of the parsed keystrokes says `alt+h` for a file that wrote
    /// `mod+h`, and that removal finds nothing.
    pub key_source: String,
}

/// One user-layer entry that overrides `action` — the unit
/// [`user_overrides_for`] returns and `keymap_edit::apply_reset` removes.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct UserOverride {
    /// The `[[bindings]]` entry's raw `context`, `None` for the no-context
    /// entry — what `keymap_edit::keys_table_for` matches on.
    pub context_source: Option<String>,
    /// The key's spelling in the user document ([`Binding::key_source`]).
    pub key: String,
}

#[derive(Debug, Default)]
pub struct Keymap {
    bindings: Vec<Binding>,
}

impl Keymap {
    pub fn bindings(&self) -> &[Binding] {
        &self.bindings
    }
}

/// Compile unmerged keymap documents in the supplied order into a flat list.
/// Callers supply builtin, module, desk, then user documents. Invalid entries
/// produce errors and are skipped; unknown actions produce warnings and are
/// skipped. The special action `"none"` needs no registry entry.
///
/// Entries retain their array order. Within each keys table, keys are sorted by
/// source spelling: aliases that normalize to the same sequence therefore tie
/// by spelling, independently of TOML's preserved declaration order.
pub fn build_keymap(
    layered: &[LayerDoc],
    mod_alias: Modifiers,
    registry: &ActionRegistry,
) -> (Keymap, Vec<Diagnostic>) {
    let mut bindings = Vec::new();
    let mut diags = Vec::new();
    let mut index = 0usize;
    for doc in layered {
        let entries = match doc.table.get("bindings") {
            None => continue,
            Some(toml::Value::Array(entries)) => entries,
            Some(_) => {
                diags.push(Diagnostic::error(
                    doc.layer,
                    doc.file.clone(),
                    "'bindings' must be an array of tables ([[bindings]])",
                ));
                continue;
            }
        };
        for entry in entries {
            let Some(entry) = entry.as_table() else {
                diags.push(Diagnostic::error(
                    doc.layer,
                    doc.file.clone(),
                    "each [[bindings]] entry must be a table",
                ));
                continue;
            };
            let (predicate, context_source) = match entry.get("context") {
                None => (None, None),
                Some(toml::Value::String(s)) => match parse_predicate(s) {
                    Ok(p) => (Some(p), Some(s.clone())),
                    Err(e) => {
                        diags.push(Diagnostic::error(
                            doc.layer,
                            doc.file.clone(),
                            format!("invalid context '{s}': {e}"),
                        ));
                        continue;
                    }
                },
                Some(_) => {
                    diags.push(Diagnostic::error(
                        doc.layer,
                        doc.file.clone(),
                        "'context' must be a string",
                    ));
                    continue;
                }
            };
            let Some(toml::Value::Table(keys)) = entry.get("keys") else {
                diags.push(Diagnostic::error(
                    doc.layer,
                    doc.file.clone(),
                    "[[bindings]] entry is missing its [bindings.keys] table",
                ));
                continue;
            };
            // Sort alias spellings explicitly: TOML preserves declaration order,
            // but the matcher must resolve equivalent spellings alphabetically.
            let mut sorted_keys: Vec<(&String, &toml::Value)> = keys.iter().collect();
            sorted_keys.sort_by_key(|(a, _)| *a);
            for (spec, action_value) in sorted_keys {
                let Some(action_str) = action_value.as_str() else {
                    diags.push(Diagnostic::error(
                        doc.layer,
                        doc.file.clone(),
                        format!("action for '{spec}' must be a string"),
                    ));
                    continue;
                };
                let keystrokes = match parse_binding(spec, mod_alias) {
                    Ok(k) => k,
                    Err(e) => {
                        diags.push(Diagnostic::error(
                            doc.layer,
                            doc.file.clone(),
                            format!("invalid binding '{spec}': {e}"),
                        ));
                        continue;
                    }
                };
                let mut action = ActionId(action_str.to_string());
                if let Some(current) = registry.renamed(&action) {
                    diags.push(Diagnostic::warning(
                        doc.layer,
                        doc.file.clone(),
                        format!(
                            "'{spec}' names the old action id '{action_str}'; bound to \
                             '{current}' — rename it in the file"
                        ),
                    ));
                    action = current.clone();
                }
                if action_str != UNBOUND_ACTION && !registry.contains(&action) {
                    diags.push(Diagnostic::warning(
                        doc.layer,
                        doc.file.clone(),
                        format!(
                            "'{spec}' bound to unknown action '{action_str}' (binding skipped)"
                        ),
                    ));
                    continue;
                }
                bindings.push(Binding {
                    keystrokes,
                    predicate: predicate.clone(),
                    action,
                    layer: doc.layer,
                    index,
                    context_source: context_source.clone(),
                    key_source: spec.clone(),
                });
                index += 1;
            }
        }
    }
    (Keymap { bindings }, diags)
}

/// Find the latest binding for `action` that is not shadowed by a later
/// binding of any action, including `"none"`, on the same sequence.
///
/// This is a display approximation without a live context stack. A later entry
/// shadows a candidate only when its context is absent or its raw context string
/// is identical. Equivalent or overlapping predicates written differently can
/// therefore remain visible here even when dispatch chooses another binding.
/// [`super::Matcher`] evaluates predicates against the actual stack at dispatch.
pub fn effective_binding<'a>(bindings: &'a [Binding], action: &ActionId) -> Option<&'a Binding> {
    bindings
        .iter()
        .enumerate()
        .rev()
        .filter(|(_, b)| b.action == *action)
        .find(|(i, b)| !is_shadowed(bindings, *i, b))
        .map(|(_, b)| b)
}

/// The effective binding for `action` among the builtin and desk layers alone:
/// what the action falls back to once every user override is gone. Shadowing
/// uses [`effective_binding`]'s source-string approximation.
pub fn effective_lower_binding<'a>(
    bindings: &'a [Binding],
    action: &ActionId,
) -> Option<&'a Binding> {
    bindings
        .iter()
        .enumerate()
        .rev()
        .filter(|(_, b)| b.layer != Layer::User && b.action == *action)
        .find(|(i, b)| {
            !bindings[i + 1..]
                .iter()
                .any(|later| later.layer != Layer::User && shadows(later, b))
        })
        .map(|(_, b)| b)
}

/// Collect user entries to remove when resetting `action`: bindings naming
/// the action, plus `"none"` entries covering its live lower-layer bindings.
///
/// A lower-layer binding already shadowed by a later lower-layer entry does not
/// make an unbind belong to its old action. This prevents resetting one action
/// from restoring a key that the desk assigned to another. Context coverage uses
/// [`effective_binding`]'s source-string approximation. Results retain original
/// key spellings and are sorted and deduplicated by context and key.
///
/// An orphan unbind also belongs to `action`: a user `"none"` that shadows no
/// lower-layer binding at all under that approximation, on the keys of one of
/// `action`'s live lower-layer bindings. A dialog rebind of a module binding
/// that was later retired into a shared action (a renamed id now shipped once
/// under a shared context) leaves exactly this: the shipped key moved to
/// another context string, so the shadow no longer matches it textually, yet
/// it still silences that key wherever its own context holds. Missing it kept
/// the key dead after `r`. An orphan on a key several actions share is
/// collected for each of them; removing it restores lower layers only.
pub fn user_overrides_for(bindings: &[Binding], action: &ActionId) -> Vec<UserOverride> {
    let lower: Vec<&Binding> = bindings.iter().filter(|b| b.layer != Layer::User).collect();
    let live = |i: usize, l: &Binding| !lower[i + 1..].iter().any(|later| shadows(later, l));
    let mut out: Vec<UserOverride> = Vec::new();
    for b in bindings.iter().filter(|b| b.layer == Layer::User) {
        let is_override = if b.action == *action {
            true
        } else if b.action.0 == UNBOUND_ACTION {
            let covers_live = lower
                .iter()
                .enumerate()
                .any(|(i, l)| l.action == *action && shadows(b, l) && live(i, l));
            let orphan = !lower.iter().any(|l| shadows(b, l))
                && lower.iter().enumerate().any(|(i, l)| {
                    l.action == *action && l.keystrokes == b.keystrokes && live(i, l)
                });
            covers_live || orphan
        } else {
            false
        };
        if is_override {
            out.push(UserOverride {
                context_source: b.context_source.clone(),
                key: b.key_source.clone(),
            });
        }
    }
    out.sort();
    out.dedup();
    out
}

/// Whether a later binding shadows this candidate under the display approximation.
fn is_shadowed(bindings: &[Binding], index: usize, candidate: &Binding) -> bool {
    bindings[index + 1..]
        .iter()
        .any(|later| shadows(later, candidate))
}

/// Compare exact sequences and raw contexts for display/reset resolution.
/// An absent later context covers every candidate context; otherwise the context
/// strings must be equal. This does not evaluate logical predicate implication.
fn shadows(later: &Binding, candidate: &Binding) -> bool {
    later.keystrokes == candidate.keystrokes
        && (later.context_source.is_none() || later.context_source == candidate.context_source)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::actions::ActionDef;
    use geode_core::config::Severity;

    fn registry() -> ActionRegistry {
        let mut reg = ActionRegistry::default();
        for id in ["workspace::focus_left", "workspace::focus_right"] {
            reg.register(ActionDef {
                id: ActionId(id.to_string()),
                title: id.to_string(),
                category: "Test".to_string(),
            })
            .unwrap();
        }
        reg
    }

    fn doc(layer: Layer, text: &str) -> LayerDoc {
        LayerDoc {
            layer,
            name: "keymap".to_string(),
            file: format!("{}/keymap.toml", layer.name()).into(),
            table: text.parse().unwrap(),
        }
    }

    #[test]
    fn collects_bindings_in_layer_then_document_order() {
        let builtin = doc(
            Layer::Builtin,
            "[[bindings]]\ncontext = \"workspace\"\n[bindings.keys]\n\"mod+h\" = \"workspace::focus_left\"\n",
        );
        let user = doc(
            Layer::User,
            "[[bindings]]\n[bindings.keys]\n\"mod+l\" = \"workspace::focus_right\"\n",
        );
        let (keymap, diags) = build_keymap(&[builtin, user], Modifiers::ALT, &registry());
        assert!(diags.is_empty(), "{diags:?}");
        let bindings = keymap.bindings();
        assert_eq!(bindings.len(), 2);
        assert_eq!(bindings[0].layer, Layer::Builtin);
        assert!(bindings[0].predicate.is_some());
        assert_eq!(bindings[1].layer, Layer::User);
        assert!(bindings[1].predicate.is_none());
        assert!(bindings[0].index < bindings[1].index);
    }

    /// A user override written before an action was renamed still binds —
    /// to the current id, so persistence and display name the new one — and
    /// a warning names both ids.
    #[test]
    fn a_binding_naming_a_renamed_action_binds_the_current_id_with_a_warning() {
        let mut reg = registry();
        reg.register_rename("workspace::focus_west", "workspace::focus_left")
            .unwrap();
        let user = doc(
            Layer::User,
            "[[bindings]]\ncontext = \"workspace\"\n[bindings.keys]\n\"mod+y\" = \"workspace::focus_west\"\n",
        );
        let (keymap, diags) = build_keymap(&[user], Modifiers::ALT, &reg);
        let bindings = keymap.bindings();
        assert_eq!(bindings.len(), 1, "{diags:?}");
        assert_eq!(bindings[0].action.0, "workspace::focus_left");
        assert_eq!(bindings[0].key_source, "mod+y");
        assert_eq!(diags.len(), 1, "{diags:?}");
        assert_eq!(diags[0].severity, Severity::Warning);
        assert!(
            diags[0].message.contains("workspace::focus_west")
                && diags[0].message.contains("workspace::focus_left"),
            "{}",
            diags[0].message
        );
    }

    #[test]
    fn ambiguous_aliases_in_one_keys_table_resolve_by_spelling_not_file_position() {
        // The aliases normalize to the same key. Reverse alphabetical source
        // order proves resolution is independent of TOML declaration order.
        let d = doc(
            Layer::User,
            "[[bindings]]\n[bindings.keys]\n\"mod+h\" = \"workspace::focus_right\"\n\"alt+h\" = \"workspace::focus_left\"\n",
        );
        let (keymap, diags) = build_keymap(&[d], Modifiers::ALT, &registry());
        assert!(diags.is_empty(), "{diags:?}");
        let bindings = keymap.bindings();
        assert_eq!(bindings.len(), 2);
        // Sorted by spelling: "alt+h" pushed first, "mod+h" pushed last —
        // the matcher keeps the last pushed on an exact tie, so "mod+h"'s
        // action must win regardless of which key the file names first.
        assert_eq!(bindings[0].action.0, "workspace::focus_left");
        assert_eq!(bindings[1].action.0, "workspace::focus_right");
    }

    #[test]
    fn context_source_carries_the_raw_pre_parse_context_string() {
        let d = doc(
            Layer::User,
            "[[bindings]]\ncontext = \"workspace && !modal\"\n[bindings.keys]\n\"mod+h\" = \"workspace::focus_left\"\n\n\
             [[bindings]]\n[bindings.keys]\n\"mod+l\" = \"workspace::focus_right\"\n",
        );
        let (keymap, diags) = build_keymap(&[d], Modifiers::ALT, &registry());
        assert!(diags.is_empty(), "{diags:?}");
        let bindings = keymap.bindings();
        assert_eq!(
            bindings[0].context_source.as_deref(),
            Some("workspace && !modal"),
            "context_source must be the exact source spelling, not a re-rendering of the \
             compiled predicate"
        );
        assert_eq!(
            bindings[1].context_source, None,
            "the no-context entry must carry context_source: None"
        );
    }

    #[test]
    fn unknown_action_is_warning_and_skipped() {
        let d = doc(
            Layer::User,
            "[[bindings]]\n[bindings.keys]\n\"mod+x\" = \"nope::nothing\"\n",
        );
        let (keymap, diags) = build_keymap(&[d], Modifiers::ALT, &registry());
        assert!(keymap.bindings().is_empty());
        assert_eq!(diags.len(), 1);
        assert_eq!(diags[0].severity, Severity::Warning);
        assert!(diags[0].message.contains("nope::nothing"));
    }

    #[test]
    fn none_action_is_accepted_without_registration() {
        let d = doc(
            Layer::User,
            "[[bindings]]\n[bindings.keys]\n\"mod+h\" = \"none\"\n",
        );
        let (keymap, diags) = build_keymap(&[d], Modifiers::ALT, &registry());
        assert!(diags.is_empty(), "{diags:?}");
        assert_eq!(keymap.bindings()[0].action.0, "none");
    }

    #[test]
    fn bad_context_is_error_and_entry_skipped() {
        let d = doc(
            Layer::User,
            "[[bindings]]\ncontext = \"a &&\"\n[bindings.keys]\n\"mod+h\" = \"workspace::focus_left\"\n",
        );
        let (keymap, diags) = build_keymap(&[d], Modifiers::ALT, &registry());
        assert!(keymap.bindings().is_empty());
        assert_eq!(diags[0].severity, Severity::Error);
    }

    #[test]
    fn bad_keystroke_is_error_and_that_key_skipped() {
        let d = doc(
            Layer::User,
            "[[bindings]]\n[bindings.keys]\n\"ctrl+\" = \"workspace::focus_left\"\n\"mod+l\" = \"workspace::focus_right\"\n",
        );
        let (keymap, diags) = build_keymap(&[d], Modifiers::ALT, &registry());
        assert_eq!(keymap.bindings().len(), 1);
        assert_eq!(diags.len(), 1);
        assert_eq!(diags[0].severity, Severity::Error);
    }

    /// A user key no keyboard sends is an error naming the spelling that
    /// works, and only that binding is skipped: it is never bound to a key
    /// that cannot fire.
    #[test]
    fn a_user_key_no_keyboard_sends_is_an_error_naming_the_fix() {
        let d = doc(
            Layer::User,
            "[[bindings]]\n[bindings.keys]\n\"alt-h\" = \"workspace::focus_left\"\n\"z L\" = \"workspace::focus_left\"\n\"mod+l\" = \"workspace::focus_right\"\n",
        );
        let (keymap, diags) = build_keymap(&[d], Modifiers::ALT, &registry());
        assert_eq!(keymap.bindings().len(), 1, "only mod+l binds");
        assert_eq!(keymap.bindings()[0].key_source, "mod+l");
        assert_eq!(diags.len(), 2, "{diags:?}");
        assert!(
            diags
                .iter()
                .all(|d| d.severity == Severity::Error && d.layer == Some(Layer::User)),
            "{diags:?}"
        );
        assert!(
            diags.iter().any(|d| d.message.contains("alt+h")),
            "{diags:?}"
        );
        assert!(
            diags.iter().any(|d| d.message.contains("shift+l")),
            "{diags:?}"
        );
    }

    fn focus_left() -> ActionId {
        ActionId("workspace::focus_left".to_string())
    }

    fn overrides(docs: &[LayerDoc], action: &ActionId) -> Vec<(Option<String>, String)> {
        let (keymap, diags) = build_keymap(docs, Modifiers::ALT, &registry());
        assert!(diags.is_empty(), "{diags:?}");
        user_overrides_for(keymap.bindings(), action)
            .into_iter()
            .map(|o| (o.context_source, o.key))
            .collect()
    }

    #[test]
    fn a_rebind_of_a_builtin_yields_both_halves_of_the_pair() {
        // Reset must remove both the replacement binding and the shadow over
        // the original key, or the lower-layer binding remains disabled.
        let builtin = doc(
            Layer::Builtin,
            "[[bindings]]\ncontext = \"workspace\"\n[bindings.keys]\n\"mod+h\" = \"workspace::focus_left\"\n",
        );
        let user = doc(
            Layer::User,
            "[[bindings]]\ncontext = \"workspace\"\n[bindings.keys]\n\"mod+j\" = \"workspace::focus_left\"\n\"mod+h\" = \"none\"\n",
        );
        let mut got = overrides(&[builtin, user], &focus_left());
        got.sort();
        assert_eq!(
            got,
            vec![
                (Some("workspace".to_string()), "mod+h".to_string()),
                (Some("workspace".to_string()), "mod+j".to_string()),
            ]
        );
    }

    #[test]
    fn a_bare_shadow_over_a_builtin_is_an_override_of_that_action() {
        // A `d` on a builtin row: the row derives as unbound, but the
        // `"none"` entry IS the user's override and reset must lift it.
        let builtin = doc(
            Layer::Builtin,
            "[[bindings]]\n[bindings.keys]\n\"mod+h\" = \"workspace::focus_left\"\n",
        );
        let user = doc(
            Layer::User,
            "[[bindings]]\n[bindings.keys]\n\"mod+h\" = \"none\"\n",
        );
        assert_eq!(
            overrides(&[builtin, user], &focus_left()),
            vec![(None, "mod+h".to_string())]
        );
    }

    #[test]
    fn a_shadow_over_another_actions_key_is_not_this_actions_override() {
        let builtin = doc(
            Layer::Builtin,
            "[[bindings]]\n[bindings.keys]\n\"mod+h\" = \"workspace::focus_left\"\n\"mod+l\" = \"workspace::focus_right\"\n",
        );
        let user = doc(
            Layer::User,
            "[[bindings]]\n[bindings.keys]\n\"mod+l\" = \"none\"\n",
        );
        assert!(overrides(&[builtin, user], &focus_left()).is_empty());
    }

    #[test]
    fn a_shadow_in_a_different_context_is_not_this_actions_override() {
        // Different context strings are treated as independent by display/reset
        // resolution, even though predicates can overlap during dispatch. The
        // shadow silences the lower `blotter` binding on its key, so it is that
        // binding's action's override, not an orphan this action adopts.
        let builtin = doc(
            Layer::Builtin,
            "[[bindings]]\ncontext = \"workspace\"\n[bindings.keys]\n\"mod+h\" = \"workspace::focus_left\"\n\n\
             [[bindings]]\ncontext = \"blotter\"\n[bindings.keys]\n\"mod+h\" = \"workspace::focus_right\"\n",
        );
        let user = doc(
            Layer::User,
            "[[bindings]]\ncontext = \"blotter\"\n[bindings.keys]\n\"mod+h\" = \"none\"\n",
        );
        assert!(overrides(&[builtin.clone(), user.clone()], &focus_left()).is_empty());
        let focus_right = ActionId("workspace::focus_right".to_string());
        assert_eq!(
            overrides(&[builtin, user], &focus_right),
            vec![(Some("blotter".to_string()), "mod+h".to_string())]
        );
    }

    /// A shadow whose context covers no lower binding at all is an orphan:
    /// what a dialog rebind left over a module key since retired into a
    /// shared action. It still silences that key wherever its context
    /// holds, so resetting the action the key now reaches removes it.
    #[test]
    fn an_orphan_shadow_on_a_live_key_of_the_action_is_its_override() {
        let builtin = doc(
            Layer::Builtin,
            "[[bindings]]\ncontext = \"workspace\"\n[bindings.keys]\n\"mod+h\" = \"workspace::focus_left\"\n",
        );
        let user = doc(
            Layer::User,
            "[[bindings]]\ncontext = \"blotter\"\n[bindings.keys]\n\"mod+h\" = \"none\"\n\"mod+y\" = \"none\"\n",
        );
        assert_eq!(
            overrides(&[builtin, user], &focus_left()),
            vec![(Some("blotter".to_string()), "mod+h".to_string())],
            "the orphan on focus_left's key is its override; the one on an \
             unbound key is nobody's"
        );
    }

    /// The case the orphan rule exists for, on the shipped keymap: a rebind
    /// of the retired `blotter::down` under the blotter's visual context
    /// wrote `n` and a `j` shadow there. `j` now ships once under the grid
    /// context, so the shadow shadows nothing textually, yet it is Motion:
    /// down's override beside the renamed `n`.
    #[test]
    fn an_old_module_rebind_of_a_retired_motion_is_all_the_motions_override() {
        let mut reg = ActionRegistry::default();
        crate::defaults::register_builtin_actions(&mut reg);
        reg.register_rename("blotter::down", "motion::down")
            .unwrap();
        let builtin = LayerDoc::builtin("keymap", crate::defaults::BUILTIN_KEYMAP).unwrap();
        let user = doc(
            Layer::User,
            "[[bindings]]\ncontext = \"blotter && mode == visual\"\n[bindings.keys]\n\
             \"n\" = \"blotter::down\"\n\"j\" = \"none\"\n",
        );
        let (keymap, _) = build_keymap(&[builtin, user], Modifiers::ALT, &reg);
        let got: Vec<(Option<String>, String)> =
            user_overrides_for(keymap.bindings(), &ActionId("motion::down".to_string()))
                .into_iter()
                .map(|o| (o.context_source, o.key))
                .collect();
        let ctx = Some("blotter && mode == visual".to_string());
        assert_eq!(
            got,
            vec![(ctx.clone(), "j".to_string()), (ctx, "n".to_string())]
        );
    }

    #[test]
    fn a_shadow_over_a_key_a_lower_layer_repurposed_is_the_repurposers_override() {
        // builtin: mod+h = focus_left; desk: mod+h = focus_right (the desk
        // re-purposed the key); user: mod+h = "none" (a `d` on Focus
        // right). The shadow silences focus_right — the live lower
        // binding — so it is focus_right's override and NOT focus_left's:
        // lifting it from the Focus left row would resurrect focus_right
        // and leave focus_left as unbound as before.
        let builtin = doc(
            Layer::Builtin,
            "[[bindings]]\ncontext = \"workspace\"\n[bindings.keys]\n\"mod+h\" = \"workspace::focus_left\"\n",
        );
        let desk = doc(
            Layer::Desk,
            "[[bindings]]\ncontext = \"workspace\"\n[bindings.keys]\n\"mod+h\" = \"workspace::focus_right\"\n",
        );
        let user = doc(
            Layer::User,
            "[[bindings]]\ncontext = \"workspace\"\n[bindings.keys]\n\"mod+h\" = \"none\"\n",
        );
        let docs = [builtin, desk, user];
        assert!(overrides(&docs, &focus_left()).is_empty());
        assert_eq!(
            overrides(&docs, &ActionId("workspace::focus_right".to_string())),
            vec![(Some("workspace".to_string()), "mod+h".to_string())]
        );
    }

    #[test]
    fn a_user_binding_for_another_action_is_not_this_actions_override() {
        let user = doc(
            Layer::User,
            "[[bindings]]\n[bindings.keys]\n\"mod+h\" = \"workspace::focus_left\"\n\"mod+l\" = \"workspace::focus_right\"\n",
        );
        assert_eq!(
            overrides(&[user], &focus_left()),
            vec![(None, "mod+h".to_string())]
        );
    }

    #[test]
    fn a_desk_override_is_never_the_users_to_remove() {
        let builtin = doc(
            Layer::Builtin,
            "[[bindings]]\n[bindings.keys]\n\"mod+h\" = \"workspace::focus_left\"\n",
        );
        let desk = doc(
            Layer::Desk,
            "[[bindings]]\n[bindings.keys]\n\"mod+j\" = \"workspace::focus_left\"\n\"mod+h\" = \"none\"\n",
        );
        assert!(overrides(&[builtin, desk], &focus_left()).is_empty());
    }

    #[test]
    fn the_override_carries_the_files_own_key_spelling() {
        // The removal is `keys.remove(spelling)` on the user document, so
        // the spelling must be the file's, not a re-rendering of the
        // parsed keystroke (`alt+h` here, which would miss `mod+h`).
        let user = doc(
            Layer::User,
            "[[bindings]]\n[bindings.keys]\n\"mod+h\" = \"workspace::focus_left\"\n",
        );
        assert_eq!(
            overrides(&[user], &focus_left()),
            vec![(None, "mod+h".to_string())]
        );
    }

    #[test]
    fn missing_bindings_array_is_fine() {
        let d = doc(Layer::User, "config_version = 1\n");
        let (keymap, diags) = build_keymap(&[d], Modifiers::ALT, &registry());
        assert!(keymap.bindings().is_empty());
        assert!(diags.is_empty());
    }
}
