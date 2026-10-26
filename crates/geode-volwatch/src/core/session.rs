//! The tile's session table: the watchlist shown, the view by name, the
//! lens and its lookback, metric, window and shown number, the sort, the
//! member under the cursor, whether the band is open, the grouping pin,
//! the collapsed group rows and each view's column order. Bad input is
//! repaired, never refused: a value that cannot be read drops its key with
//! a notice, so one stale field cannot cost the tile its others.

use std::collections::{BTreeMap, BTreeSet};

use geode_core::sort::SortOrder;
use geode_core::volboard::Lookback;
use toml::{Table, Value};

use super::grouping::Pin;
use super::lens::{Lens, LensState, Metric, Shown, Window};

// The lens keys are additive: a record written before them still reads,
// with the default lens, so the version stays.
const VERSION: i64 = 1;

/// Everything a tile restores. `view` names a tab in the VolMonitor views
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
    /// The grouping pin (§7.1); `Pin::None` follows the frame.
    pub pin: Pin,
    /// Collapsed group rows by path (one label per level, the group's own
    /// last), kept across rebuilds and regroups within the chain's depth.
    pub collapsed: BTreeSet<Vec<String>>,
    /// Each view's column order by name: the unit stems (`atfm-3M`,
    /// `spark-atfm-3M`, `vega`) in painted order, written by a column
    /// move. Tile presentation, never the document's order: a view the
    /// map does not name paints in file order.
    pub column_order: BTreeMap<String, Vec<String>>,
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
/// `[column id, order word]`. The pin is `pinned` (a list of dimensions;
/// empty for the flat pin) or `pinned_slot` (1–9); `collapsed` is a list
/// of group paths, written only when a row is collapsed; `column_order`
/// is a table of `<view name> = [stem, …]`, written only when a view has
/// been reordered.
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
    match &s.pin {
        Pin::None => {}
        Pin::Grouping(g) => {
            t.insert("pinned".into(), strings(g));
        }
        Pin::Slot(n) => {
            t.insert("pinned_slot".into(), Value::Integer(i64::from(*n)));
        }
        // The empty list: `:group none`, a flat tile by choice.
        Pin::Flat => {
            t.insert("pinned".into(), Value::Array(Vec::new()));
        }
    }
    if !s.collapsed.is_empty() {
        t.insert(
            "collapsed".into(),
            Value::Array(s.collapsed.iter().map(|p| strings(p)).collect()),
        );
    }
    if !s.column_order.is_empty() {
        t.insert(
            "column_order".into(),
            Value::Table(
                s.column_order
                    .iter()
                    .map(|(view, ids)| (view.clone(), strings(ids)))
                    .collect(),
            ),
        );
    }
    t
}

/// A table of view name to a list of column id stems; anything else
/// drops the whole key, since a half-read order would move columns the
/// trader never moved.
fn read_column_order(v: &Value) -> Result<BTreeMap<String, Vec<String>>, String> {
    v.as_table()
        .and_then(|t| {
            t.iter()
                .map(|(view, ids)| Some((view.clone(), read_strings(ids)?)))
                .collect::<Option<BTreeMap<_, _>>>()
        })
        .ok_or_else(|| "not a table of column id lists".into())
}

fn strings(words: &[String]) -> Value {
    Value::Array(words.iter().map(|w| Value::String(w.clone())).collect())
}

fn read_strings(v: &Value) -> Option<Vec<String>> {
    v.as_array()?
        .iter()
        .map(|w| w.as_str().map(str::to_string))
        .collect()
}

fn read_pinned(v: &Value) -> Result<Pin, String> {
    match read_strings(v) {
        Some(g) if g.is_empty() => Ok(Pin::Flat),
        Some(g) => Ok(Pin::Grouping(g)),
        None => Err("not a list of dimensions".into()),
    }
}

fn read_slot(v: &Value) -> Result<Pin, String> {
    v.as_integer()
        .and_then(|n| u8::try_from(n).ok())
        .filter(|n| (1..=9).contains(n))
        .map(Pin::Slot)
        .ok_or_else(|| "not a slot 1\u{2013}9".into())
}

