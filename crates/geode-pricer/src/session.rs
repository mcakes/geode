//! Tile session state: sheet name, view, refresh setting, cursor line ID,
//! expanded package IDs, `:autosize` column widths, `:unscoped`, the
//! grouping pin (`pinned` / `pinned_slot`, the blotter's keys) and the
//! open grouping rows (`expanded_paths`). Sheet contents are stored
//! separately. Reading ignores wrong-typed fields, invalid refresh values,
//! and negative IDs; valid members of a mixed expansion list are retained.
//!
//! `expanded_paths` is an array of paths, each an array of the group
//! values from the root: a string for a value, the inline table
//! `{ null = true }` for NULL (TOML has no null, and NULL is a different
//! group from the empty string). A path holding anything else is dropped
//! whole.

use crate::core::sheet::{LineId, Refresh};
use crate::core::storage::{encode_refresh, parse_refresh};
use geode_core::expansion::Path;
use geode_shell::colfit::{FittedWidths, SESSION_KEY, widths_from_record, widths_to_toml};

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Record {
    pub sheet: Option<String>,
    pub view: Option<String>,
    pub refresh: Option<Refresh>,
    pub cursor: Option<LineId>,
    pub expanded: Vec<LineId>,
    /// `:autosize`'s fitted widths by column name (`geode_shell::colfit`).
    pub widths: FittedWidths,
    /// `:unscoped`: the tile ignores the frame's scope (the blotter's
    /// session key). Written only when set.
    pub unscoped: bool,
    /// `:group <cols…>`: the pinned chain (session key `pinned`).
    pub pinned: Option<Vec<String>>,
    /// `:group slot N`: the pinned slot, 1–9 (session key `pinned_slot`).
    pub pinned_slot: Option<u8>,
    /// The open grouping rows by path; empty when none is open.
    pub expanded_paths: Vec<Path>,
}

/// One path segment: a string, or NULL as `{ null = true }`.
fn segment(v: &toml::Value) -> Option<Option<String>> {
    match v {
        toml::Value::String(s) => Some(Some(s.clone())),
        toml::Value::Table(t) if t.get("null").and_then(|v| v.as_bool()) == Some(true) => {
            Some(None)
        }
        _ => None,
    }
}

fn path(v: &toml::Value) -> Option<Path> {
    v.as_array()?.iter().map(segment).collect()
}

fn path_value(p: &Path) -> toml::Value {
    toml::Value::Array(
        p.iter()
            .map(|s| match s {
                Some(s) => toml::Value::String(s.clone()),
                None => {
                    let mut t = toml::Table::new();
                    t.insert("null".into(), toml::Value::Boolean(true));
                    toml::Value::Table(t)
                }
            })
            .collect(),
    )
}

fn id(v: &toml::Value) -> Option<LineId> {
    v.as_integer()
        .and_then(|i| u64::try_from(i).ok())
        .map(LineId)
}

impl Record {
    pub fn from_table(t: &toml::Table) -> Record {
        Record {
            sheet: t.get("sheet").and_then(|v| v.as_str()).map(str::to_string),
            view: t.get("view").and_then(|v| v.as_str()).map(str::to_string),
            refresh: t
                .get("refresh")
                .and_then(|v| v.as_str())
                .and_then(|s| parse_refresh(s).ok()),
            cursor: t.get("cursor").and_then(id),
            expanded: t
                .get("expanded")
                .and_then(|v| v.as_array())
                .map(|a| a.iter().filter_map(id).collect())
                .unwrap_or_default(),
            widths: widths_from_record(Some(t)),
            unscoped: t.get("unscoped").and_then(|v| v.as_bool()).unwrap_or(false),
            pinned: t.get("pinned").and_then(|v| v.as_array()).map(|a| {
                a.iter()
                    .filter_map(|v| v.as_str())
                    .map(str::to_string)
                    .collect()
            }),
            pinned_slot: t
                .get("pinned_slot")
                .and_then(|v| v.as_integer())
                .and_then(|n| u8::try_from(n).ok())
                .filter(|n| (1..=9).contains(n)),
            expanded_paths: t
                .get("expanded_paths")
                .and_then(|v| v.as_array())
                .map(|a| a.iter().filter_map(path).collect())
                .unwrap_or_default(),
        }
    }

