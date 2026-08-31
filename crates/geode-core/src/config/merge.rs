use super::{Layer, LayerDoc};
use std::collections::BTreeMap;
use toml::{Table, Value};

/// A doc after layer merging, with per-dotted-path provenance for
/// `geode config explain`-style tooling (spec §8: "layered config without
/// provenance is a support nightmare").
#[derive(Debug, Clone, Default)]
pub struct MergedDoc {
    pub value: Table,
    pub provenance: BTreeMap<String, Layer>,
}

/// Docs whose top-level entries are named objects overridden whole-object
/// by name (spec §8): merging inside a view/layout is clever but undebuggable.
fn atomic_depth(doc_name: &str) -> Option<u32> {
    match doc_name {
        "views" | "layouts" | "groupings" | "scopes" | "datasets" | "sources" | "dimensions" => {
            Some(1)
        }
        _ => None,
    }
}

/// Merge layer docs in slice order (callers pass Builtin → Desk → User).
pub fn merge_docs(name: &str, layered: &[LayerDoc]) -> MergedDoc {
    let mut out = MergedDoc::default();
    let atomic = atomic_depth(name);
    for doc in layered {
        merge_table(
            &mut out.value,
            &doc.table,
            doc.layer,
            &mut out.provenance,
            "",
            0,
            atomic,
        );
    }
    out
}

fn merge_table(
    dst: &mut Table,
    src: &Table,
    layer: Layer,
    prov: &mut BTreeMap<String, Layer>,
    path: &str,
    depth: u32,
    atomic: Option<u32>,
) {
    for (key, value) in src {
        let child_path = if path.is_empty() {
            key.clone()
        } else {
            format!("{path}.{key}")
        };
        let entry_depth = depth + 1;
        let replace_whole = atomic == Some(entry_depth);
        match (dst.get(key), value) {
            (Some(Value::Table(_)), Value::Table(s)) if !replace_whole => {
                // Both are tables and not at atomic boundary: recurse
                if let Some(Value::Table(d)) = dst.get_mut(key) {
                    merge_table(d, s, layer, prov, &child_path, entry_depth, atomic);
                }
            }
            (None, Value::Table(s)) if !replace_whole => {
                // Key doesn't exist, create table and recurse to record provenance on leaves
                dst.insert(key.clone(), Value::Table(Table::new()));
                if let Some(Value::Table(d)) = dst.get_mut(key) {
                    merge_table(d, s, layer, prov, &child_path, entry_depth, atomic);
                }
            }
            _ => {
                // All other cases: wholesale replace (scalars, arrays, type mismatches, atomic)
                dst.insert(key.clone(), value.clone());
                record_provenance(prov, &child_path, layer);
            }
        }
    }
}

fn record_provenance(prov: &mut BTreeMap<String, Layer>, path: &str, layer: Layer) {
    let prefix = format!("{path}.");
    prov.retain(|p, _| p != path && !p.starts_with(&prefix));
    prov.insert(path.to_string(), layer);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn doc(layer: Layer, name: &str, text: &str) -> LayerDoc {
        LayerDoc {
            layer,
            name: name.to_string(),
            file: format!("{}/{name}.toml", layer.name()).into(),
            table: text.parse().unwrap(),
        }
    }

    #[test]
    fn later_layer_scalar_wins() {
        let merged = merge_docs(
            "app",
            &[
                doc(Layer::Desk, "app", "[keymap]\nmod = \"alt\"\n"),
                doc(Layer::User, "app", "[keymap]\nmod = \"ctrl\"\n"),
            ],
        );
        assert_eq!(merged.value["keymap"]["mod"].as_str(), Some("ctrl"));
    }

    #[test]
    fn deep_merge_preserves_sibling_keys() {
        let merged = merge_docs(
            "app",
            &[
                doc(
                    Layer::Desk,
                    "app",
                    "[keymap]\nmod = \"alt\"\n[theme]\nname = \"dark\"\n",
                ),
                doc(Layer::User, "app", "[keymap]\nmod = \"ctrl\"\n"),
            ],
        );
        assert_eq!(merged.value["theme"]["name"].as_str(), Some("dark"));
        assert_eq!(merged.value["keymap"]["mod"].as_str(), Some("ctrl"));
    }

    #[test]
    fn atomic_doc_replaces_named_object_whole() {
        // "views" is an atomic doc: a later layer's view of the same name
        // replaces the earlier one entirely — no field-level merge (spec §8).
        let merged = merge_docs(
            "views",
            &[
                doc(
                    Layer::Desk,
                    "views",
                    "[risk]\ndataset = \"risk\"\ncolumns = [\"npv\", \"delta\"]\n",
                ),
                doc(Layer::User, "views", "[risk]\ndataset = \"risk\"\n"),
            ],
        );
        let risk = merged.value["risk"].as_table().unwrap();
        assert!(
            risk.get("columns").is_none(),
            "atomic override must drop desk-only fields"
        );
    }

    #[test]
    fn non_atomic_arrays_are_replaced_not_appended() {
        let merged = merge_docs(
            "app",
            &[
                doc(Layer::Desk, "app", "recent = [1, 2]\n"),
                doc(Layer::User, "app", "recent = [3]\n"),
            ],
        );
        assert_eq!(merged.value["recent"].as_array().unwrap().len(), 1);
    }

    #[test]
    fn provenance_tracks_winning_layer() {
        let merged = merge_docs(
            "app",
            &[
                doc(
                    Layer::Builtin,
                    "app",
                    "[keymap]\nmod = \"alt\"\n[theme]\nname = \"dark\"\n",
                ),
                doc(Layer::User, "app", "[keymap]\nmod = \"ctrl\"\n"),
            ],
        );
        assert_eq!(merged.provenance.get("keymap.mod"), Some(&Layer::User));
        assert_eq!(merged.provenance.get("theme.name"), Some(&Layer::Builtin));
    }

    #[test]
    fn later_table_replaces_earlier_scalar_of_same_key() {
        let merged = merge_docs(
            "app",
            &[
                doc(Layer::Builtin, "app", "keymap = \"flat\"\n"),
                doc(Layer::User, "app", "[keymap]\nmod = \"ctrl\"\n"),
            ],
        );
        assert_eq!(merged.value["keymap"]["mod"].as_str(), Some("ctrl"));
        assert_eq!(merged.provenance.get("keymap"), Some(&Layer::User));
    }
}
