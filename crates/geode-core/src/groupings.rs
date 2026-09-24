//! Nine numbered grouping slots, labelled by their column sequence, such as
//! `lhu / underlying_ref / position_ref`. Each slot replaces whole across
//! configuration layers, so overriding slot 3 preserves the other slots.
//!
//! The reader checks that each name exists in some dataset or in derived
//! dimensions; it does not require a single dataset to carry the whole group
//! or verify that each column is groupable.

use crate::config::{Diagnostic, MergedDoc, Severity};
use crate::dimensions::DerivedDimensions;
use crate::schema::SchemaSpec;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct GroupingSlots {
    slots: [Option<Vec<String>>; 9],
}

fn index(slot: u8) -> Option<usize> {
    (1..=9).contains(&slot).then(|| slot as usize - 1)
}

impl GroupingSlots {
    pub fn from_doc(
        doc: &MergedDoc,
        schema: &SchemaSpec,
        dims: &DerivedDimensions,
    ) -> (GroupingSlots, Vec<Diagnostic>) {
        let mut out = GroupingSlots::default();
        let mut diags = Vec::new();
        let known = |column: &str| {
            dims.get(column).is_some()
                || schema.datasets.iter().any(|ds| ds.column(column).is_some())
        };
        // A slot's bare array is one object, so diagnostics name `groupings.<slot>`.
        // Unparseable slot numbers retain their original spelling in the path.
        let at = |severity: Severity, slot: &str, m: String| Diagnostic {
            severity,
            layer: None,
            file: None,
            message: m,
            path: Some(format!("groupings.{slot}")),
        };
        for (key, value) in &doc.value {
            if key == "config_version" {
                continue;
            }
            let slot = match key.parse::<u8>().ok().filter(|s| (1..=9).contains(s)) {
                Some(s) => s,
                None => {
                    diags.push(at(
                        Severity::Warning,
                        key,
                        format!("groupings: key '{key}' is not a slot number 1–9; ignored"),
                    ));
                    continue;
                }
            };
            let grouping: Vec<String> = match value.as_array() {
                Some(a) => a
                    .iter()
                    .filter_map(|v| v.as_str())
                    .map(str::to_string)
                    .collect(),
                None => {
                    diags.push(at(
                        Severity::Warning,
                        &slot.to_string(),
                        format!("groupings: slot {slot} must be an array of column names"),
                    ));
                    continue;
                }
            };
            if grouping.is_empty() {
                diags.push(at(
                    Severity::Warning,
                    &slot.to_string(),
                    format!("groupings: slot {slot} is empty; ignored"),
                ));
                continue;
            }
            if let Some(unknown) = grouping.iter().find(|c| !known(c)) {
                diags.push(at(
                    Severity::Error,
                    &slot.to_string(),
                    format!(
                        "groupings: slot {slot} names '{unknown}', which no dataset or \
                         derived dimension declares; slot dropped"
                    ),
                ));
                continue;
            }
            out.set(slot, grouping);
        }
        (out, diags)
    }

    pub fn get(&self, slot: u8) -> Option<&[String]> {
        self.slots.get(index(slot)?)?.as_deref()
    }

    /// `false` for a slot outside 1–9 or an empty grouping.
    pub fn set(&mut self, slot: u8, grouping: Vec<String>) -> bool {
        let Some(i) = index(slot) else {
            return false;
        };
        if grouping.is_empty() {
            return false;
        }
        self.slots[i] = Some(grouping);
        true
    }

    pub fn label(&self, slot: u8) -> Option<String> {
        self.get(slot).map(Self::label_of)
    }

    /// The grouping string a slot is known by everywhere.
    pub fn label_of(grouping: &[String]) -> String {
        grouping.join(" / ")
    }

