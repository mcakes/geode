//! The tile's session table: which classification it shows, its sort and
//! the source value under its cursor. Bad input is repaired, never refused:
//! a value that cannot be read drops its key with a notice, so one stale
//! field cannot cost the tile its others.

use toml::{Table, Value};

const VERSION: i64 = 1;

/// A grid column the tile can sort by.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SortCol {
    Source,
    Label,
    Rows,
}

impl SortCol {
    pub fn name(self) -> &'static str {
        match self {
            SortCol::Source => "source",
            SortCol::Label => "label",
            SortCol::Rows => "rows",
        }
    }

    pub fn parse(word: &str) -> Option<SortCol> {
        match word {
            "source" => Some(SortCol::Source),
            "label" => Some(SortCol::Label),
            "rows" => Some(SortCol::Rows),
            _ => None,
        }
    }
}

/// Everything a tile restores. `sort`'s flag is descending.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct State {
    pub name: Option<String>,
    pub sort: Option<(SortCol, bool)>,
    pub cursor: Option<String>,
}

const ASC: &str = "asc";
const DESC: &str = "desc";

/// `version`, then each set key; an unset key is absent and restores unset.
/// `sort` is `[column, "asc" | "desc"]`.
pub fn to_table(state: &State) -> Table {
    let mut t = Table::new();
    t.insert("version".into(), Value::Integer(VERSION));
    if let Some(name) = &state.name {
        t.insert("name".into(), Value::String(name.clone()));
    }
    if let Some((col, desc)) = state.sort {
        let dir = if desc { DESC } else { ASC };
        t.insert(
            "sort".into(),
            Value::Array(vec![
                Value::String(col.name().into()),
                Value::String(dir.into()),
            ]),
        );
    }
    if let Some(cursor) = &state.cursor {
        t.insert("cursor".into(), Value::String(cursor.clone()));
    }
    t
}

fn read_name(v: &Value) -> Result<String, String> {
    v.as_str()
        .filter(|s| !s.trim().is_empty())
        .map(str::to_string)
        .ok_or_else(|| "not a classification name".into())
}

fn read_sort(v: &Value) -> Result<(SortCol, bool), String> {
    let words: Option<Vec<&str>> = v
        .as_array()
        .and_then(|a| a.iter().map(Value::as_str).collect());
    let Some([col, dir]) = words.as_deref() else {
        return Err("not a column and a direction".into());
    };
    let col = SortCol::parse(col).ok_or_else(|| format!("unknown column '{col}'"))?;
    match *dir {
        ASC => Ok((col, false)),
        DESC => Ok((col, true)),
        other => Err(format!("unknown direction '{other}'")),
    }
}

fn read_cursor(v: &Value) -> Result<String, String> {
    v.as_str()
        .map(str::to_string)
        .ok_or_else(|| "not a source value".into())
}

/// Restore a state from a session table. A missing key takes its default;
/// a value that cannot be read drops its key with a
/// `session: dropped <key>` notice. Whether the named classification still
/// exists is the configuration's to say, and it is unknown here.
pub fn from_table(table: &Table) -> (State, Vec<String>) {
    let mut notices = Vec::new();
    fn read<T>(
        table: &Table,
        key: &str,
        notices: &mut Vec<String>,
        f: impl FnOnce(&Value) -> Result<T, String>,
    ) -> Option<T> {
        let v = table.get(key)?;
        f(v).map_err(|why| notices.push(format!("session: dropped {key}: {why}")))
            .ok()
    }
    let state = State {
        name: read(table, "name", &mut notices, read_name),
        sort: read(table, "sort", &mut notices, read_sort),
        cursor: read(table, "cursor", &mut notices, read_cursor),
    };
    (state, notices)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_state_round_trips() {
        let s = State {
            name: Some("sector".into()),
            sort: Some((SortCol::Label, false)),
            cursor: Some("SPX".into()),
        };
        assert_eq!(from_table(&to_table(&s)).0, s);
        // Through text, as the session file holds it, every column and
        // direction.
        for col in [SortCol::Source, SortCol::Label, SortCol::Rows] {
            for desc in [false, true] {
                let s = State {
                    sort: Some((col, desc)),
                    ..s.clone()
                };
                let text = toml::to_string(&to_table(&s)).unwrap();
                let (back, notices) = from_table(&text.parse().unwrap());
                assert_eq!(back, s);
                assert!(notices.is_empty(), "{notices:?}");
            }
        }
    }

    #[test]
    fn the_table_spells_sort_as_column_and_direction() {
        let t = to_table(&State {
            sort: Some((SortCol::Label, true)),
            ..State::default()
        });
        assert_eq!(t.get("version").and_then(Value::as_integer), Some(1));
        let sort: Vec<&str> = t["sort"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap())
            .collect();
        assert_eq!(sort, ["label", "desc"]);
        for key in ["name", "cursor"] {
            assert!(!t.contains_key(key), "{key} is absent while unset");
        }
    }

    #[test]
    fn an_unreadable_key_is_dropped_with_a_notice_and_the_rest_kept() {
        let mut t = to_table(&State {
            name: Some("sector".into()),
            ..State::default()
        });
        t.insert("sort".into(), Value::Integer(3));
        let (s, notices) = from_table(&t);
        assert_eq!(s.name.as_deref(), Some("sector"));
        assert_eq!(s.sort, None);
        assert_eq!(notices.len(), 1);
        assert!(
            notices[0].starts_with("session: dropped sort"),
            "{notices:?}"
        );
    }

    #[test]
    fn bad_values_are_dropped_with_a_notice() {
        for (key, text) in [
            ("name", "name = 7"),
            ("name", "name = \"  \""),
            ("sort", "sort = [\"label\"]"),
            ("sort", "sort = [\"colour\", \"asc\"]"),
            ("sort", "sort = [\"label\", \"up\"]"),
            ("cursor", "cursor = true"),
        ] {
            let (s, notices) = from_table(&format!("{text}\nversion = 1").parse().unwrap());
            assert_eq!(s, State::default(), "{text}");
            assert_eq!(notices.len(), 1, "{text}: {notices:?}");
            assert!(
                notices[0].starts_with(&format!("session: dropped {key}")),
                "{text}: {notices:?}"
            );
        }
    }

    #[test]
    fn an_empty_table_is_the_default() {
        assert_eq!(from_table(&Table::new()).0, State::default());
    }
}
