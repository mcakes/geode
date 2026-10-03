//! Classifications: the editing model over a derived dimension
//! (`dimensions.toml`). A classification maps each value of one source
//! column to at most one label. This module is pure: rows for a grid, edits
//! that return the whole next object plus an undo entry, and the TOML the
//! config door writes.
//!
//! Undo re-applies over the *current* object, row by row, and skips a row
//! another surface changed since. Restoring a stored whole-object snapshot
//! would silently revert edits made elsewhere in the meantime.

use crate::dimensions::DerivedDimension;
use std::collections::BTreeSet;

pub mod csv;
pub mod import;
pub mod validate;

/// One grid row: a source value, its label, and how many stored rows carry
/// it (`None` when the value is only in the map, not in the data).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClassRow {
    pub source: String,
    pub label: Option<String>,
    pub count: Option<u64>,
    pub in_data: bool,
}

/// The union of observed source values and the map's keys. Unclassified rows
/// come first (most rows first, then by source), then classified rows by
/// label, then source. A value can be unclassified only if it was observed.
pub fn rows(dim: &DerivedDimension, observed: &[(String, u64)]) -> Vec<ClassRow> {
    let mut out: Vec<ClassRow> = observed
        .iter()
        .map(|(source, count)| ClassRow {
            source: source.clone(),
            label: dim.values.get(source).cloned(),
            count: Some(*count),
            in_data: true,
        })
        .collect();
    let seen: BTreeSet<&str> = observed.iter().map(|(s, _)| s.as_str()).collect();
    out.extend(
        dim.values
            .iter()
            .filter(|(source, _)| !seen.contains(source.as_str()))
            .map(|(source, label)| ClassRow {
                source: source.clone(),
                label: Some(label.clone()),
                count: None,
                in_data: false,
            }),
    );
    out.sort_by(|a, b| match (&a.label, &b.label) {
        (None, None) => b.count.cmp(&a.count).then_with(|| a.source.cmp(&b.source)),
        (None, Some(_)) => std::cmp::Ordering::Less,
        (Some(_), None) => std::cmp::Ordering::Greater,
        (Some(x), Some(y)) => x.cmp(y).then_with(|| a.source.cmp(&b.source)),
    });
    out
}

/// One row's label before and after an edit. `None` is unclassified.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Change {
    pub source: String,
    pub before: Option<String>,
    pub after: Option<String>,
}

/// The rows one operation changed — what `undo`/`redo` replay.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct UndoEntry {
    pub changes: Vec<Change>,
}

impl UndoEntry {
    pub fn is_empty(&self) -> bool {
        self.changes.is_empty()
    }
}

/// A trimmed label, `None` when blank: a blank label means unclassified.
fn normalized(label: Option<&str>) -> Option<String> {
    label
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .map(str::to_string)
}

fn set_label(dim: &mut DerivedDimension, source: &str, label: Option<&String>) {
    match label {
        Some(l) => {
            dim.values.insert(source.to_string(), l.clone());
        }
        None => {
            dim.values.remove(source);
        }
    }
}

/// Give every source in `sources` the label (`None` or blank clears). Returns
/// the next whole object and an entry holding only rows that changed, in the
/// order given; a repeated source counts once.
pub fn assign(
    dim: &DerivedDimension,
    sources: &[String],
    label: Option<&str>,
) -> (DerivedDimension, UndoEntry) {
    let after = normalized(label);
    let mut next = dim.clone();
    let mut entry = UndoEntry::default();
    for source in sources {
        let before = next.values.get(source).cloned();
        if before == after {
            continue;
        }
        set_label(&mut next, source, after.as_ref());
        entry.changes.push(Change {
            source: source.clone(),
            before,
            after: after.clone(),
        });
    }
    (next, entry)
}

