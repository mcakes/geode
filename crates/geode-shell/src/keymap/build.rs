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
    /// Global definition order across all layers. Informational: the matcher resolves ties by iteration order of Keymap::bindings(), which this mirrors — do not reorder bindings and rely on index alone.
    pub index: usize,
    /// The raw `context` string from the source doc, pre-parse (`None` for
    /// the no-context entry) — kept alongside the compiled `predicate`
    /// because a compiled `Predicate` has no `Display`/round-trip back to
    /// the exact source spelling. This is the seam the keybinding dialog
    /// (Part B) uses to build a `keymap_edit::Rebind::context` that will
    /// exactly re-match the `[[bindings]]` entry a given effective binding
    /// actually came from — a `Predicate` alone can't do that.
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

/// Compile keymap docs (unmerged, in Builtin → Desk → User order) into a
/// flat binding list. Bad entries are skipped with a diagnostic — a typo in
/// a user keymap must never take down the keymap (spec §10.1).
/// Within one [bindings.keys] table, TOML key uniqueness is by spelling, so two spellings that normalize to the same sequence (e.g. "alt+h" and "mod+h" when mod=alt) can coexist; they are iterated alphabetically, so which wins is determined by spelling, not declaration order. This is sorted explicitly below rather than relied on from `toml::Table`'s own iteration order: `geode-core`'s `preserve_order` feature (Phase 4 §3.3, for schema column declaration order) is workspace-wide by Cargo feature unification, so every crate's `toml::Table` — this one included — iterates in file order, not sorted order, unless a consumer sorts for itself.
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
            // Sorted explicitly by spelling (see the doc comment above):
            // `toml::Table` iterates in file order under `preserve_order`,
            // and the ambiguous-alias tie-break this loop's push order
            // decides (`matcher.rs` keeps the *last* pushed binding) must
            // stay keyed on spec spelling, not on where a spec happens to
            // sit in the source file.
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
                let action = ActionId(action_str.to_string());
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

/// Resolve `action`'s effective binding within `bindings` (`keymap.
/// bindings()`'s own layer-then-declaration order), or `None` if unbound.
///
/// Scans candidates whose `action` matches, **most recently declared
/// first**; a candidate only counts as effective if no *later* binding in
/// the full list — declared after it, any action, including `"none"` —
/// shares its exact keystroke sequence AND carries a context that would
/// apply whenever the candidate's own does. A shadowed candidate is
/// skipped in favor of an earlier one for the same action (which may in
/// turn be shadowed by something else); if every candidate is shadowed (or
/// there are none), the action is unbound. This mirrors the real
/// `Matcher::press`'s own keystroke-keyed last-wins rule (spec §3.4): for
/// one keystroke, the *last* matching entry across the whole document
/// stack wins, regardless of which action it names — a plain "last binding
/// for this action id" search (the pre-review-round version of this
/// function) missed exactly this: a bare `"mod+h" = "none"` unbind (no
/// replacement key) never carries the real action id, so an action-id-only
/// search skips it and reports the old binding as if still live.
///
/// **Context approximation, stated honestly**: the real `Matcher` decides
/// "would this later binding actually apply" by evaluating a compiled
/// `Predicate` against a live context stack; this free function has no
/// such stack (there is no notion of "the current UI context" for a
/// dialog listing every action at once), so it approximates with the
/// *source spelling* of `context_source` instead: a later same-keystroke
/// binding shadows the candidate when its `context_source` is `None` (a
/// no-context entry is always active, so it always shadows) or is
/// string-equal to the candidate's own (the common real case — a rebind
/// shadowing its own prior entry within the same context, `keymap_edit`'s
/// own documented assumption). This is exact for every shape this crate's
/// own tooling ever writes. It can only drift from the real predicate
/// evaluation for a hand-written keymap pairing two *different but
/// overlapping* context strings on the same keystroke (e.g. `"workspace"`
/// and `"workspace && !modal"`) — string comparison would under-mark that
/// as not-shadowing even though the predicates do overlap at runtime.
/// Accepted as a documented approximation for a display-only resolution,
/// not the authoritative dispatch path (`Matcher::press` remains that).
pub fn effective_binding<'a>(bindings: &'a [Binding], action: &ActionId) -> Option<&'a Binding> {
    bindings
        .iter()
        .enumerate()
        .rev()
        .filter(|(_, b)| b.action == *action)
        .find(|(i, b)| !is_shadowed(bindings, *i, b))
        .map(|(_, b)| b)
}

