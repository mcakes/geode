use super::{Layer, LayerDoc};
use std::collections::BTreeMap;
use toml::{Table, Value};

/// A merged document with dotted-path provenance. Whole-object replacements
/// record provenance at the object's root; leaf lookups use that ancestor.
#[derive(Debug, Clone, Default)]
pub struct MergedDoc {
    pub value: Table,
    pub provenance: BTreeMap<String, Layer>,
}

/// Depth at which a higher layer replaces an entire named object.
/// Documents without an atomic depth merge tables recursively.
fn atomic_depth(doc_name: &str) -> Option<u32> {
    match doc_name {
        // Presentation tables replace whole objects by name, including column
        // order and hidden-column settings.
        "views"
        | "view_presentation"
        | "dataset_presentation"
        | "layouts"
        | "groupings"
        | "scopes"
        | "datasets"
        | "sources"
        | "egress"
        | "dimensions" => Some(1),
        // One complete definition per color name.
        "colors" => Some(1),
        // One complete definition per pricer view name.
        "pricer_views" => Some(1),
        // One complete override entry per name.
        "overrides" => Some(1),
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
        // A higher-layer view replaces the whole named object; omitted fields
        // do not inherit from the lower-layer definition.
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
    fn pricer_views_is_atomic_by_view_name() {
        let desk =
            LayerDoc::builtin("pricer_views", "[v]\ncolumns = [\"qty\", \"price\"]\n").unwrap();
        let mut user = LayerDoc::builtin("pricer_views", "[v]\ncolumns = [\"delta\"]\n").unwrap();
        user.layer = Layer::User;
        let merged = merge_docs("pricer_views", &[desk, user]);
        let cols = merged.value["v"]["columns"].as_array().unwrap();
        assert_eq!(
            cols.len(),
            1,
            "the user's view replaced the desk's whole: {cols:?}"
        );
        assert_eq!(merged.provenance.get("v"), Some(&Layer::User));
    }

    #[test]
    fn egress_is_atomic_by_target_name() {
        // A higher-layer egress target replaces the whole named object, like
        // `sources`: the user layer's target must not inherit the desk
        // layer's `documents` table when it names its own.
        let merged = merge_docs(
            "egress",
            &[
                doc(
                    Layer::Desk,
                    "egress",
                    "[sophis]\nadapter = \"demo_bus\"\n[sophis.documents]\ncvi_params = \"a\"\n",
                ),
                doc(Layer::User, "egress", "[sophis]\nadapter = \"other_bus\"\n"),
            ],
        );
        let sophis = merged.value["sophis"].as_table().unwrap();
        assert_eq!(sophis["adapter"].as_str(), Some("other_bus"));
        assert!(
            sophis.get("documents").is_none(),
            "atomic override must drop the desk-only documents table"
        );
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
