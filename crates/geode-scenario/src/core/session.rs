//! The tile's persisted state: currency, paint, scale, diff, the filter
//! layer, `unscoped` and the as-of override, as a `toml::Table` the shell
//! stores opaquely. Cursor, selection and notices are transient. An
//! unreadable value drops its key with a notice naming it; nothing is
//! guessed from a malformed record.

use crate::core::pivot::Currency;
use geode_core::query::AsOf;
use geode_core::scenario::{Paint, ShapeSpec};
use geode_core::scope::{DimensionSelection, Scope, parse_expr};
use geode_core::view::Scale;
use toml::{Table, Value};

/// The tile's as-of override. A pinned tile queries at its own instant and
/// ignores the frame's `as_of` counter; `:asof clear` returns it to `Follow`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TileAsOf {
    Follow,
    Pinned(AsOf),
}

#[derive(Debug, Clone, PartialEq)]
pub struct State {
    pub currency: Currency,
    pub paint: Paint,
    /// `None`: each column's own format scale; `Some`: overrides every column.
    pub scale: Option<Scale>,
    /// Meaningful only on a shape with `diff`.
    pub diff: bool,
    /// The `:filter` layer (and a launch's `underlying_ref` selection).
    pub filter: Scope,
    pub unscoped: bool,
    pub as_of: TileAsOf,
}

impl State {
    /// What a tile starts with: USD when any painted measure has a twin,
    /// the shape's paint, diff on when the shape has one.
    pub fn fresh(shape: &ShapeSpec) -> State {
        let has_twin = shape.measures().iter().any(|m| m.usd_twin.is_some());
        State {
            currency: if has_twin {
                Currency::Usd
            } else {
                Currency::Native
            },
            paint: shape.paint,
            scale: None,
            diff: shape.diff.is_some(),
            filter: Scope::default(),
            unscoped: false,
            as_of: TileAsOf::Follow,
        }
    }
}

/// The record the shell stores; `diff` is written only for a shape that
/// has one, so a matrix's record never carries a key it cannot read back.
pub fn to_table(shape: &ShapeSpec, state: &State) -> Table {
    let mut t = Table::new();
    let currency = match state.currency {
        Currency::Usd => "usd",
        Currency::Native => "native",
    };
    t.insert("currency".into(), Value::String(currency.into()));
    t.insert("paint".into(), Value::String(state.paint.as_str().into()));
    if let Some(scale) = state.scale {
        let scale = match scale {
            Scale::Thousands => "k",
            Scale::Millions => "m",
            Scale::None => "1",
        };
        t.insert("scale".into(), Value::String(scale.into()));
    }
    if shape.diff.is_some() {
        t.insert("diff".into(), Value::Boolean(state.diff));
    }
    t.insert("unscoped".into(), Value::Boolean(state.unscoped));
    match &state.as_of {
        TileAsOf::Follow => {}
        TileAsOf::Pinned(AsOf::Live) => {
            t.insert("as_of".into(), Value::String("live".into()));
        }
        TileAsOf::Pinned(AsOf::At(at)) => {
            t.insert(
                "as_of".into(),
                Value::String(at.to_rfc3339_opts(chrono::SecondsFormat::AutoSi, true)),
            );
        }
    }
    if !state.filter.is_empty() {
        let mut filter = Table::new();
        if let Some(expr) = &state.filter.expression {
            filter.insert("expr".into(), Value::String(expr.to_string()));
        }
        if let Some(text) = &state.filter.text {
            filter.insert("text".into(), Value::String(text.clone()));
        }
        let dims: Table = state
            .filter
            .dimensions
            .iter()
            .filter(|d| !d.values.is_empty())
            .map(|d| {
                (
                    d.column.clone(),
                    Value::Array(d.values.iter().cloned().map(Value::String).collect()),
                )
            })
            .collect();
        if !dims.is_empty() {
            filter.insert("dimensions".into(), Value::Table(dims));
        }
        t.insert("filter".into(), Value::Table(filter));
    }
    t
}

