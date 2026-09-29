//! Tile session state: sheet name, view, refresh setting, cursor line ID,
//! expanded package IDs, `:autosize` column widths, and `:unscoped`. Sheet
//! contents are stored separately. Reading ignores wrong-typed fields,
//! invalid refresh values, and negative IDs; valid members of a mixed
//! expansion list are retained.

use crate::core::sheet::{LineId, Refresh};
use crate::core::storage::{encode_refresh, parse_refresh};
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
        };
        assert_eq!(Record::from_table(&r.to_table()), r);
        assert_eq!(
            Record::from_table(&Record::default().to_table()),
            Record::default()
        );
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
