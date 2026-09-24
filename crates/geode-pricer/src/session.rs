//! The tile's session record (line-pricer spec §7.4): `{ sheet, view,
//! refresh, cursor, expanded }`. The sheet's rows are the store's; this
//! names which sheet the tile shows and how it was looking at it. Read
//! leniently — a key of the wrong type is ignored, never a refusal — so
//! a hand-edited `session.toml` still opens the tile.

use crate::core::sheet::{LineId, Refresh};
use crate::core::storage::{encode_refresh, parse_refresh};

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Record {
    pub sheet: Option<String>,
    pub view: Option<String>,
    pub refresh: Option<Refresh>,
    pub cursor: Option<LineId>,
    pub expanded: Vec<LineId>,
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
        };
        assert_eq!(Record::from_table(&r.to_table()), r);
        assert_eq!(
            Record::from_table(&Record::default().to_table()),
            Record::default()
        );
    }

    #[test]
    fn a_wrong_typed_key_is_ignored_not_refused() {
        let t: toml::Table =
            toml::from_str("sheet = 3\ncursor = -1\nexpanded = [\"x\", 4]\nrefresh = \"soon\"")
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