/// `restored` read over `State::fresh(shape)`; each unreadable key is
/// dropped with a notice `session: dropped <key>: <why>`.
pub fn from_table(shape: &ShapeSpec, restored: &Table) -> (State, Vec<String>) {
    let mut s = State::fresh(shape);
    let mut notices = Vec::new();
    let mut drop = |key: &str, why: String| notices.push(format!("session: dropped {key}: {why}"));
    if let Some(v) = restored.get("currency") {
        match v.as_str() {
            Some("usd") => s.currency = Currency::Usd,
            Some("native") => s.currency = Currency::Native,
            _ => drop(
                "currency",
                format!("expected \"usd\" or \"native\", got {v}"),
            ),
        }
    }
    if let Some(v) = restored.get("paint") {
        match v.as_str().and_then(Paint::parse) {
            Some(p) => s.paint = p,
            None => drop("paint", format!("expected \"heat\" or \"plain\", got {v}")),
        }
    }
    if let Some(v) = restored.get("scale") {
        match v.as_str() {
            Some("k") => s.scale = Some(Scale::Thousands),
            Some("m") => s.scale = Some(Scale::Millions),
            Some("1") => s.scale = None,
            _ => drop("scale", format!("expected \"k\", \"m\" or \"1\", got {v}")),
        }
    }
    if let Some(v) = restored.get("diff") {
        match (v.as_bool(), shape.diff.is_some()) {
            (Some(b), true) => s.diff = b,
            (Some(_), false) => {}
            (None, _) => drop("diff", format!("expected a boolean, got {v}")),
        }
    }
    if let Some(v) = restored.get("unscoped") {
        match v.as_bool() {
            Some(b) => s.unscoped = b,
            None => drop("unscoped", format!("expected a boolean, got {v}")),
        }
    }
    if let Some(v) = restored.get("as_of") {
        match v.as_str() {
            Some("live") => s.as_of = TileAsOf::Pinned(AsOf::Live),
            Some(text) => match chrono::DateTime::parse_from_rfc3339(text) {
                Ok(at) => s.as_of = TileAsOf::Pinned(AsOf::At(at.with_timezone(&chrono::Utc))),
                Err(e) => drop("as_of", format!("'{text}' is not RFC 3339 ({e})")),
            },
            None => drop("as_of", format!("expected a string, got {v}")),
        }
    }
    if let Some(v) = restored.get("filter") {
        match v.as_table().map(read_filter) {
            Some(Ok(scope)) => s.filter = scope,
            Some(Err(why)) => drop("filter", why),
            None => drop("filter", format!("expected a table, got {v}")),
        }
    }
    (s, notices)
}

