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
/// Within one [bindings.keys] table, TOML key uniqueness is by spelling, so two spellings that normalize to the same sequence (e.g. "alt+h" and "mod+h" when mod=alt) can coexist; they are iterated alphabetically, so which wins is determined by spelling, not declaration order.
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
            let predicate = match entry.get("context") {
                None => None,
                Some(toml::Value::String(s)) => match parse_predicate(s) {
                    Ok(p) => Some(p),
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
            for (spec, action_value) in keys {
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
                });
                index += 1;
            }
        }
    }
    (Keymap { bindings }, diags)
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