/// Move each row of `entry` from `from` to `to` where the current label still
/// equals `from`; report the sources skipped because they changed since.
fn replay(
    current: &DerivedDimension,
    entry: &UndoEntry,
    forward: bool,
) -> (DerivedDimension, Vec<String>) {
    let mut next = current.clone();
    let mut skipped = Vec::new();
    for change in &entry.changes {
        let (expect, put) = if forward {
            (&change.before, &change.after)
        } else {
            (&change.after, &change.before)
        };
        if next.values.get(&change.source) != expect.as_ref() {
            skipped.push(change.source.clone());
            continue;
        }
        set_label(&mut next, &change.source, put.as_ref());
    }
    (next, skipped)
}

/// Revert `entry` over `current`, row by row (see the module docs).
pub fn undo(current: &DerivedDimension, entry: &UndoEntry) -> (DerivedDimension, Vec<String>) {
    replay(current, entry, false)
}

/// Re-apply `entry` over `current`, row by row.
pub fn redo(current: &DerivedDimension, entry: &UndoEntry) -> (DerivedDimension, Vec<String>) {
    replay(current, entry, true)
}

/// The labels in use, sorted and distinct.
pub fn labels(dim: &DerivedDimension) -> Vec<String> {
    dim.values
        .values()
        .cloned()
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect()
}