/// `[filter]`: `expr`, `text` and `[filter.dimensions]`. A syntax error in
/// `expr`, or any part of the wrong type, drops the whole filter with the
/// `filter` notice naming the part, as the blotter's restore does: a
/// partly read filter would narrow the grid differently from the saved one.
fn read_filter(f: &Table) -> Result<Scope, String> {
    let mut scope = Scope::default();
    if let Some(v) = f.get("expr") {
        let expr = v
            .as_str()
            .ok_or_else(|| format!("expr: expected a string, got {v}"))?;
        let parsed = parse_expr(expr).map_err(|e| {
            format!(
                "expr '{expr}' failed to parse at column {}: {}",
                e.caret + 1,
                e.message
            )
        })?;
        scope.expression = Some(parsed);
    }
    if let Some(v) = f.get("text") {
        let text = v
            .as_str()
            .ok_or_else(|| format!("text: expected a string, got {v}"))?;
        scope.text = (!text.is_empty()).then(|| text.to_string());
    }
    if let Some(v) = f.get("dimensions") {
        let dims = v
            .as_table()
            .ok_or_else(|| format!("dimensions: expected a table, got {v}"))?;
        for (column, values) in dims {
            let values: Vec<String> = values
                .as_array()
                .ok_or_else(|| format!("dimensions.{column} is not an array"))?
                .iter()
                .map(|v| {
                    v.as_str()
                        .map(str::to_string)
                        .ok_or_else(|| format!("dimensions.{column} holds a non-string"))
                })
                .collect::<Result<_, _>>()?;
            if !values.is_empty() {
                scope.dimensions.push(DimensionSelection {
                    column: column.clone(),
                    values,
                });
            }
        }
    }
    Ok(scope)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::pivot::fixtures::{by_maturity, spot_ladder, vega_kt};

    #[test]
    fn a_fresh_state_starts_in_usd_when_a_twin_exists_else_native() {
        let s = State::fresh(&vega_kt());
        assert_eq!(s.currency, Currency::Usd);
        assert_eq!(s.paint, Paint::Heat);
        assert!(!s.diff, "a matrix has no diff");
        assert_eq!(s.scale, None);
        assert!(s.filter.is_empty() && !s.unscoped);
        assert_eq!(s.as_of, TileAsOf::Follow);
        let ladder = State::fresh(&spot_ladder());
        assert!(ladder.diff, "a shape with diff starts in diff mode");
        let plain = State::fresh(&by_maturity());
        assert_eq!(plain.currency, Currency::Native, "no twin anywhere");
        assert_eq!(plain.paint, Paint::Plain);
    }

    #[test]
    fn the_table_round_trips_every_key() {
        let mut s = State::fresh(&spot_ladder());
        s.currency = Currency::Native;
        s.paint = Paint::Plain;
        s.scale = Some(Scale::Millions);
        s.diff = false;
        s.unscoped = true;
        s.as_of = TileAsOf::Pinned(AsOf::Live);
        s.filter = Scope {
            dimensions: vec![DimensionSelection {
                column: "underlying_ref".into(),
                values: vec!["SPX".into(), "NDX".into()],
            }],
            text: Some("call".into()),
            expression: Some(parse_expr("vega > 0").unwrap()),
            ..Scope::default()
        };
        let t = to_table(&spot_ladder(), &s);
        assert_eq!(t.get("currency").and_then(Value::as_str), Some("native"));
        assert_eq!(t.get("scale").and_then(Value::as_str), Some("m"));
        assert_eq!(t.get("as_of").and_then(Value::as_str), Some("live"));
        let (back, notices) = from_table(&spot_ladder(), &t);
        assert!(notices.is_empty(), "{notices:?}");
        assert_eq!(back, s);
    }

    #[test]
    fn a_pinned_instant_round_trips_in_rfc3339_utc() {
        let at = chrono::DateTime::parse_from_rfc3339("2026-10-10T14:30:00Z")
            .unwrap()
            .with_timezone(&chrono::Utc);
        let mut s = State::fresh(&vega_kt());
        s.as_of = TileAsOf::Pinned(AsOf::At(at));
        let t = to_table(&vega_kt(), &s);
        assert_eq!(
            t.get("as_of").and_then(Value::as_str),
            Some("2026-10-10T14:30:00Z")
        );
        assert_eq!(from_table(&vega_kt(), &t).0.as_of, s.as_of);
    }

    #[test]
    fn omitted_keys_mean_the_fresh_defaults_and_bad_ones_drop_with_a_notice() {
        let fresh = State::fresh(&spot_ladder());
        let (s, notices) = from_table(&spot_ladder(), &Table::new());
        assert_eq!(s, fresh);
        assert!(notices.is_empty());
        let t: Table = r#"
currency = "gbp"
paint = "stripes"
scale = "x"
diff = "yes"
unscoped = 1
as_of = "yesterday"
[filter]
expr = "vega >"
text = "ok"
"#
        .parse()
        .unwrap();
        let (s, notices) = from_table(&spot_ladder(), &t);
        assert_eq!(s.currency, fresh.currency);
        assert_eq!(s.paint, fresh.paint);
        assert_eq!(s.scale, None);
        assert_eq!(s.diff, fresh.diff);
        assert!(!s.unscoped);
        assert_eq!(s.as_of, TileAsOf::Follow);
        assert!(
            s.filter.expression.is_none(),
            "a bad expression drops the whole filter"
        );
        assert!(s.filter.text.is_none());
        let keys: Vec<&str> = notices
            .iter()
            .map(|n| {
                n.strip_prefix("session: dropped ")
                    .and_then(|r| r.split(':').next())
                    .unwrap_or_else(|| panic!("not a session notice: {n}"))
            })
            .collect();
        assert_eq!(
            keys,
            [
                "currency", "paint", "scale", "diff", "unscoped", "as_of", "filter"
            ]
        );
        assert!(
            notices.iter().all(|n| n.starts_with("session: dropped ")),
            "{notices:?}"
        );
    }

    #[test]
    fn a_pinned_instant_keeps_its_fractional_seconds() {
        let at = chrono::DateTime::parse_from_rfc3339("2026-10-10T14:30:00.750Z")
            .unwrap()
            .with_timezone(&chrono::Utc);
        let mut s = State::fresh(&vega_kt());
        s.as_of = TileAsOf::Pinned(AsOf::At(at));
        let t = to_table(&vega_kt(), &s);
        assert_eq!(
            t.get("as_of").and_then(Value::as_str),
            Some("2026-10-10T14:30:00.750Z")
        );
        assert_eq!(from_table(&vega_kt(), &t).0.as_of, s.as_of);
    }

    #[test]
    fn a_filter_part_of_the_wrong_type_drops_the_filter_with_a_notice() {
        for (record, part) in [
            ("[filter]\nexpr = 1\ntext = \"ok\"\n", "expr"),
            ("[filter]\ntext = true\nexpr = \"vega > 0\"\n", "text"),
            (
                "[filter]\ndimensions = \"SPX\"\ntext = \"ok\"\n",
                "dimensions",
            ),
        ] {
            let t: Table = record.parse().unwrap();
            let (s, notices) = from_table(&spot_ladder(), &t);
            assert!(s.filter.is_empty(), "{part}: nothing of the filter is kept");
            assert_eq!(notices.len(), 1, "{part}: {notices:?}");
            assert!(
                notices[0].starts_with(&format!("session: dropped filter: {part}: expected")),
                "{notices:?}"
            );
        }
    }

    #[test]
    fn diff_is_ignored_on_a_shape_without_one() {
        let t: Table = "diff = true\n".parse().unwrap();
        let (s, notices) = from_table(&vega_kt(), &t);
        assert!(!s.diff);
        assert!(
            notices.is_empty(),
            "no notice: the key is simply not this shape's"
        );
        assert!(
            !to_table(&vega_kt(), &s).contains_key("diff"),
            "and it is not written back"
        );
    }
}
