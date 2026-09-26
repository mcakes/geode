//! `TileState` in `session.toml` (spec §9.11): slots (kind, colour, axis,
//! visible, rule; an expression by its TEXT), range, frequency, axis
//! mode, split, density, percentiles. Not the view, not slot state.
//! `from_table` heals a hostile table rather than refusing it, like
//! `Tree::from_parts`; every drop is a notice the tile shows once.

use geode_chart::{Axis, AxisMode};
use geode_core::series::{BucketRule, Frequency, MAX_BINS, MIN_BINS, SlotKind};
use toml::{Table, Value};

use super::model::{Colour, Model, SlotState};
use super::range::Range;
use super::resolve::resolve;
use super::rgb::Rgb8;

pub fn to_table(model: &Model) -> Table {
    let mut t = Table::new();
    let slots: Vec<Value> = model
        .slots()
        .iter()
        .map(|s| {
            let mut r = Table::new();
            r.insert("number".into(), Value::Integer(s.number as i64));
            match &s.kind {
                SlotKind::Source {
                    source,
                    identity,
                    rule,
                } => {
                    r.insert("kind".into(), Value::String("source".into()));
                    r.insert("identity".into(), Value::String(identity.clone()));
                    r.insert("source".into(), Value::String(source.clone()));
                    if *rule != BucketRule::Last {
                        r.insert("rule".into(), Value::String(rule.as_str().into()));
                    }
                }
                SlotKind::Expr(_) => {
                    r.insert("kind".into(), Value::String("expr".into()));
                    r.insert(
                        "text".into(),
                        Value::String(s.text.clone().unwrap_or_default()),
                    );
                }
            }
            match &s.colour {
                Colour::Palette(i) => {
                    r.insert("color".into(), Value::Integer(*i as i64));
                }
                Colour::Named(n) => {
                    r.insert("color".into(), Value::String(n.clone()));
                }
                // `#rrggbb`: a `[colours]` name can never start with `#`
                // (`geode_core::colour::RESERVED_PREFIX`), so the string
                // form stays unambiguous.
                Colour::Custom(c) => {
                    r.insert("color".into(), Value::String(c.hex()));
                }
            }
            if s.axis != Axis::Left {
                r.insert("axis".into(), Value::String(s.axis.as_str().into()));
            }
            if !s.visible {
                r.insert("visible".into(), Value::Boolean(false));
            }
            Value::Table(r)
        })
        .collect();
    if !slots.is_empty() {
        t.insert("slots".into(), Value::Array(slots));
    }
    t.insert("range".into(), model.range().to_toml());
    t.insert(
        "frequency".into(),
        Value::String(model.frequency().as_str().into()),
    );
    t.insert(
        "axis".into(),
        Value::String(model.axis_mode().as_str().into()),
    );
    t.insert("split".into(), Value::Float(model.split() as f64));
    t.insert(
        "density".into(),
        match model.density() {
            Some(b) => Value::Integer(b as i64),
            None => Value::Boolean(false),
        },
    );
    t.insert(
        "percentiles".into(),
        Value::Array(
            model
                .percentiles()
                .iter()
                .map(|f| Value::Float(f * 100.0))
                .collect(),
        ),
    );
    t
}

