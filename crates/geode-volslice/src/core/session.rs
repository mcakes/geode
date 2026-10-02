//! The tile's session table: every choice in [`State`] but the cursor.
//! The launch table `{ underlying = "<u>" }` restores through the same
//! [`from_table`], every missing key taking its default. Bad input is
//! repaired, never refused: a value that cannot be read drops its key
//! with a notice, so one stale field cannot cost the tile its others.

use std::collections::BTreeSet;

use chrono::NaiveDate;
use geode_chart::core::layout::{SPLIT_MAX, SPLIT_MIN};
use geode_core::vol::Coordinate;
use toml::{Table, Value};

use crate::core::model::{Kind, Pair, State};

const VERSION: i64 = 1;
const DATE: &str = "%Y-%m-%d";

pub fn to_table(state: &State) -> Table {
    let mut t = Table::new();
    t.insert("version".into(), Value::Integer(VERSION));
    if let Some(u) = &state.underlying {
        t.insert("underlying".into(), Value::String(u.clone()));
    }
    t.insert(
        "coordinate".into(),
        Value::String(state.coordinate.name().into()),
    );
    if let Some(active) = &state.active {
        let dates = active
            .iter()
            .map(|e| Value::String(e.format(DATE).to_string()))
            .collect();
        t.insert("expiries".into(), Value::Array(dates));
    }
    let hidden = state
        .hidden
        .iter()
        .map(|k| Value::String(k.label().into()))
        .collect();
    t.insert("hidden".into(), Value::Array(hidden));
    if let Some(p) = state.diff {
        t.insert(
            "diff".into(),
            Value::Array(vec![
                Value::String(p.minuend.label().into()),
                Value::String(p.subtrahend.label().into()),
            ]),
        );
    }
    t.insert("density".into(), Value::Boolean(state.density));
    t.insert("split".into(), Value::Float(state.split as f64));
    if let Some((lo, hi)) = state.view {
        t.insert(
            "view".into(),
            Value::Array(vec![Value::Float(lo), Value::Float(hi)]),
        );
    }
    t
}

/// A TOML number, integer or float.
fn number(v: &Value) -> Option<f64> {
    v.as_float().or_else(|| v.as_integer().map(|i| i as f64))
}

fn strings(v: &Value) -> Option<Vec<&str>> {
    v.as_array()?.iter().map(Value::as_str).collect()
}

fn kinds(v: &Value) -> Result<Vec<Kind>, String> {
    let words = strings(v).ok_or("not a list of kinds")?;
    words
        .into_iter()
        .map(|w| Kind::parse(w).ok_or_else(|| format!("unknown kind '{w}'")))
        .collect()
}

fn read_underlying(v: &Value) -> Result<String, String> {
    v.as_str()
        .filter(|s| !s.trim().is_empty())
        .map(str::to_string)
        .ok_or_else(|| "not an underlying".into())
}

fn read_coordinate(v: &Value) -> Result<Coordinate, String> {
    let name = v.as_str().ok_or("not a coordinate name")?;
    Coordinate::parse(name).ok_or_else(|| format!("unknown coordinate '{name}'"))
}

fn read_expiries(v: &Value) -> Result<BTreeSet<NaiveDate>, String> {
    let words = strings(v).ok_or("not a list of dates")?;
    words
        .into_iter()
        .map(|w| NaiveDate::parse_from_str(w, DATE).map_err(|_| format!("'{w}' is not a date")))
        .collect()
}

fn read_diff(v: &Value) -> Result<Pair, String> {
    let [a, b] = kinds(v)?[..] else {
        return Err("a difference names two kinds".into());
    };
    Pair::new(a, b).ok_or_else(|| "a difference needs two different kinds".into())
}

fn read_density(v: &Value) -> Result<bool, String> {
    v.as_bool().ok_or_else(|| "not true or false".into())
}

fn read_split(v: &Value) -> Result<f64, String> {
    number(v)
        .filter(|s| s.is_finite())
        .ok_or_else(|| "not a finite number".into())
}

fn read_view(v: &Value) -> Result<(f64, f64), String> {
    let pair: Option<Vec<f64>> = v.as_array().and_then(|a| a.iter().map(number).collect());
    match pair.as_deref() {
        Some(&[lo, hi]) if lo.is_finite() && hi.is_finite() && lo < hi => Ok((lo, hi)),
        Some(&[_, _]) => Err("not a finite, ascending range".into()),
        _ => Err("not a range of two numbers".into()),
    }
}

