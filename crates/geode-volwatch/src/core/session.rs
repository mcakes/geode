//! The tile's session table: the watchlist shown, the view and lookback,
//! the sort, the member under the cursor and whether the band is open. Bad
//! input is repaired, never refused: a value that cannot be read drops its
//! key with a notice, so one stale field cannot cost the tile its others.

use geode_core::sort::SortOrder;
use geode_core::volboard::Lookback;
use toml::{Table, Value};

use super::views::View;

const VERSION: i64 = 1;

/// Everything a tile restores. `sort` names a column id.
#[derive(Debug, Clone, PartialEq)]
pub struct State {
    pub list: Option<String>,
    pub view: View,
    pub lookback: Lookback,
    pub sort: Option<(String, SortOrder)>,
    pub cursor: Option<String>,
    pub band: bool,
}

impl Default for State {
    fn default() -> Self {
        State {
            list: None,
            view: View::Changes,
            lookback: Lookback::D1,
            sort: None,
            cursor: None,
            band: false,
        }
    }
}

fn order_word(o: SortOrder) -> &'static str {
    match o {
        SortOrder::Asc => "asc",
        SortOrder::Desc => "desc",
        SortOrder::AbsDesc => "abs desc",
        SortOrder::AbsAsc => "abs asc",
    }
}

fn parse_order(w: &str) -> Option<SortOrder> {
    [
        SortOrder::Asc,
        SortOrder::Desc,
        SortOrder::AbsDesc,
        SortOrder::AbsAsc,
    ]
    .into_iter()
    .find(|o| order_word(*o) == w)
}

/// `version`, then each set key; an unset key is absent and restores unset.
/// `sort` is `[column id, order word]`.
pub fn to_table(s: &State) -> Table {
    let mut t = Table::new();
    t.insert("version".into(), Value::Integer(VERSION));
    if let Some(list) = &s.list {
        t.insert("list".into(), Value::String(list.clone()));
    }
    t.insert("view".into(), Value::String(s.view.key().into()));
    t.insert("lookback".into(), Value::String(s.lookback.label().into()));
    if let Some((id, order)) = &s.sort {
        t.insert(
            "sort".into(),
            Value::Array(vec![
                Value::String(id.clone()),
                Value::String(order_word(*order).into()),
            ]),
        );
    }
    if let Some(cursor) = &s.cursor {
        t.insert("cursor".into(), Value::String(cursor.clone()));
    }
    t.insert("band".into(), Value::Boolean(s.band));
    t
}

fn read_name(v: &Value) -> Result<String, String> {
    v.as_str()
        .filter(|s| !s.trim().is_empty())
        .map(str::to_string)
        .ok_or_else(|| "not a name".into())
}

fn read_view(v: &Value) -> Result<View, String> {
    v.as_str()
        .and_then(View::parse)
        .ok_or_else(|| "not a view name".into())
}

fn read_lookback(v: &Value) -> Result<Lookback, String> {
    v.as_str()
        .and_then(|w| Lookback::ALL.into_iter().find(|l| l.label() == w))
        .ok_or_else(|| "not a lookback".into())
}

fn read_sort(v: &Value) -> Result<(String, SortOrder), String> {
    let words: Option<Vec<&str>> = v
        .as_array()
        .and_then(|a| a.iter().map(Value::as_str).collect());
    match words.as_deref() {
        Some([id, order]) if !id.trim().is_empty() => parse_order(order)
            .map(|o| (id.to_string(), o))
            .ok_or_else(|| format!("unknown order '{order}'")),
        _ => Err("not [column, order]".into()),
    }
}

fn read_band(v: &Value) -> Result<bool, String> {
    v.as_bool().ok_or_else(|| "not a bool".into())
}

/// Restore a state from a session table. A missing key takes its default;
/// an unreadable one drops with a `session: dropped <key>: <why>` notice.
/// Whether the list or column still exists is the tile's to say.
pub fn from_table(t: &Table) -> (State, Vec<String>) {
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
    let d = State::default();
    let state = State {
        list: read(t, "list", &mut notices, read_name),
        view: read(t, "view", &mut notices, read_view).unwrap_or(d.view),
        lookback: read(t, "lookback", &mut notices, read_lookback).unwrap_or(d.lookback),
        sort: read(t, "sort", &mut notices, read_sort),
        cursor: read(t, "cursor", &mut notices, read_name),
        band: read(t, "band", &mut notices, read_band).unwrap_or(d.band),
    };
    (state, notices)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_session_round_trips_and_drops_an_unreadable_key_with_a_notice() {
        let s = State {
            list: Some("europe_risk".into()),
            view: View::Term,
            lookback: Lookback::M1,
            sort: Some(("atfm-3M-change".into(), SortOrder::AbsDesc)),
            cursor: Some("SX5E".into()),
            band: true,
        };
        let (back, notices) = from_table(&to_table(&s));
        assert_eq!(back, s);
        assert!(notices.is_empty());
        let mut t = to_table(&s);
        t.insert("view".into(), toml::Value::Integer(3));
        let (back, notices) = from_table(&t);
        assert_eq!(back.view, View::Changes);
        assert_eq!(notices, ["session: dropped view: not a view name"]);
    }

    #[test]
    fn every_view_lookback_and_order_round_trips_through_text() {
        for view in [
            View::Changes,
            View::Levels,
            View::RichCheap,
            View::Term,
            View::Skew,
        ] {
            for lookback in Lookback::ALL {
                for order in [
                    SortOrder::Asc,
                    SortOrder::Desc,
                    SortOrder::AbsDesc,
                    SortOrder::AbsAsc,
                ] {
                    let s = State {
                        view,
                        lookback,
                        sort: Some(("c".into(), order)),
                        ..State::default()
                    };
                    let text = toml::to_string(&to_table(&s)).unwrap();
                    let (back, notices) = from_table(&text.parse().unwrap());
                    assert_eq!(back, s);
                    assert!(notices.is_empty(), "{notices:?}");
                }
            }
        }
    }

    #[test]
    fn an_empty_table_is_the_default_and_unset_keys_are_absent() {
        let (s, notices) = from_table(&Table::new());
        assert_eq!(s, State::default());
        assert!(notices.is_empty());
        let t = to_table(&State::default());
        for key in ["list", "sort", "cursor"] {
            assert!(!t.contains_key(key), "{key}");
        }
    }

    #[test]
    fn every_key_drops_alone_with_its_own_notice() {
        for (key, text, why) in [
            ("list", "list = 7", "not a name"),
            ("list", "list = \" \"", "not a name"),
            ("view", "view = \"wide\"", "not a view name"),
            ("lookback", "lookback = \"2w\"", "not a lookback"),
            ("lookback", "lookback = 1", "not a lookback"),
            ("sort", "sort = [\"c\"]", "not [column, order]"),
            ("sort", "sort = 3", "not [column, order]"),
            ("sort", "sort = [\"c\", \"up\"]", "unknown order 'up'"),
            ("cursor", "cursor = true", "not a name"),
            ("band", "band = \"yes\"", "not a bool"),
        ] {
            let (s, notices) = from_table(&format!("{text}\nversion = 1").parse().unwrap());
            assert_eq!(s, State::default(), "{text}");
            assert_eq!(
                notices,
                [format!("session: dropped {key}: {why}")],
                "{text}"
            );
        }
    }
}
