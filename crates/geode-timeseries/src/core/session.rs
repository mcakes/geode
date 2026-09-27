//! Session serialization for slots (kind, color, axis, visibility, rule,
//! expression text), range, frequency, axis mode, split, density, and percentiles.
//! Viewport bounds and transient slot state are not saved. `from_table` repairs
//! malformed input where possible and reports discarded data through notices
//! the tile shows once.
//!
//! A table with a missing version or `version < 2` may name slots by handle
//! (`s3`) in expression text. Restoring it rewrites each handle to a name: a
//! source slot's full `identity@source` (never the default-aware label,
//! which a later default change would retarget), or an expression
//! slot's own text in parentheses, recursively. A text with no handle
//! is read as current text. A text that cannot be rewritten (a cycle, a
//! handle to a slot the table no longer holds, a series no name can
//! pick out) keeps its slot, failed with the reason, and is written
//! back with `legacy = true` so a later restore tries again; numbering
//! continues past every slot number such a text names, so none is ever
//! reissued to a series the retry would then pick up.

use geode_chart::{Axis, AxisMode};
use geode_core::series::expr;
use geode_core::series::{BucketRule, Frequency, MAX_BINS, MIN_BINS, SlotKind};
use toml::{Table, Value};

use super::model::{Color, Model, SlotState};
use super::range::Range;
use super::resolve::{name_for, resolve};
use super::rgb::Rgb8;

/// Session format written by this crate. Versions below 2, including an
/// absent version, permit slot handles in expression text.
const VERSION: i64 = 2;

/// The longest text a handle rewrite may build. Inlining expressions
/// into expressions can double the text per level; the parser's token
/// bound would refuse the result anyway, and this stops the rewrite
/// before it allocates its way there.
const REWRITE_MAX: usize = 16 * 1024;