pub fn from_table(
    t: &Table,
    dataset_of: &dyn Fn(&str) -> Option<String>,
    default_source: Option<&str>,
) -> (Model, Vec<String>) {
    let mut m = Model::new();
    let mut notices = Vec::new();
    let now = chrono::Utc::now();
    let live = geode_core::query::AsOf::Live;
    if let Some(f) = t
        .get("frequency")
        .and_then(Value::as_str)
        .and_then(Frequency::parse)
    {
        let _ = m.set_frequency(f, now, &live);
    }
    if let Some(r) = t.get("range").and_then(Range::from_toml) {
        let _ = m.set_range(r, now, &live);
    }
    if let Some(a) = t
        .get("axis")
        .and_then(Value::as_str)
        .and_then(AxisMode::parse)
    {
        m.set_axis_mode(a);
    }
    if let Some(s) = t.get("split").and_then(Value::as_float) {
        let _ = m.set_split(s as f32);
    }
    match t.get("density") {
        Some(Value::Integer(b)) if (MIN_BINS as i64..=MAX_BINS as i64).contains(b) => {
            let _ = m.set_density(Some(*b as u32));
        }
        Some(Value::Boolean(false)) => {
            let _ = m.set_density(None);
        }
        _ => {}
    }
    if let Some(p) = t.get("percentiles").and_then(Value::as_array) {
        let fractions: Vec<f64> = p
            .iter()
            .filter_map(|v| v.as_float().or_else(|| v.as_integer().map(|i| i as f64)))
            .map(|x| x / 100.0)
            .filter(|f| *f > 0.0 && *f < 1.0)
            .collect();
        let _ = m.set_percentiles(fractions);
    }
    // Sources first, then expressions in file order, so a handle resolves.
    let rows: Vec<&Table> = t
        .get("slots")
        .and_then(Value::as_array)
        .map(|a| a.iter().filter_map(Value::as_table).collect())
        .unwrap_or_default();
    let mut seen = Vec::new();
    let mut pending_exprs: Vec<(u8, &Table)> = Vec::new();
    for r in rows {
        let Some(number) = r
            .get("number")
            .and_then(Value::as_integer)
            .and_then(|n| u8::try_from(n).ok())
            .filter(|n| *n > 0)
        else {
            notices.push("a restored slot had no number and was dropped".into());
            continue;
        };
        if seen.contains(&number) {
            notices.push(format!("a second slot numbered s{number} was dropped"));
            continue;
        }
        seen.push(number);
        match r.get("kind").and_then(Value::as_str) {
            Some("source") => {
                let (Some(identity), Some(source)) = (
                    r.get("identity").and_then(Value::as_str),
                    r.get("source").and_then(Value::as_str),
                ) else {
                    notices.push(format!(
                        "s{number} names no identity or source and was dropped"
                    ));
                    continue;
                };
                let Some(dataset) = dataset_of(source) else {
                    notices.push(format!(
                        "s{number}: '{source}' is not a fetch source in this config; {identity}@{source} was dropped"
                    ));
                    continue;
                };
                m.set_next_number(number);
                match m.add_source(identity, source, &dataset) {
                    Ok(_) => {
                        if let Some(rule) = r
                            .get("rule")
                            .and_then(Value::as_str)
                            .and_then(BucketRule::parse)
                        {
                            let _ = m.set_rule(number, rule);
                        }
                        apply_look(&mut m, number, r);
                    }
                    Err(e) => notices.push(format!("s{number}: {e}")),
                }
            }
            Some("expr") => pending_exprs.push((number, r)),
            _ => notices.push(format!("s{number} has an unknown kind and was dropped")),
        }
    }
    for (number, r) in pending_exprs {
        let text = r.get("text").and_then(Value::as_str).unwrap_or("");
        match resolve(text, m.slots(), default_source, None) {
            Ok(expr) => {
                m.set_next_number(number);
                match m.add_expr(text, expr) {
                    Ok(_) => apply_look(&mut m, number, r),
                    Err(e) => {
                        notices.push(format!("expression s{number} `{text}` was dropped: {e}"))
                    }
                }
            }
            Err(e) => notices.push(format!("expression s{number} `{text}` was dropped: {e}")),
        }
    }
    if let Some(n) = m.take_notice() {
        notices.push(n);
    }
    m.set_next_number(
        seen.iter()
            .copied()
            .max()
            .map_or(1, |n| n.saturating_add(1)),
    );
    for s in m.slots_mut() {
        s.state = SlotState::Idle;
    }
    m.set_cursor(0);
    (m, notices)
}

