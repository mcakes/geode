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

    #[test]
    fn missing_bindings_array_is_fine() {
        let d = doc(Layer::User, "config_version = 1\n");
        let (keymap, diags) = build_keymap(&[d], Modifiers::ALT, &registry());
        assert!(keymap.bindings().is_empty());
        assert!(diags.is_empty());
    }
}