/// Restore a state from a session or launch table. A missing key takes
/// its default; a value that cannot be read drops its key with a
/// `session: dropped <key>` notice, and an out-of-bounds split is clamped
/// to the chart's split bounds with a notice. Expiries are kept as
/// written: whether each is still listed is the strip's to say, and the
/// strip is unknown here.
pub fn from_table(table: &Table) -> (State, Vec<String>) {
    let mut state = State::default();
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
    state.underlying = read(table, "underlying", &mut notices, read_underlying);
    if let Some(c) = read(table, "coordinate", &mut notices, read_coordinate) {
        state.coordinate = c;
    }
    state.active = read(table, "expiries", &mut notices, read_expiries);
    if let Some(h) = read(table, "hidden", &mut notices, kinds) {
        state.hidden = h.into_iter().collect();
    }
    state.diff = read(table, "diff", &mut notices, read_diff);
    if let Some(dn) = read(table, "density", &mut notices, read_density) {
        state.density = dn;
    }
    if let Some(s) = read(table, "split", &mut notices, read_split) {
        // The chart's own bounds: a split it would clamp at paint is
        // repaired here, so the saved and the painted split agree.
        let clamped = (s as f32).clamp(SPLIT_MIN, SPLIT_MAX);
        if clamped != s as f32 {
            notices.push(format!(
                "session: split {s} clamped to {SPLIT_MIN}..={SPLIT_MAX}"
            ));
        }
        state.split = clamped;
    }
    state.view = read(table, "view", &mut notices, read_view);
    (state, notices)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::model::tests::d;

    fn table(text: &str) -> Table {
        text.parse().unwrap()
    }

    #[test]
    fn a_state_round_trips_through_its_table() {
        let st = State {
            underlying: Some("SPX.Z".into()),
            coordinate: Coordinate::Delta,
            hidden: [Kind::Chain, Kind::Draft].into(),
            active: Some([d("2026-10-16"), d("2026-12-18")].into()),
            cursor: 0,
            density: true,
            diff: Pair::new(Kind::Draft, Kind::Cvi),
            split: 0.55,
            view: Some((0.85, 1.15)),
        };
        let t = to_table(&st);
        assert_eq!(t.get("version").and_then(|v| v.as_integer()), Some(1));
        assert_eq!(
            t.get("diff").unwrap().as_array().unwrap()[0].as_str(),
            Some("cvi draft")
        );
        // Through text, as the session file holds it.
        let text = toml::to_string(&t).unwrap();
        let (back, notices) = from_table(&table(&text));
        assert_eq!(back, st);
        assert!(notices.is_empty(), "{notices:?}");
    }

    #[test]
    fn unset_values_are_absent_and_restore_unset() {
        let t = to_table(&State::default());
        for key in ["underlying", "expiries", "diff", "view"] {
            assert!(!t.contains_key(key), "{key} is absent while unset");
        }
        assert_eq!(from_table(&t), (State::default(), Vec::new()));
    }

    #[test]
    fn a_launch_table_restores_its_underlying_with_defaults() {
        let (st, notices) = from_table(&table(r#"underlying = "SPX.Z""#));
        assert_eq!(
            st,
            State {
                underlying: Some("SPX.Z".into()),
                ..State::default()
            }
        );
        assert!(notices.is_empty());
    }

    #[test]
    fn bad_values_are_dropped_with_a_notice() {
        let cases = [
            ("coordinate", r#"coordinate = "radians""#),
            ("coordinate", "coordinate = 3"),
            ("expiries", r#"expiries = ["2026-10-16", "someday"]"#),
            ("hidden", r#"hidden = ["chain", "bids"]"#),
            ("diff", r#"diff = ["cvi", "cvi"]"#),
            ("diff", r#"diff = ["cvi", "bids"]"#),
            ("diff", r#"diff = ["cvi"]"#),
            ("view", "view = [nan, 1.1]"),
            ("view", "view = [0.9, inf]"),
            ("view", "view = [1.1, 0.9]"),
            ("density", r#"density = "yes""#),
            ("underlying", "underlying = 7"),
            ("split", "split = nan"),
        ];
        for (key, text) in cases {
            // A good sibling key survives its neighbour's drop.
            let mut want = State::default();
            let sibling = if key == "density" {
                want.coordinate = Coordinate::Delta;
                "coordinate = \"delta\""
            } else {
                want.density = true;
                "density = true"
            };
            let (st, notices) = from_table(&table(&format!("{text}\n{sibling}")));
            assert_eq!(notices.len(), 1, "{text}: {notices:?}");
            assert!(
                notices[0].starts_with(&format!("session: dropped {key}")),
                "{text}: {notices:?}"
            );
            assert_eq!(st, want, "{text}: the key takes its default");
        }
    }

    #[test]
    fn a_split_out_of_bounds_is_clamped_with_a_notice() {
        let (st, notices) = from_table(&table("split = 0.95"));
        assert_eq!(st.split, SPLIT_MAX);
        assert_eq!(notices.len(), 1);
        assert!(notices[0].starts_with("session: split"), "{notices:?}");
        let (st, _) = from_table(&table("split = 0"));
        assert_eq!(st.split, SPLIT_MIN, "an integer is a number");
    }

    #[test]
    fn the_cursor_is_not_saved() {
        let st = State {
            cursor: 3,
            ..State::default()
        };
        let t = to_table(&st);
        assert!(!t.contains_key("cursor"));
        assert_eq!(from_table(&t).0.cursor, 0);
    }
}