fn apply_look(m: &mut Model, number: u8, r: &Table) {
    // `colour` is the key's spelling before the rename; the next save
    // rewrites it as `color`.
    match r.get("color").or_else(|| r.get("colour")) {
        Some(Value::Integer(i))
            if (0..geode_chart::core::palette::Palette::LEN as i64).contains(i) =>
        {
            let _ = m.set_colour(number, Colour::Palette(*i as usize));
        }
        // A malformed `#…` keeps the slot's default colour, as an
        // out-of-range palette index does.
        Some(Value::String(n)) if n.starts_with(geode_core::colour::RESERVED_PREFIX) => {
            if let Some(c) = Rgb8::parse_hex(n) {
                let _ = m.set_colour(number, Colour::Custom(c));
            }
        }
        Some(Value::String(n)) => {
            let _ = m.set_colour(number, Colour::Named(n.clone()));
        }
        _ => {}
    }
    if let Some(a) = r.get("axis").and_then(Value::as_str).and_then(Axis::parse) {
        let _ = m.set_axis(number, a);
    }
    if r.get("visible").and_then(Value::as_bool) == Some(false) {
        let _ = m.set_visible(number, false);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::{Colour, Model, Preset, Range, SlotState};
    use geode_chart::{Axis, AxisMode};
    use geode_core::series::{BucketRule, Frequency, SlotKind};

    fn dataset_of(s: &str) -> Option<String> {
        matches!(s, "demo_kdb" | "demo_rest").then(|| "series".to_string())
    }

    #[test]
    fn a_model_round_trips_through_its_table() {
        let mut m = Model::new();
        m.add_source("SPX.close", "demo_kdb", "series").unwrap();
        m.add_source("VIX", "demo_rest", "series").unwrap();
        m.set_rule(2, BucketRule::Mean).unwrap();
        m.set_axis(2, Axis::BottomRight).unwrap();
        m.set_colour(1, Colour::Named("spx".into())).unwrap();
        m.toggle_visible();
        let e = crate::core::resolve("s1 / s2", m.slots(), Some("demo_kdb"), None).unwrap();
        m.add_expr("s1 / s2", e).unwrap();
        m.set_range(
            Range::Relative(Preset::M6),
            chrono::Utc::now(),
            &Default::default(),
        )
        .unwrap();
        m.set_frequency(Frequency::H1, chrono::Utc::now(), &Default::default())
            .unwrap();
        m.set_axis_mode(AxisMode::Continuous);
        m.set_split(0.6).unwrap();
        m.set_density(Some(20)).unwrap();
        m.set_percentiles(vec![0.1, 0.9]).unwrap();
        let t = to_table(&m);
        let (mut back, notices) = from_table(&t, &dataset_of, Some("demo_kdb"));
        assert!(notices.is_empty(), "{notices:?}");
        assert_eq!(back.slots().len(), 3);
        assert_eq!(back.slots()[0].number, 1);
        assert_eq!(back.slots()[0].colour, Colour::Named("spx".into()));
        assert!(
            matches!(&back.slots()[1].kind, SlotKind::Source { rule: BucketRule::Mean, source, .. } if source == "demo_rest")
        );
        assert_eq!(back.slots()[1].axis, Axis::BottomRight);
        assert!(!back.slots()[1].visible);
        assert_eq!(back.slots()[2].text.as_deref(), Some("s1 / s2"));
        assert!(matches!(back.slots()[2].kind, SlotKind::Expr(_)));
        assert!(
            back.slots().iter().all(|s| s.state == SlotState::Idle),
            "slot state is not persisted (§9.11)"
        );
        assert_eq!(*back.range(), Range::Relative(Preset::M6));
        assert_eq!(back.frequency(), Frequency::H1);
        assert_eq!(back.axis_mode(), AxisMode::Continuous);
        assert!((back.split() - 0.6).abs() < 1e-6);
        assert_eq!(back.density(), Some(20));
        assert_eq!(back.percentiles(), &[0.1, 0.9][..]);
        assert_eq!(back.dataset(), Some("series"));
        assert_eq!(to_table(&back), t, "a second round trip is identical");
        assert_eq!(
            back.add_source("X", "demo_kdb", "series").unwrap().0,
            4,
            "numbering continues past the restored max"
        );
    }

    /// An absolute colour is written as lowercase `#rrggbb` and read
    /// back as `Custom`, never as a name; a malformed one keeps the
    /// slot's default colour.
    #[test]
    fn a_custom_colour_round_trips_as_hex() {
        let mut m = Model::new();
        m.add_source("SPX.close", "demo_kdb", "series").unwrap();
        m.add_source("VIX", "demo_kdb", "series").unwrap();
        m.set_colour(1, Colour::Custom(crate::core::Rgb8([0xff, 0x88, 0x00])))
            .unwrap();
        let t = to_table(&m);
        let slots = t["slots"].as_array().unwrap();
        assert_eq!(
            slots[0].as_table().unwrap()["color"].as_str(),
            Some("#ff8800")
        );
        let (back, _) = from_table(&t, &dataset_of, Some("demo_kdb"));
        assert_eq!(
            back.slots()[0].colour,
            Colour::Custom(crate::core::Rgb8([0xff, 0x88, 0x00]))
        );
        let text = r##"
            [[slots]]
            number = 1
            kind = "source"
            identity = "SPX.close"
            source = "demo_kdb"
            colour = "#ff88"
        "##;
        let t: toml::Table = toml::from_str(text).unwrap();
        let (back, _) = from_table(&t, &dataset_of, Some("demo_kdb"));
        assert_eq!(
            back.slots()[0].colour,
            Colour::Palette(0),
            "malformed hex is neither a colour nor a name"
        );
    }

    /// A session saved before the key was renamed still says `colour`: it is
    /// read, `color` wins beside it, and the next save writes `color` only.
    #[test]
    fn the_old_colour_key_is_read_and_rewritten_as_color() {
        let text = r##"
            [[slots]]
            number = 1
            kind = "source"
            identity = "SPX.close"
            source = "demo_kdb"
            colour = "#00ff00"
            [[slots]]
            number = 2
            kind = "source"
            identity = "VIX"
            source = "demo_kdb"
            colour = 4
            color = 2
        "##;
        let t: toml::Table = toml::from_str(text).unwrap();
        let (back, _) = from_table(&t, &dataset_of, Some("demo_kdb"));
        assert_eq!(
            back.slots()[0].colour,
            Colour::Custom(crate::core::Rgb8([0x00, 0xff, 0x00]))
        );
        assert_eq!(back.slots()[1].colour, Colour::Palette(2));
        let saved = to_table(&back);
        let first = saved["slots"].as_array().unwrap()[0].as_table().unwrap();
        assert_eq!(first["color"].as_str(), Some("#00ff00"));
        assert!(first.get("colour").is_none());
    }

    #[test]
    fn a_hostile_table_heals_rather_than_refuses() {
        let text = r#"
            frequency = "9h"
            axis = "sideways"
            split = 7.0
            density = 100000
            percentiles = [5, 150, "x"]
            [[slots]]
            number = 1
            kind = "source"
            identity = "SPX.close"
            source = "gone_src"
            [[slots]]
            number = 2
            kind = "source"
            identity = "VIX"
            source = "demo_kdb"
            axis = "right"
            colour = 99
            [[slots]]
            number = 2
            kind = "expr"
            text = "s1 / s2"
            [[slots]]
            number = 3
            kind = "expr"
            text = "s2 * s7"
        "#;
        let t: toml::Table = toml::from_str(text).unwrap();
        let (m, notices) = from_table(&t, &dataset_of, Some("demo_kdb"));
        assert_eq!(m.frequency(), Frequency::D1, "unknown → default");
        assert_eq!(m.axis_mode(), AxisMode::Session);
        assert_eq!(m.split(), 0.7);
        assert_eq!(m.density(), Some(40), "out of range → default");
        assert_eq!(
            m.percentiles(),
            &[0.05][..],
            "only the valid fraction survives"
        );
        let numbers: Vec<u8> = m.slots().iter().map(|s| s.number).collect();
        assert_eq!(
            numbers,
            vec![2],
            "unknown source dropped, duplicate number dropped, unresolvable expression dropped"
        );
        assert_eq!(
            m.slots()[0].colour,
            Colour::Palette(0),
            "99 is off the palette"
        );
        assert_eq!(notices.len(), 3, "{notices:?}");
        assert!(notices[0].contains("gone_src"));
        assert!(notices.iter().any(|n| n.contains("s2 * s7")));
    }

    #[test]
    fn an_empty_or_absent_table_is_a_fresh_model() {
        let (m, n) = from_table(&toml::Table::new(), &dataset_of, None);
        assert!(m.slots().is_empty() && n.is_empty());
        assert_eq!(m.frequency(), Frequency::D1);
    }

    #[test]
    fn an_expression_slot_numbered_255_heals_with_a_notice_instead_of_panicking() {
        let text = r#"
            [[slots]]
            number = 1
            kind = "source"
            identity = "SPX.close"
            source = "demo_kdb"
            [[slots]]
            number = 255
            kind = "expr"
            text = "s1 * 2"
        "#;
        let t: toml::Table = toml::from_str(text).unwrap();
        let (m, notices) = from_table(&t, &dataset_of, Some("demo_kdb"));
        assert_eq!(m.slots().len(), 1, "the source slot restores");
        assert_eq!(m.slots()[0].number, 1);
        assert_eq!(notices.len(), 1, "{notices:?}");
        assert!(
            notices[0].contains("255") || notices[0].contains("every slot number"),
            "{notices:?}"
        );
    }
}