/// The `dimensions.<name>` object as `DerivedDimensions::from_doc` reads it:
/// `{ from, values = { label = [sources…] } }`, labels sorted, sources sorted
/// within a label.
pub fn to_toml(dim: &DerivedDimension) -> toml::Value {
    let mut by_label: std::collections::BTreeMap<&str, Vec<toml::Value>> = Default::default();
    for (source, label) in &dim.values {
        by_label
            .entry(label.as_str())
            .or_default()
            .push(toml::Value::String(source.clone()));
    }
    let mut values = toml::Table::new();
    for (label, sources) in by_label {
        values.insert(label.to_string(), toml::Value::Array(sources));
    }
    let mut table = toml::Table::new();
    table.insert("from".into(), toml::Value::String(dim.from.clone()));
    table.insert("values".into(), toml::Value::Table(values));
    toml::Value::Table(table)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{LayerDoc, merge_docs};
    use crate::dimensions::DerivedDimensions;
    use std::collections::BTreeMap;

    fn sector(pairs: &[(&str, &str)]) -> DerivedDimension {
        DerivedDimension {
            name: "sector".into(),
            from: "underlying_ref".into(),
            values: pairs
                .iter()
                .map(|(s, l)| (s.to_string(), l.to_string()))
                .collect::<BTreeMap<_, _>>(),
        }
    }

    fn s(v: &[&str]) -> Vec<String> {
        v.iter().map(|x| x.to_string()).collect()
    }

    #[test]
    fn rows_put_unclassified_first_by_count_then_classified_by_label() {
        let dim = sector(&[("AAPL", "Tech"), ("XOM", "Energy"), ("OLD", "Energy")]);
        let observed = vec![
            ("AAPL".to_string(), 10),
            ("NKY".to_string(), 3),
            ("HSCEI".to_string(), 7),
            ("XOM".to_string(), 1),
        ];
        let got: Vec<(String, Option<String>, Option<u64>, bool)> = rows(&dim, &observed)
            .into_iter()
            .map(|r| (r.source, r.label, r.count, r.in_data))
            .collect();
        assert_eq!(
            got,
            vec![
                ("HSCEI".into(), None, Some(7), true),
                ("NKY".into(), None, Some(3), true),
                ("OLD".into(), Some("Energy".into()), None, false),
                ("XOM".into(), Some("Energy".into()), Some(1), true),
                ("AAPL".into(), Some("Tech".into()), Some(10), true),
            ]
        );
    }

    #[test]
    fn unclassified_rows_with_equal_counts_order_by_source() {
        let dim = sector(&[]);
        let observed = vec![("B".to_string(), 2), ("A".to_string(), 2)];
        let got: Vec<String> = rows(&dim, &observed)
            .into_iter()
            .map(|r| r.source)
            .collect();
        assert_eq!(got, s(&["A", "B"]));
    }

    #[test]
    fn assign_sets_labels_and_records_only_real_changes() {
        let dim = sector(&[("AAPL", "Tech"), ("XOM", "Energy")]);
        let (next, entry) = assign(&dim, &s(&["AAPL", "XOM", "NKY"]), Some(" Tech "));
        assert_eq!(next.values.get("XOM").map(String::as_str), Some("Tech"));
        assert_eq!(next.values.get("NKY").map(String::as_str), Some("Tech"));
        assert_eq!(
            entry.changes,
            vec![
                Change {
                    source: "XOM".into(),
                    before: Some("Energy".into()),
                    after: Some("Tech".into())
                },
                Change {
                    source: "NKY".into(),
                    before: None,
                    after: Some("Tech".into())
                },
            ],
            "AAPL already said Tech, so it is not a change"
        );
    }

    #[test]
    fn assigning_a_blank_label_clears() {
        let dim = sector(&[("AAPL", "Tech")]);
        let (next, entry) = assign(&dim, &s(&["AAPL"]), Some("   "));
        assert!(!next.values.contains_key("AAPL"));
        assert_eq!(entry.changes[0].after, None);
        let (next, _) = assign(&sector(&[("AAPL", "Tech")]), &s(&["AAPL"]), None);
        assert!(next.values.is_empty());
    }

    #[test]
    fn undo_reverts_over_the_current_object_and_skips_rows_changed_since() {
        let dim = sector(&[("AAPL", "Tech"), ("XOM", "Energy")]);
        let (edited, entry) = assign(&dim, &s(&["AAPL", "XOM"]), Some("Index"));
        // Another surface changes XOM afterwards, and adds an unrelated row.
        let (mut current, _) = assign(&edited, &s(&["XOM"]), Some("Utilities"));
        current.values.insert("SPX".into(), "Index".into());

        let (undone, skipped) = undo(&current, &entry);
        assert_eq!(undone.values.get("AAPL").map(String::as_str), Some("Tech"));
        assert_eq!(
            undone.values.get("XOM").map(String::as_str),
            Some("Utilities")
        );
        assert_eq!(undone.values.get("SPX").map(String::as_str), Some("Index"));
        assert_eq!(skipped, s(&["XOM"]));
    }

    #[test]
    fn redo_reapplies_where_the_row_still_holds_its_before_value() {
        let dim = sector(&[("AAPL", "Tech")]);
        let (edited, entry) = assign(&dim, &s(&["AAPL", "NKY"]), Some("Index"));
        let (undone, _) = undo(&edited, &entry);
        let (redone, skipped) = redo(&undone, &entry);
        assert_eq!(redone, edited);
        assert!(skipped.is_empty());
    }

    #[test]
    fn to_toml_round_trips_through_the_dimensions_reader() {
        let dim = sector(&[("AAPL", "Tech"), ("MSFT", "Tech"), ("XOM", "Energy")]);
        let mut doc = toml::Table::new();
        doc.insert("sector".into(), to_toml(&dim));
        let text = toml::to_string(&doc).unwrap();
        let merged = merge_docs(
            "dimensions",
            &[LayerDoc::builtin("dimensions", &text).unwrap()],
        );
        let (dims, diags) = DerivedDimensions::from_doc(&merged);
        assert!(diags.is_empty(), "{diags:?}");
        assert_eq!(dims.get("sector"), Some(&dim));
        // Labels sorted, sources sorted within a label.
        assert!(
            text.find("Energy").unwrap() < text.find("Tech").unwrap(),
            "{text}"
        );
    }

    #[test]
    fn an_empty_map_renders_an_empty_values_table() {
        let value = to_toml(&sector(&[]));
        let table = value.as_table().unwrap();
        assert_eq!(
            table.get("from").and_then(|v| v.as_str()),
            Some("underlying_ref")
        );
        assert!(table.get("values").unwrap().as_table().unwrap().is_empty());
    }

    #[test]
    fn labels_are_sorted_and_distinct() {
        let dim = sector(&[("A", "Tech"), ("B", "Energy"), ("C", "Tech")]);
        assert_eq!(labels(&dim), s(&["Energy", "Tech"]));
    }
}
