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
        | "dimensions"
        | "expressions"
        | "watchlists"
        | "vol_views" => Some(1),
        // One complete definition per color name.
        "colors" => Some(1),
        // A dimension's values merge per value; one value's entry (a color
        // name or an inline `{ hue }` / `{ token }` table) is replaced whole,
        // so a user hue never deep-merges into a desk token.
        "value_colors" => Some(2),
        // One complete definition per pricer view name.
        "pricer_views" => Some(1),
        // One complete definition per pricer template name.
        "pricer_templates" => Some(1),
        // One complete override entry per name.
        "overrides" => Some(1),
        // One complete definition per market-data panel name: a partial
        // override must not inherit a lower layer's columns and formats.
        "panels" => Some(1),
        // One complete shape per scenario tile kind: a partial override
        // must not inherit a lower layer's columns, value or diff.
        "scenarios" => Some(1),
        _ => None,
    }
}

/// Documents whose top-level key order a higher layer may change. Inserting
/// into the merged table keeps a key where the lowest defining layer put it,
/// so a user file listing two shipped tables the other way round would leave
/// the merged order as shipped. Here the slots a layer's keys already hold
/// are refilled in that layer's order instead: keys the layer does not
/// define stay where they are, and keys new to the document follow at the
/// end. Vol views alone, whose tables are the watchlist's tabs, picked by
/// digit in document order, and whose dialog reorders them by writing the
/// user layer in the new order. Every other document keeps each key where
/// the lowest defining layer put it.
fn order_follows_layers(doc_name: &str) -> bool {
    doc_name == "vol_views"
}

/// Merge layer docs in slice order (callers pass Builtin → Desk → User).
pub fn merge_docs(name: &str, layered: &[LayerDoc]) -> MergedDoc {
    let mut out = MergedDoc::default();
    let atomic = atomic_depth(name);
    let reorders = order_follows_layers(name);
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
        if reorders {
            refill_slots(&mut out.value, &doc.table);
        }
    }
    out
}

/// Put `src`'s keys, in `src`'s order, into the positions of `dst` they
/// already occupy (every one of them does: `src` was just merged in). The
/// other keys keep their positions.
fn refill_slots(dst: &mut Table, src: &Table) {
    let mut keys: Vec<String> = dst.keys().cloned().collect();
    let slots: Vec<usize> = keys
        .iter()
        .enumerate()
        .filter(|(_, key)| src.contains_key(*key))
        .map(|(i, _)| i)
        .collect();
    for (slot, key) in slots.into_iter().zip(src.keys()) {
        keys[slot] = key.clone();
    }
    let mut old = std::mem::take(dst);
    for key in keys {
        if let Some(value) = old.remove(&key) {
            dst.insert(key, value);
        }
    }
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

    /// A user `vol_views.toml` that lists two shipped tabs the other way
    /// round swaps them in the merged document; a tab it does not list
    /// stays put and a new one follows. The same two files as `views`
    /// merge in the shipped order, as every other document does.
    #[test]
    fn vol_views_takes_a_higher_layers_order_for_the_keys_it_defines() {
        let keys = |merged: &MergedDoc| merged.value.keys().cloned().collect::<Vec<_>>();
        let merged = merge_docs(
            "vol_views",
            &[
                doc(
                    Layer::Builtin,
                    "vol_views",
                    "config_version = 1\n[vol]\n[skew]\n[wing]\n",
                ),
                doc(
                    Layer::User,
                    "vol_views",
                    "config_version = 1\n[wing]\n[vol]\n[mine]\n",
                ),
            ],
        );
        assert_eq!(
            keys(&merged),
            ["config_version", "wing", "skew", "vol", "mine"]
        );
        let merged = merge_docs(
            "views",
            &[
                doc(Layer::Builtin, "views", "[a]\n[b]\n"),
                doc(Layer::User, "views", "[b]\n[a]\n"),
            ],
        );
        assert_eq!(keys(&merged), ["a", "b"]);
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
    fn value_colors_replace_an_entry_whole_and_merge_a_dimension_per_value() {
        // Depth 2: a dimension's values merge per value, and one value's
        // entry (a name or an inline table) is replaced whole.
        let merged = merge_docs(
            crate::config::VALUE_COLORS_DOC,
            &[
                doc(
                    Layer::Desk,
                    "value_colors",
                    "[underlying_ref]\nSPX = \"blue\"\nNDX = { token = \"warning\" }\nRUT = \"amber\"\n",
                ),
                doc(
                    Layer::User,
                    "value_colors",
                    "[underlying_ref]\nSPX = \"teal\"\nNDX = { hue = 30 }\n",
                ),
            ],
        );
        let dimension = merged.value["underlying_ref"].as_table().unwrap();
        assert_eq!(dimension["SPX"].as_str(), Some("teal"));
        assert_eq!(
            dimension["RUT"].as_str(),
            Some("amber"),
            "the desk's other values survive a user override of one"
        );
        let ndx = dimension["NDX"].as_table().unwrap();
        assert_eq!(ndx.get("hue").and_then(|v| v.as_integer()), Some(30));
        assert!(
            ndx.get("token").is_none(),
            "the user's table replaces the desk's whole, never merging a token beside the hue"
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
    fn a_user_pricer_template_replaces_the_whole_builtin_entry() {
        let builtin = LayerDoc::builtin(
            "pricer_templates",
            "[RR]\nlegs = [ { weight = -1, strike = 1, kind = \"P\" }, { weight = 1, strike = 2, kind = \"C\" } ]\n",
        )
        .unwrap();
        let mut user = LayerDoc::builtin(
            "pricer_templates",
            "[RR]\nlegs = [ { weight = 1, strike = 1, kind = \"P\" }, { weight = -1, strike = 2, kind = \"C\" } ]\n",
        )
        .unwrap();
        user.layer = Layer::User;
        let merged = merge_docs("pricer_templates", &[builtin, user]);
        let legs = merged.value["RR"]["legs"].as_array().unwrap();
        assert_eq!(legs.len(), 2);
        assert_eq!(
            legs[0]["weight"].as_integer(),
            Some(1),
            "the user's entry, whole"
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

    #[test]
    fn a_user_watchlist_replaces_the_desk_one_whole() {
        let merged = merge_docs(
            "watchlists",
            &[
                doc(
                    Layer::Desk,
                    "watchlists",
                    "[eu]\ninclude = [\"SPX\"]\nexclude = [\"SMI\"]\n",
                ),
                doc(Layer::User, "watchlists", "[eu]\ninclude = [\"DAX\"]\n"),
            ],
        );
        let eu = merged.value["eu"].as_table().unwrap();
        assert_eq!(eu["include"].as_array().unwrap().len(), 1);
        assert!(
            eu.get("exclude").is_none(),
            "a lower layer's exclude must not leak into the user object"
        );
        assert_eq!(merged.provenance.get("eu"), Some(&Layer::User));
    }
}