/// A list of group paths, each at least one label long: an empty path
/// would be a fold of nothing that no group row could ever match.
fn read_collapsed(v: &Value) -> Result<BTreeSet<Vec<String>>, String> {
    v.as_array()
        .and_then(|a| a.iter().map(read_strings).collect::<Option<BTreeSet<_>>>())
        .filter(|paths| paths.iter().all(|p| !p.is_empty()))
        .ok_or_else(|| "not a list of group paths".into())
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
    // `pinned_slot` wins over `pinned` when both are present, so a slot
    // pin written beside a stale list restores as the slot.
    let slot = read(t, "pinned_slot", &mut notices, read_slot);
    let pin = match slot {
        Some(p) => p,
        None => read(t, "pinned", &mut notices, read_pinned).unwrap_or_default(),
    };
    let state = State {
        list: read(t, "list", &mut notices, read_name),
        view: read(t, "view", &mut notices, read_name),
        lens,
        sort: read(t, "sort", &mut notices, read_sort),
        cursor: read(t, "cursor", &mut notices, read_name),
        band: read(t, "band", &mut notices, read_band).unwrap_or(d.band),
        pin,
        collapsed: read(t, "collapsed", &mut notices, read_collapsed).unwrap_or_default(),
        column_order: read(t, "column_order", &mut notices, read_column_order).unwrap_or_default(),
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
            pin: Pin::Slot(4),
            collapsed: paths(&[&["Core"]]),
            column_order: order("vol", &["atfm-3M", "atfm-1W"]),
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
        for key in ["list", "view", "sort", "cursor", "column_order"] {
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

    fn paths(p: &[&[&str]]) -> BTreeSet<Vec<String>> {
        p.iter()
            .map(|path| path.iter().map(|s| s.to_string()).collect())
            .collect()
    }

    #[test]
    fn the_pin_and_the_collapsed_set_round_trip() {
        for pin in [
            Pin::None,
            Pin::Grouping(vec!["region".into(), "sector".into()]),
            Pin::Slot(3),
            Pin::Flat,
        ] {
            let s = State {
                pin: pin.clone(),
                collapsed: paths(&[&["Core"], &["Core", "Index"], &["\u{2014}"]]),
                ..State::default()
            };
            let t = to_table(&s);
            let (back, notices) = from_table(&t);
            assert_eq!(back, s, "{pin:?}");
            assert!(notices.is_empty());
            match &pin {
                Pin::None => assert!(!t.contains_key("pinned") && !t.contains_key("pinned_slot")),
                Pin::Grouping(_) => assert_eq!(t["pinned"].as_array().unwrap().len(), 2),
                Pin::Slot(n) => assert_eq!(t["pinned_slot"].as_integer(), Some(i64::from(*n))),
                Pin::Flat => assert_eq!(t["pinned"].as_array().unwrap().len(), 0),
            }
        }
        let t = to_table(&State::default());
        assert!(!t.contains_key("collapsed"), "an empty set writes no key");
    }

    #[test]
    fn a_bad_pin_or_collapsed_entry_drops_its_key_alone() {
        let mut t = to_table(&State::default());
        t.insert("pinned_slot".into(), toml::Value::Integer(12));
        t.insert(
            "collapsed".into(),
            toml::Value::Array(vec![toml::Value::String("Core".into())]),
        );
        let (back, notices) = from_table(&t);
        assert_eq!(back.pin, Pin::None);
        assert!(back.collapsed.is_empty());
        assert_eq!(
            notices,
            [
                "session: dropped pinned_slot: not a slot 1\u{2013}9",
                "session: dropped collapsed: not a list of group paths"
            ]
        );
        let mut t = to_table(&State::default());
        t.insert(
            "pinned".into(),
            toml::Value::Array(vec![toml::Value::Integer(1)]),
        );
        let (back, notices) = from_table(&t);
        assert_eq!(back.pin, Pin::None);
        assert_eq!(
            notices,
            ["session: dropped pinned: not a list of dimensions"]
        );
    }

    #[test]
    fn an_empty_collapsed_path_drops_the_key() {
        // A group path names at least one label; an empty inner list would
        // be a fold of nothing that the tree could never match.
        let mut t = to_table(&State::default());
        t.insert(
            "collapsed".into(),
            toml::Value::Array(vec![toml::Value::Array(Vec::new())]),
        );
        let (back, notices) = from_table(&t);
        assert!(back.collapsed.is_empty());
        assert_eq!(
            notices,
            ["session: dropped collapsed: not a list of group paths"]
        );
    }

    fn order(view: &str, ids: &[&str]) -> BTreeMap<String, Vec<String>> {
        BTreeMap::from([(
            view.to_string(),
            ids.iter().map(|s| s.to_string()).collect(),
        )])
    }

    #[test]
    fn the_column_order_round_trips_per_view() {
        let mut s = State {
            column_order: order("vol", &["atfm-3M", "atfm-1W", "spark-atfm-3M"]),
            ..State::default()
        };
        s.column_order.insert("skew".into(), vec!["skew-1M".into()]);
        let t = to_table(&s);
        let orders = t["column_order"].as_table().unwrap();
        assert_eq!(orders.len(), 2);
        assert_eq!(
            orders["vol"].as_array().unwrap().len(),
            3,
            "one entry per unit, the measure stem"
        );
        let text = toml::to_string(&t).unwrap();
        let (back, notices) = from_table(&text.parse().unwrap());
        assert_eq!(back, s);
        assert!(notices.is_empty(), "{notices:?}");
        assert!(
            !to_table(&State::default()).contains_key("column_order"),
            "no order writes no key"
        );
    }

    #[test]
    fn a_bad_column_order_drops_the_key_alone() {
        for text in [
            "column_order = 3",
            "column_order = [\"vol\"]",
            "[column_order]\nvol = 3",
            "[column_order]\nvol = [1, 2]",
        ] {
            let (s, notices) = from_table(&format!("{text}\nversion = 1").parse().unwrap());
            assert_eq!(s, State::default(), "{text}");
            assert_eq!(
                notices,
                ["session: dropped column_order: not a table of column id lists"],
                "{text}"
            );
        }
    }

    #[test]
    fn pinned_slot_wins_when_both_pin_keys_are_present() {
        let mut t = to_table(&State {
            pin: Pin::Grouping(vec!["region".into()]),
            ..State::default()
        });
        t.insert("pinned_slot".into(), toml::Value::Integer(2));
        let (back, _) = from_table(&t);
        assert_eq!(back.pin, Pin::Slot(2));
    }
}
