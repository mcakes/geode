//! The tile's session table: the watchlist shown, the view by name, the
//! lens and its lookback, metric, window and shown number, the sort, the
//! member under the cursor and whether the band is open. Bad input is
//! repaired, never refused: a value that cannot be read drops its key with
//! a notice, so one stale field cannot cost the tile its others.

use geode_core::sort::SortOrder;
use geode_core::volboard::Lookback;
use toml::{Table, Value};

use super::lens::{Lens, LensState, Metric, Shown, Window};

// The lens keys are additive: a record written before them still reads,
// with the default lens, so the version stays.
const VERSION: i64 = 1;

/// Everything a tile restores. `view` names a tab in the vol views
/// document; whether that name still exists is the tile's to say. `sort`
/// names a column id.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct State {
    pub list: Option<String>,
    pub view: Option<String>,
    pub lens: LensState,
    pub sort: Option<(String, SortOrder)>,
    pub cursor: Option<String>,
    pub band: bool,
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
/// The lens keys are always written, as a word each. `sort` is
/// `[column id, order word]`.
pub fn to_table(s: &State) -> Table {
    let mut t = Table::new();
    t.insert("version".into(), Value::Integer(VERSION));
    if let Some(list) = &s.list {
        t.insert("list".into(), Value::String(list.clone()));
    }
    if let Some(view) = &s.view {
        t.insert("view".into(), Value::String(view.clone()));
    }
    let lens = &s.lens;
    t.insert(
        "lookback".into(),
        Value::String(lens.lookback.label().into()),
    );
    t.insert("lens".into(), Value::String(lens.lens.word().into()));
    t.insert("metric".into(), Value::String(lens.metric.word().into()));
    t.insert("window".into(), Value::String(lens.window.word().into()));
    t.insert("shown".into(), Value::String(lens.shown.word().into()));
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

/// A string key whose text is one of a closed set of words.
fn read_word<T>(
    v: &Value,
    parse: impl Fn(&str) -> Option<T>,
    why: &'static str,
) -> Result<T, String> {
    v.as_str().and_then(parse).ok_or_else(|| why.into())
}

fn parse_lookback(w: &str) -> Option<Lookback> {
    Lookback::ALL.into_iter().find(|l| l.label() == w)
}

fn read_sort(v: &Value) -> Result<(String, SortOrder), String> {
    let words: Option<Vec<&str>> = v
        .as_array()
        .and_then(|a| a.iter().map(Value::as_str).collect());
    match words.as_deref() {
        Some([id, order]) if !id.trim().is_empty() => {
            parse_order(order).map(|o| (id.to_string(), o))
        }
        _ => None,
    }
    .ok_or_else(|| "not [column, order]".to_string())
}

fn read_band(v: &Value) -> Result<bool, String> {
    v.as_bool().ok_or_else(|| "not a bool".into())
}

/// Restore a state from a session table. A missing key takes its default;
/// an unreadable one drops with a `session: dropped <key>: <why>` notice.
/// Whether the list, view or column still exists is the tile's to say.
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
    let lens = LensState {
        lookback: read(t, "lookback", &mut notices, |v| {
            read_word(v, parse_lookback, "not a lookback")
        })
        .unwrap_or(d.lens.lookback),
        lens: read(t, "lens", &mut notices, |v| {
            read_word(v, Lens::parse, "not a lens")
        })
        .unwrap_or(d.lens.lens),
        metric: read(t, "metric", &mut notices, |v| {
            read_word(v, Metric::parse, "not a metric")
        })
        .unwrap_or(d.lens.metric),
        window: read(t, "window", &mut notices, |v| {
            read_word(v, Window::parse, "not a window")
        })
        .unwrap_or(d.lens.window),
        shown: read(t, "shown", &mut notices, |v| {
            read_word(v, Shown::parse, "not level or companion")
        })
        .unwrap_or(d.lens.shown),
    };
    let state = State {
        list: read(t, "list", &mut notices, read_name),
        view: read(t, "view", &mut notices, read_name),
        lens,
        sort: read(t, "sort", &mut notices, read_sort),
        cursor: read(t, "cursor", &mut notices, read_name),
        band: read(t, "band", &mut notices, read_band).unwrap_or(d.band),
    };
    (state, notices)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::lens::{Lens, Metric, Shown, Window};

    fn rich() -> LensState {
        LensState {
            lens: Lens::Rich,
            lookback: Lookback::M1,
            metric: Metric::Pctl,
            window: Window::Y2,
            shown: Shown::Companion,
        }
    }

    #[test]
    fn a_session_round_trips_and_drops_an_unreadable_key_with_a_notice() {
        let s = State {
            list: Some("europe_risk".into()),
            view: Some("skew".into()),
            lens: rich(),
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
        assert_eq!(back.view, None);
        assert_eq!(back.lens, rich(), "the lens keeps its keys");
        assert_eq!(notices, ["session: dropped view: not a name"]);
    }

    #[test]
    fn every_lens_key_and_order_round_trips_through_text() {
        for lens in [Lens::Moves, Lens::Rich] {
            for lookback in Lookback::ALL {
                for metric in [Metric::Z, Metric::Pctl] {
                    for window in Window::ALL {
                        for shown in [Shown::Level, Shown::Companion] {
                            for order in [
                                SortOrder::Asc,
                                SortOrder::Desc,
                                SortOrder::AbsDesc,
                                SortOrder::AbsAsc,
                            ] {
                                let s = State {
                                    lens: LensState {
                                        lens,
                                        lookback,
                                        metric,
                                        window,
                                        shown,
                                    },
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
            }
        }
    }

    #[test]
    fn an_empty_table_is_the_default_and_unset_keys_are_absent() {
        let (s, notices) = from_table(&Table::new());
        assert_eq!(s, State::default());
        assert!(notices.is_empty());
        let t = to_table(&State::default());
        for key in ["list", "view", "sort", "cursor"] {
            assert!(!t.contains_key(key), "{key}");
        }
        for key in ["lookback", "lens", "metric", "window", "shown", "band"] {
            assert!(t.contains_key(key), "{key} is always written");
        }
    }

    #[test]
    fn the_lens_keys_are_words_a_hand_can_type() {
        let t = to_table(&State {
            lens: rich(),
            ..State::default()
        });
        assert_eq!(t["lens"].as_str(), Some("rich"));
        assert_eq!(t["lookback"].as_str(), Some("1m"));
        assert_eq!(t["metric"].as_str(), Some("pctl"));
        assert_eq!(t["window"].as_str(), Some("2Y"));
        assert_eq!(t["shown"].as_str(), Some("companion"));
    }

    #[test]
    fn every_key_drops_alone_with_its_own_notice() {
        for (key, text, why) in [
            ("list", "list = 7", "not a name"),
            ("list", "list = \" \"", "not a name"),
            ("view", "view = 3", "not a name"),
            ("view", "view = \" \"", "not a name"),
            ("lookback", "lookback = \"2w\"", "not a lookback"),
            ("lookback", "lookback = 1", "not a lookback"),
            ("lens", "lens = \"both\"", "not a lens"),
            ("lens", "lens = 1", "not a lens"),
            ("metric", "metric = \"sd\"", "not a metric"),
            ("window", "window = \"3Y\"", "not a window"),
            ("shown", "shown = \"delta\"", "not level or companion"),
            ("sort", "sort = [\"c\"]", "not [column, order]"),
            ("sort", "sort = 3", "not [column, order]"),
            ("sort", "sort = [\"c\", \"up\"]", "not [column, order]"),
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

    #[test]
    fn an_old_five_view_name_reads_as_a_name_for_the_tile_to_judge() {
        let (s, notices) = from_table(&"view = \"richcheap\"\nversion = 1".parse().unwrap());
        assert_eq!(s.view.as_deref(), Some("richcheap"));
        assert!(notices.is_empty());
    }

    #[test]
    fn a_record_without_the_lens_keys_reads_the_default_lens() {
        let text = "version = 1\nlist = \"l\"\nview = \"vol\"\nlookback = \"1w\"\nband = false";
        let (s, notices) = from_table(&text.parse().unwrap());
        assert_eq!(
            s.lens,
            LensState {
                lookback: Lookback::W1,
                ..LensState::default()
            }
        );
        assert_eq!(s.view.as_deref(), Some("vol"));
        assert!(notices.is_empty());
    }

    #[test]
    fn two_unreadable_keys_drop_with_two_notices_and_the_rest_stays() {
        let s = State {
            list: Some("l".into()),
            lens: LensState {
                lookback: Lookback::W1,
                ..LensState::default()
            },
            cursor: Some("SPX".into()),
            ..State::default()
        };
        let mut t = to_table(&s);
        t.insert("metric".into(), Value::String("sd".into()));
        t.insert("band".into(), Value::String("yes".into()));
        let (back, notices) = from_table(&t);
        assert_eq!(back, s);
        assert_eq!(
            notices,
            [
                "session: dropped metric: not a metric",
                "session: dropped band: not a bool"
            ]
        );
    }
}