/// Every user-layer entry that overrides `action` — what a reset of that
/// action removes so the layers beneath show through again. Two kinds:
///
/// 1. a user binding whose action IS `action` (a rebind's new key, or a
///    binding the user added themselves);
/// 2. a user `"none"` shadow ([`UNBOUND_ACTION`]) on a keystroke that a
///    lower layer binds to `action`, under a context that would apply
///    whenever that lower binding's does — the same context-equality
///    approximation [`effective_binding`] states (a no-context shadow
///    silences everything; a contexted one only its own string).
///
/// A rebind of a builtin writes both kinds at once (`keymap_edit::
/// apply_rebind`: the new key, then the `"none"` shadow over the old), and
/// a reset that removed only the first left the action *unbound* rather
/// than restored — the defect this function exists to close. Desk-layer
/// entries are never returned: the app only ever writes the user layer.
///
/// Sorted by `(context_source, key)`, deduplicated, so the same shadow
/// covering two lower bindings is one removal.
pub fn user_overrides_for(bindings: &[Binding], action: &ActionId) -> Vec<UserOverride> {
    let mut out: Vec<UserOverride> = Vec::new();
    for b in bindings.iter().filter(|b| b.layer == Layer::User) {
        let is_override = if b.action == *action {
            true
        } else if b.action.0 == UNBOUND_ACTION {
            bindings.iter().any(|lower| {
                lower.layer != Layer::User
                    && lower.action == *action
                    && lower.keystrokes == b.keystrokes
                    && (b.context_source.is_none() || b.context_source == lower.context_source)
            })
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

/// True if some binding declared AFTER `bindings[index]` (`candidate`)
/// shares its exact keystroke sequence and carries a context that would
/// apply whenever `candidate`'s own would — see [`effective_binding`]'s
/// doc comment for the full reasoning and the stated context-equality
/// approximation.
fn is_shadowed(bindings: &[Binding], index: usize, candidate: &Binding) -> bool {
    bindings[index + 1..].iter().any(|later| {
        later.keystrokes == candidate.keystrokes
            && (later.context_source.is_none() || later.context_source == candidate.context_source)
    })
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

    #[test]
    fn ambiguous_aliases_in_one_keys_table_resolve_by_spelling_not_file_position() {
        // "mod+h" (with mod=alt) and "alt+h" normalize to the same
        // keystroke, so both are legal keys of one [bindings.keys] table
        // (spec: TOML key uniqueness is by spelling). Declared in file
        // order mod+h, then alt+h — the *opposite* of alphabetical order —
        // so a regression back to raw `toml::Table` iteration order
        // (file order, under `preserve_order`) would push alt+h last and
        // flip which action wins.
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
        // `apply_rebind` on a builtin `mod+h` → `mod+j` writes the new
        // binding AND a `"none"` shadow over the old key. Both are the
        // user's override of this action; removing only the first (what
        // `r` did before) leaves the action unbound instead of reset.
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
        // The shadow only silences the builtin where its context applies;
        // a shadow under another context string never reaches it, so it
        // is not an override of this action (the same context-equality
        // approximation `effective_binding` states).
        let builtin = doc(
            Layer::Builtin,
            "[[bindings]]\ncontext = \"workspace\"\n[bindings.keys]\n\"mod+h\" = \"workspace::focus_left\"\n",
        );
        let user = doc(
            Layer::User,
            "[[bindings]]\ncontext = \"blotter\"\n[bindings.keys]\n\"mod+h\" = \"none\"\n",
        );
        assert!(overrides(&[builtin, user], &focus_left()).is_empty());
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