    pub fn is_empty(&self) -> bool {
        self.slots.iter().all(Option::is_none)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{Layer, LayerDoc, Severity, merge_docs};
    use crate::dimensions::DerivedDimensions;
    use crate::schema::SchemaSpec;

    fn schema() -> SchemaSpec {
        let text = r#"
[risk_snapshot.columns.book]
type = "utf8"
role = "dimension"
[risk_snapshot.columns.lhu]
type = "utf8"
role = "dimension"
[risk_snapshot.columns.position_ref]
type = "utf8"
role = "key"
"#;
        let doc = merge_docs("datasets", &[LayerDoc::builtin("datasets", text).unwrap()]);
        SchemaSpec::from_doc(&doc).0
    }

    fn dims() -> DerivedDimensions {
        let text = "[desk]\nfrom = \"book\"\n[desk.values]\nEU = [\"BK000\"]\n";
        let doc = merge_docs(
            "dimensions",
            &[LayerDoc::builtin("dimensions", text).unwrap()],
        );
        DerivedDimensions::from_doc(&doc).0
    }

    fn layered(desk: &str, user: &str) -> MergedDoc {
        merge_docs(
            "groupings",
            &[
                LayerDoc {
                    layer: Layer::Desk,
                    name: "groupings".into(),
                    file: "desk/groupings.toml".into(),
                    table: desk.parse().unwrap(),
                },
                LayerDoc {
                    layer: Layer::User,
                    name: "groupings".into(),
                    file: "user/groupings.toml".into(),
                    table: user.parse().unwrap(),
                },
            ],
        )
    }

    #[test]
    fn slots_are_numbered_and_labelled_by_their_grouping_string() {
        let doc = merge_docs(
            "groupings",
            &[LayerDoc::builtin(
                "groupings",
                "config_version = 1\n1 = [\"desk\", \"book\", \"lhu\"]\n2 = [\"lhu\", \"position_ref\"]\n",
            )
            .unwrap()],
        );
        let (slots, diags) = GroupingSlots::from_doc(&doc, &schema(), &dims());
        assert!(diags.is_empty(), "{diags:?}");
        assert_eq!(
            slots.get(1),
            Some(&["desk".to_string(), "book".into(), "lhu".into()][..])
        );
        assert_eq!(slots.label(1).as_deref(), Some("desk / book / lhu"));
        assert_eq!(slots.label(2).as_deref(), Some("lhu / position_ref"));
        assert_eq!(slots.get(3), None);
        assert_eq!(slots.get(0), None);
        assert_eq!(slots.get(10), None);
        assert!(!slots.is_empty());
    }

    #[test]
    fn a_user_layer_overrides_one_slot_and_inherits_the_rest() {
        let doc = layered(
            "1 = [\"book\"]\n2 = [\"lhu\"]\n",
            "2 = [\"position_ref\"]\n",
        );
        let (slots, diags) = GroupingSlots::from_doc(&doc, &schema(), &dims());
        assert!(diags.is_empty(), "{diags:?}");
        assert_eq!(slots.label(1).as_deref(), Some("book"));
        assert_eq!(slots.label(2).as_deref(), Some("position_ref"));
    }

    #[test]
    fn an_unknown_column_is_an_error_for_that_slot_only() {
        let doc = merge_docs(
            "groupings",
            &[
                LayerDoc::builtin("groupings", "1 = [\"book\", \"nonesuch\"]\n2 = [\"lhu\"]\n")
                    .unwrap(),
            ],
        );
        let (slots, diags) = GroupingSlots::from_doc(&doc, &schema(), &dims());
        assert_eq!(slots.get(1), None);
        assert!(slots.get(2).is_some());
        assert_eq!(diags.len(), 1);
        assert_eq!(diags[0].severity, Severity::Error);
        assert!(diags[0].message.contains("slot 1") && diags[0].message.contains("nonesuch"));
    }

    #[test]
    fn bad_keys_and_shapes_warn_and_are_ignored() {
        let doc = merge_docs(
            "groupings",
            &[LayerDoc::builtin(
                "groupings",
                "config_version = 1\nfoo = [\"book\"]\n0 = [\"book\"]\n10 = [\"book\"]\n3 = \"book\"\n4 = []\n",
            )
            .unwrap()],
        );
        let (slots, diags) = GroupingSlots::from_doc(&doc, &schema(), &dims());
        assert!(slots.is_empty(), "{slots:?}");
        assert_eq!(diags.len(), 5, "{diags:?}");
        assert!(diags.iter().all(|d| d.severity == Severity::Warning));
    }

    #[test]
    fn a_grouping_diagnostic_carries_its_slot_path() {
        let doc = merge_docs(
            "groupings",
            &[LayerDoc::builtin("groupings", "3 = [\"book\", \"nonesuch\"]\n").unwrap()],
        );
        let (_, diags) = GroupingSlots::from_doc(&doc, &schema(), &dims());
        assert_eq!(diags[0].path.as_deref(), Some("groupings.3"), "{diags:?}");
    }

    #[test]
    fn set_replaces_a_slot_in_memory() {
        let mut slots = GroupingSlots::default();
        assert!(slots.set(3, vec!["book".into()]));
        assert!(!slots.set(0, vec!["book".into()]));
        assert!(!slots.set(3, Vec::new()), "an empty grouping is not a slot");
        assert_eq!(slots.label(3).as_deref(), Some("book"));
        assert_eq!(
            GroupingSlots::label_of(&["a".to_string(), "b".into()]),
            "a / b"
        );
    }
}