pub fn to_table(model: &Model) -> Table {
    let mut t = Table::new();
    t.insert("version".into(), Value::Integer(VERSION));
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
                    if s.legacy {
                        r.insert("legacy".into(), Value::Boolean(true));
                    }
                }
            }
            match &s.color {
                Color::Palette(i) => {
                    r.insert("color".into(), Value::Integer(*i as i64));
                }
                Color::Named(n) => {
                    r.insert("color".into(), Value::String(n.clone()));
                }
                // `#rrggbb`: a `[colors]` name can never start with `#`
                // (`geode_core::colour::RESERVED_PREFIX`), so the string
                // form stays unambiguous.
                Color::Custom(c) => {
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
    // Sources first, then expressions in file order, so every name an
    // expression uses (and every handle a legacy one rewrites) is loaded.
    let legacy_file = t.get("version").and_then(Value::as_integer).unwrap_or(1) < VERSION;
    let rows: Vec<&Table> = t
        .get("slots")
        .and_then(Value::as_array)
        .map(|a| a.iter().filter_map(Value::as_table).collect())
        .unwrap_or_default();
    let mut seen = Vec::new();
    let mut pending_exprs: Vec<PendingExpr> = Vec::new();
    for r in rows {
        let Some(number) = r
            .get("number")
            .and_then(Value::as_integer)
            .and_then(|n| u8::try_from(n).ok())
            .filter(|n| *n > 0)
        else {
            notices.push("a restored series had no slot number and was dropped".into());
            continue;
        };
        if seen.contains(&number) {
            notices.push(format!(
                "{} shared a slot number and was dropped",
                row_name(r)
            ));
            continue;
        }
        seen.push(number);
        match r.get("kind").and_then(Value::as_str) {
            Some("source") => {
                let (Some(identity), Some(source)) = (
                    r.get("identity").and_then(Value::as_str),
                    r.get("source").and_then(Value::as_str),
                ) else {
                    notices.push(
                        "a restored series named no identity or source and was dropped".into(),
                    );
                    continue;
                };
                let Some(dataset) = dataset_of(source) else {
                    notices.push(format!(
                        "'{source}' is not a fetch source in this config; {identity}@{source} was dropped"
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
                    Err(e) => notices.push(format!("{identity}@{source}: {e}")),
                }
            }
            Some("expr") => {
                let text = r.get("text").and_then(Value::as_str).unwrap_or("");
                // Only a text that holds a handle needs rewriting; one
                // already written in names resolves as current text does.
                let marked = legacy_file || r.get("legacy").and_then(Value::as_bool) == Some(true);
                pending_exprs.push(PendingExpr {
                    number,
                    text,
                    legacy: marked && !handles(text).is_empty(),
                    row: r,
                })
            }
            _ => notices.push("a restored series of unknown kind was dropped".into()),
        }
    }
    for p in &pending_exprs {
        let rewritten = if p.legacy {
            let mut visiting = vec![p.number];
            rewrite_handles(p.text, &pending_exprs, &m, &mut visiting)
                .and_then(|text| resolve(&text, m.slots(), default_source).map(|e| (text, e)))
                .map_err(|why| format!("could not rewrite this saved expression by name: {why}"))
        } else {
            resolve(p.text, m.slots(), default_source)
                .map(|e| (p.text.to_string(), e))
                .map_err(|why| format!("expression `{}` was dropped: {why}", p.text))
        };
        m.set_next_number(p.number);
        let added = match rewritten {
            Ok((text, expr)) => m.add_expr(&text, expr).map(|_| ()),
            // Keep failed handle rewrites for a later restore or edit.
            // Unresolvable name-based expressions are dropped with a notice.
            Err(why) if p.legacy => m.add_legacy_expr(p.text, why).map(|_| ()),
            Err(why) => {
                notices.push(why);
                continue;
            }
        };
        match added {
            Ok(()) => apply_look(&mut m, p.number, p.row),
            Err(e) => notices.push(format!("expression `{}` was dropped: {e}", p.text)),
        }
    }
    if let Some(n) = m.take_notice() {
        notices.push(n);
    }
    // A failed legacy text still names its operands by number and is
    // retried on every restore, so no number it names may be handed to a
    // new series: numbering continues past those as well as past every
    // saved slot. The text is saved as is, so this holds on every reload.
    let named_by_legacy = m
        .slots()
        .iter()
        .filter(|s| s.legacy)
        .flat_map(|s| handles(s.text.as_deref().unwrap_or("")));
    m.set_next_number(
        seen.iter()
            .copied()
            .chain(named_by_legacy)
            .max()
            .map_or(1, |n| n.saturating_add(1)),
    );
    for s in m.slots_mut() {
        if !s.legacy {
            s.state = SlotState::Idle;
        }
    }
    m.set_cursor(0);
    (m, notices)
}

/// An expression row waiting for the sources to load.
struct PendingExpr<'a> {
    number: u8,
    text: &'a str,
    /// Its text names slots by handle and must be rewritten.
    legacy: bool,
    row: &'a Table,
}

/// A dropped row's name for a notice: its pair, its text, or a plain
/// "a restored series".
fn row_name(r: &Table) -> String {
    let get = |k: &str| r.get(k).and_then(Value::as_str);
    match (get("identity"), get("source"), get("text")) {
        (Some(i), Some(s), _) => format!("{i}@{s}"),
        (_, _, Some(text)) => format!("expression `{text}`"),
        _ => "a restored series".into(),
    }
}

/// A legacy slot handle: `s` then digits that fit a `u8`, with no `@source`.
fn legacy_handle(r: &expr::RefName) -> Option<u8> {
    if r.source.is_some() {
        return None;
    }
    let digits = r.identity.strip_prefix('s')?;
    if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    digits.parse().ok()
}

/// The slot numbers `text` names by handle; none when it does not
/// tokenize.
fn handles(text: &str) -> Vec<u8> {
    expr::references(text)
        .map(|refs| refs.iter().filter_map(|(_, r)| legacy_handle(r)).collect())
        .unwrap_or_default()
}

/// `text` with every handle written as a name: a source slot's full pair
/// ([`name_for`]), an expression slot's own text in parentheses (itself
/// rewritten when it is legacy too). Other names pass through as typed.
/// `visiting` holds the expressions being inlined, the root first, so a
/// cycle is an error rather than a recursion that never ends.
fn rewrite_handles(
    text: &str,
    exprs: &[PendingExpr],
    m: &Model,
    visiting: &mut Vec<u8>,
) -> Result<String, String> {
    let refs = expr::references(text).map_err(|e| e.message)?;
    let mut out = String::with_capacity(text.len());
    let mut at = 0;
    for (span, r) in refs {
        let Some(n) = legacy_handle(&r) else {
            continue;
        };
        out.push_str(&text[at..span.start]);
        at = span.end;
        if matches!(
            m.slot_by_number(n).map(|s| &s.kind),
            Some(SlotKind::Source { .. })
        ) {
            out.push_str(&name_for(n, m.slots())?);
        } else if let Some(inner) = exprs.iter().find(|p| p.number == n) {
            if visiting.contains(&n) {
                return Err(if visiting.first() == Some(&n) {
                    "it references itself, directly or through another expression".into()
                } else {
                    "it references an expression that references itself".into()
                });
            }
            let inlined = if inner.legacy {
                visiting.push(n);
                let t = rewrite_handles(inner.text, exprs, m, visiting)?;
                visiting.pop();
                t
            } else {
                inner.text.to_string()
            };
            out.push('(');
            out.push_str(&inlined);
            out.push(')');
        } else {
            return Err("it references a series this session no longer holds".into());
        }
        if out.len() > REWRITE_MAX {
            return Err("it is too long once its references are written out".into());
        }
    }
    out.push_str(&text[at..]);
    Ok(out)
}

fn apply_look(m: &mut Model, number: u8, r: &Table) {
    // Accept the compatibility key `colour` only when `color` is absent.
    // Serialization always writes `color`.
    match r.get("color").or_else(|| r.get("colour")) {
        Some(Value::Integer(i))
            if (0..geode_chart::core::palette::Palette::LEN as i64).contains(i) =>
        {
            let _ = m.set_color(number, Color::Palette(*i as usize));
        }
        // A malformed `#…` keeps the slot's default colour, as an
        // out-of-range palette index does.
        Some(Value::String(n)) if n.starts_with(geode_core::colour::RESERVED_PREFIX) => {
            if let Some(c) = Rgb8::parse_hex(n) {
                let _ = m.set_color(number, Color::Custom(c));
            }
        }
        Some(Value::String(n)) => {
            let _ = m.set_color(number, Color::Named(n.clone()));
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
    use crate::core::{Color, Model, Preset, Range, SlotState};
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
        m.set_color(1, Color::Named("spx".into())).unwrap();
        m.toggle_visible();
        let e = crate::core::resolve("SPX.close / VIX", m.slots(), Some("demo_kdb")).unwrap();
        m.add_expr("SPX.close / VIX", e).unwrap();
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
        assert_eq!(back.slots()[0].color, Color::Named("spx".into()));
        assert!(
            matches!(&back.slots()[1].kind, SlotKind::Source { rule: BucketRule::Mean, source, .. } if source == "demo_rest")
        );
        assert_eq!(back.slots()[1].axis, Axis::BottomRight);
        assert!(!back.slots()[1].visible);
        assert_eq!(back.slots()[2].text.as_deref(), Some("SPX.close / VIX"));
        assert!(matches!(&back.slots()[2].kind, SlotKind::Expr(e) if e.slots() == vec![1, 2]));
        assert_eq!(t.get("version").and_then(|v| v.as_integer()), Some(2));
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
        m.set_color(1, Color::Custom(crate::core::Rgb8([0xff, 0x88, 0x00])))
            .unwrap();
        let t = to_table(&m);
        let slots = t["slots"].as_array().unwrap();
        assert_eq!(
            slots[0].as_table().unwrap()["color"].as_str(),
            Some("#ff8800")
        );
        let (back, _) = from_table(&t, &dataset_of, Some("demo_kdb"));
        assert_eq!(
            back.slots()[0].color,
            Color::Custom(crate::core::Rgb8([0xff, 0x88, 0x00]))
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
            back.slots()[0].color,
            Color::Palette(0),
            "malformed hex is neither a colour nor a name"
        );
    }

    /// The compatibility key `colour` is accepted, `color` takes precedence,
    /// and serialization writes `color` only.
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
            back.slots()[0].color,
            Color::Custom(crate::core::Rgb8([0x00, 0xff, 0x00]))
        );
        assert_eq!(back.slots()[1].color, Color::Palette(2));
        let saved = to_table(&back);
        let first = saved["slots"].as_array().unwrap()[0].as_table().unwrap();
        assert_eq!(first["color"].as_str(), Some("#00ff00"));
        assert!(first.get("colour").is_none());
    }

    #[test]
    fn a_hostile_table_heals_rather_than_refuses() {
        let text = r#"
            version = 2
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
            text = "SPX.close / VIX"
            [[slots]]
            number = 3
            kind = "expr"
            text = "VIX * V2X"
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
            m.slots()[0].color,
            Color::Palette(0),
            "99 is off the palette"
        );
        assert_eq!(notices.len(), 3, "{notices:?}");
        assert!(notices[0].contains("gone_src"));
        assert_eq!(
            notices[1],
            "expression `SPX.close / VIX` shared a slot number and was dropped"
        );
        assert_eq!(
            notices[2],
            "expression `VIX * V2X` was dropped: 'V2X' is not loaded — `a` adds it"
        );
    }

    fn slot_text(m: &Model, number: u8) -> Option<&str> {
        m.slot_by_number(number).and_then(|s| s.text.as_deref())
    }

    /// A session saved while expressions named slots by handle, shaped
    /// as the tile wrote it: every handle is rewritten to a name, an
    /// expression over an expression inlines the inner text, and a
    /// cycle or a dangling handle keeps its slot, failed. Saved again,
    /// the rewritten texts are final and the failed ones stay marked.
    #[test]
    fn a_handle_session_migrates_to_names() {
        let text = r#"
            range = "1y"
            frequency = "1d"
            axis = "session"
            split = 0.7
            density = 40
            percentiles = [5.0, 50.0, 95.0]
            [[slots]]
            number = 1
            kind = "source"
            identity = "SPX.close"
            source = "demo_kdb"
            colour = 0
            [[slots]]
            number = 2
            kind = "source"
            identity = "VIX"
            source = "demo_rest"
            colour = 1
            axis = "right"
            [[slots]]
            number = 3
            kind = "expr"
            text = "s1 / s2"
            colour = 2
            [[slots]]
            number = 4
            kind = "expr"
            text = "-s3*100 - s1"
            colour = 3
            axis = "bottomleft"
            [[slots]]
            number = 6
            kind = "expr"
            text = "s7 + 1"
            [[slots]]
            number = 7
            kind = "expr"
            text = "s6 * 2"
            [[slots]]
            number = 8
            kind = "expr"
            text = "s1 + s5"
            visible = false
            [[slots]]
            number = 9
            kind = "expr"
            text = "s7 - s1"
        "#;
        let t: toml::Table = toml::from_str(text).unwrap();
        let (m, notices) = from_table(&t, &dataset_of, Some("demo_kdb"));
        assert!(notices.is_empty(), "{notices:?}");
        let numbers: Vec<u8> = m.slots().iter().map(|s| s.number).collect();
        assert_eq!(numbers, vec![1, 2, 3, 4, 6, 7, 8, 9], "every slot is kept");

        assert_eq!(slot_text(&m, 3), Some("SPX.close@demo_kdb / VIX@demo_rest"));
        assert_eq!(
            slot_text(&m, 4),
            Some("-(SPX.close@demo_kdb / VIX@demo_rest)*100 - SPX.close@demo_kdb")
        );
        for n in [3, 4] {
            let s = m.slot_by_number(n).unwrap();
            assert!(!s.legacy && s.state == SlotState::Idle, "{n}: {s:?}");
        }
        assert!(
            matches!(&m.slot_by_number(4).unwrap().kind, SlotKind::Expr(e) if e.slots() == vec![1, 2])
        );
        assert_eq!(m.slot_by_number(4).unwrap().color, Color::Palette(3));
        assert_eq!(m.slot_by_number(4).unwrap().axis, Axis::BottomLeft);

        let failed = |n: u8| match &m.slot_by_number(n).unwrap().state {
            SlotState::Failed(why) => why.clone(),
            other => panic!("{n} is {other:?}"),
        };
        for n in [6, 7] {
            assert_eq!(
                failed(n),
                "could not rewrite this saved expression by name: it references itself, directly or through another expression"
            );
        }
        assert_eq!(
            failed(8),
            "could not rewrite this saved expression by name: it references a series this session no longer holds"
        );
        assert_eq!(
            failed(9),
            "could not rewrite this saved expression by name: it references an expression that references itself"
        );
        assert_eq!(
            slot_text(&m, 8),
            Some("s1 + s5"),
            "a failed text is kept as saved"
        );
        assert!(!m.slot_by_number(8).unwrap().visible);
        assert_eq!(
            m.label(m.index_of(8).unwrap(), Some("demo_kdb")),
            "s1 + s5",
            "the chip shows the saved text"
        );

        let again = to_table(&m);
        let (back, notices) = from_table(&again, &dataset_of, Some("demo_kdb"));
        assert!(notices.is_empty(), "{notices:?}");
        assert_eq!(slot_text(&back, 4), slot_text(&m, 4));
        assert!(
            back.slot_by_number(8).unwrap().legacy,
            "still marked, retried on restore"
        );
        assert_eq!(to_table(&back), again);
    }

    /// Only a legacy text is rewritten: in a current table `s1` is a name.
    #[test]
    fn a_current_table_reads_a_handle_shaped_name_as_a_name() {
        let text = r#"
            version = 2
            [[slots]]
            number = 1
            kind = "source"
            identity = "SPX.close"
            source = "demo_kdb"
            [[slots]]
            number = 2
            kind = "source"
            identity = "s1"
            source = "demo_kdb"
            [[slots]]
            number = 3
            kind = "expr"
            text = "s1 * 2"
        "#;
        let t: toml::Table = toml::from_str(text).unwrap();
        let (m, notices) = from_table(&t, &dataset_of, Some("demo_kdb"));
        assert!(notices.is_empty(), "{notices:?}");
        assert!(
            matches!(&m.slot_by_number(3).unwrap().kind, SlotKind::Expr(e) if e.slots() == vec![2])
        );
    }

    /// A handle to one of two slots holding the same pair has no name
    /// that picks it out, so the rewrite refuses rather than retarget.
    #[test]
    fn a_handle_to_a_pair_loaded_twice_is_not_rewritten() {
        let text = r#"
            [[slots]]
            number = 1
            kind = "source"
            identity = "VIX"
            source = "demo_kdb"
            [[slots]]
            number = 2
            kind = "source"
            identity = "VIX"
            source = "demo_kdb"
            rule = "mean"
            [[slots]]
            number = 3
            kind = "expr"
            text = "s2 * 2"
        "#;
        let t: toml::Table = toml::from_str(text).unwrap();
        let (m, _) = from_table(&t, &dataset_of, Some("demo_kdb"));
        let s = m.slot_by_number(3).unwrap();
        assert!(s.legacy);
        assert!(
            matches!(&s.state, SlotState::Failed(why) if why.ends_with("'VIX@demo_kdb' is ambiguous: VIX@demo_kdb (last) or VIX@demo_kdb (mean)")),
            "{:?}",
            s.state
        );
    }

    /// A failed legacy text still names its operands by slot number, so
    /// a number it names is never handed to a new series: reissued, the
    /// next restore would rewrite the handle to that series and plot it
    /// under an expression the trader never wrote.
    #[test]
    fn a_handle_a_failed_text_names_is_never_reissued() {
        let text = r#"
            [[slots]]
            number = 1
            kind = "source"
            identity = "SPX.close"
            source = "demo_kdb"
            [[slots]]
            number = 2
            kind = "expr"
            text = "s1 / s3"
            [[slots]]
            number = 3
            kind = "source"
            identity = "VIX"
            source = "gone_src"
        "#;
        let t: toml::Table = toml::from_str(text).unwrap();
        let (m, _) = from_table(&t, &dataset_of, Some("demo_kdb"));
        assert!(
            m.slot_by_number(3).is_none(),
            "the unknown source is dropped"
        );
        assert!(m.slot_by_number(2).unwrap().legacy);
        let (mut m, _) = from_table(&to_table(&m), &dataset_of, Some("demo_kdb"));
        let (n, _) = m.add_source("NKY.close", "demo_kdb", "series").unwrap();
        assert!(n > 3, "slot 3 is still named by a saved text, got {n}");
        let (back, notices) = from_table(&to_table(&m), &dataset_of, Some("demo_kdb"));
        assert!(notices.is_empty(), "{notices:?}");
        let s = back.slot_by_number(2).unwrap();
        assert!(s.legacy, "{s:?}");
        assert_eq!(s.text.as_deref(), Some("s1 / s3"));
        assert!(matches!(s.state, SlotState::Failed(_)));
    }

    /// A rewritten handle names its series by the full pair, so a later
    /// change of the default source cannot retarget it.
    #[test]
    fn a_rewritten_handle_survives_a_change_of_default_source() {
        let text = r#"
            [[slots]]
            number = 1
            kind = "source"
            identity = "VIX"
            source = "demo_kdb"
            [[slots]]
            number = 2
            kind = "source"
            identity = "VIX"
            source = "demo_rest"
            [[slots]]
            number = 3
            kind = "expr"
            text = "s1 * 2"
        "#;
        let t: toml::Table = toml::from_str(text).unwrap();
        let (m, _) = from_table(&t, &dataset_of, Some("demo_kdb"));
        assert_eq!(slot_text(&m, 3), Some("VIX@demo_kdb * 2"));
        let (back, notices) = from_table(&to_table(&m), &dataset_of, Some("demo_rest"));
        assert!(notices.is_empty(), "{notices:?}");
        assert!(
            matches!(&back.slot_by_number(3).unwrap().kind, SlotKind::Expr(e) if e.slots() == vec![1])
        );
    }

    /// An unversioned text with no handle in it is already written in
    /// names: it resolves as current text does, and an unresolvable one
    /// is dropped with the resolution's own reason.
    #[test]
    fn a_handle_free_unversioned_text_is_read_as_current() {
        let text = r#"
            [[slots]]
            number = 1
            kind = "source"
            identity = "VIX"
            source = "demo_kdb"
            [[slots]]
            number = 2
            kind = "expr"
            text = "VIX * 2"
            [[slots]]
            number = 3
            kind = "expr"
            text = "VIX * V2X"
        "#;
        let t: toml::Table = toml::from_str(text).unwrap();
        let (m, notices) = from_table(&t, &dataset_of, Some("demo_kdb"));
        assert_eq!(slot_text(&m, 2), Some("VIX * 2"));
        assert!(!m.slot_by_number(2).unwrap().legacy);
        assert!(m.slot_by_number(3).is_none());
        assert_eq!(
            notices,
            vec!["expression `VIX * V2X` was dropped: 'V2X' is not loaded — `a` adds it"]
        );
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