    pub fn to_table(&self) -> toml::Table {
        let mut t = toml::Table::new();
        if let Some(s) = &self.sheet {
            t.insert("sheet".into(), s.clone().into());
        }
        if let Some(v) = &self.view {
            t.insert("view".into(), v.clone().into());
        }
        if let Some(r) = self.refresh {
            t.insert("refresh".into(), encode_refresh(r).into());
        }
        if let Some(c) = self.cursor {
            t.insert("cursor".into(), toml::Value::Integer(c.0 as i64));
        }
        if !self.expanded.is_empty() {
            t.insert(
                "expanded".into(),
                toml::Value::Array(
                    self.expanded
                        .iter()
                        .map(|id| toml::Value::Integer(id.0 as i64))
                        .collect(),
                ),
            );
        }
        if let Some(w) = widths_to_toml(&self.widths) {
            t.insert(SESSION_KEY.into(), w);
        }
        if self.unscoped {
            t.insert("unscoped".into(), toml::Value::Boolean(true));
        }
        if let Some(chain) = &self.pinned {
            t.insert(
                "pinned".into(),
                toml::Value::Array(chain.iter().map(|c| c.clone().into()).collect()),
            );
        }
        if let Some(n) = self.pinned_slot {
            t.insert("pinned_slot".into(), toml::Value::Integer(n.into()));
        }
        if !self.expanded_paths.is_empty() {
            t.insert(
                "expanded_paths".into(),
                toml::Value::Array(self.expanded_paths.iter().map(path_value).collect()),
            );
        }
        t
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn a_record_round_trips_through_its_table() {
        let r = Record {
            sheet: Some("book".into()),
            view: Some("barrier".into()),
            refresh: Some(Refresh::Every(Duration::from_secs(10))),
            cursor: Some(LineId(7)),
            expanded: vec![LineId(2), LineId(9)],
            widths: [("strike".to_string(), 92.0), ("__tree".to_string(), 64.0)]
                .into_iter()
                .collect(),
            unscoped: true,
            pinned: Some(vec!["underlying_ref".into(), "expiry".into()]),
            pinned_slot: None,
            expanded_paths: vec![
                vec![Some("SPX".into()), Some("2026-12-18".into())],
                vec![None],
                vec![Some(String::new())],
            ],
        };
        assert_eq!(Record::from_table(&r.to_table()), r);
        let slot = Record {
            pinned: None,
            pinned_slot: Some(3),
            ..r.clone()
        };
        assert_eq!(Record::from_table(&slot.to_table()), slot);
        assert_eq!(
            Record::from_table(&Record::default().to_table()),
            Record::default()
        );
    }

    /// `:group none` is an empty `pinned` array: it reads back as the
    /// empty pin, never as no pin.
    #[test]
    fn an_empty_pinned_array_is_the_empty_pin() {
        let t: toml::Table = toml::from_str("pinned = []").unwrap();
        assert_eq!(Record::from_table(&t).pinned, Some(Vec::new()));
        let none = Record {
            pinned: Some(Vec::new()),
            ..Record::default()
        };
        assert_eq!(Record::from_table(&none.to_table()), none);
    }

    /// NULL is `{ null = true }`, distinct from the empty string; a path
    /// holding any other shape is dropped whole (a shortened path would
    /// open a different node), the rest kept. A slot out of 1–9 is no pin.
    #[test]
    fn expanded_paths_keep_null_apart_from_empty_and_drop_a_malformed_path() {
        let t: toml::Table = toml::from_str(
            "expanded_paths = [[\"SPX\", { null = true }], [\"\"], [\"NDX\", 3], \"flat\"]\n\
             pinned_slot = 12",
        )
        .unwrap();
        let r = Record::from_table(&t);
        assert_eq!(
            r.expanded_paths,
            vec![
                vec![Some("SPX".to_string()), None],
                vec![Some(String::new())]
            ]
        );
        assert_eq!(r.pinned_slot, None);
    }

    #[test]
    fn a_wrong_typed_key_is_ignored_not_refused() {
        let t: toml::Table = toml::from_str(
            "sheet = 3\ncursor = -1\nexpanded = [\"x\", 4]\nrefresh = \"soon\"\nunscoped = 1",
        )
        .unwrap();
        assert_eq!(
            Record::from_table(&t),
            Record {
                expanded: vec![LineId(4)],
                ..Record::default()
            }
        );
    }
}
