//! A configured view as columns, and the board ask it implies.
//!
//! A view is data read from `vol_views.toml` ([`builtin`] ships the
//! default tabs): [`columns`] lists what it paints under the lens in force,
//! and [`ask`] derives the board request from the same columns, so a column
//! cannot read a measure the request omitted.

use std::time::Duration;

use geode_core::config::{LayerDoc, VOL_VIEWS_DOC, merge_docs};
use geode_core::schema::axis::tenor_days;
use geode_core::volboard::{Lookback, Measure, delta_text};
pub use geode_core::volviews::{ALL_TENORS, DELTAS, SPARK_30D};
use geode_core::volviews::{
    Cells, CompanionGlyph, LevelGlyph, MeasureText, VolColumn, VolView, VolViews, from_doc,
    measure_text, resolve,
};

use super::lens::{LensState, Shown};

pub const KIND: &str = "volwatch";
pub const TITLE: &str = "Vol";
pub const SURFACE: &str = "vol_surface";
pub const REALIZED: &str = "realized_vol";
pub const VARSWAP: &str = "varswap";

/// The shipped tabs. A desk or user `vol_views.toml` replaces a view by
/// name or adds beside these.
pub const BUILTIN_VOL_VIEWS: &str = r#"config_version = 1
default = "vol"

# ATFM vol across the tenor ladder, a 30-day sparkline and book vega.
[vol]
[[vol.columns]]
measure = "atfm 1W"
[[vol.columns]]
measure = "atfm 1M"
[[vol.columns]]
measure = "atfm 3M"
[[vol.columns]]
measure = "atfm 6M"
[[vol.columns]]
measure = "atfm 1Y"
[[vol.columns]]
measure = "atfm 2Y"
[[vol.columns]]
measure = "atfm 3Y"
[[vol.columns]]
measure = "atfm 5Y"
[[vol.columns]]
spark = "atfm 3M"
span = "30d"
[[vol.columns]]
kind = "vega"

# 25Δ skew across the ladder and the 3M wing.
[skew]
[[skew.columns]]
measure = "skew 1W"
[[skew.columns]]
measure = "skew 1M"
[[skew.columns]]
measure = "skew 3M"
[[skew.columns]]
measure = "skew 6M"
[[skew.columns]]
measure = "skew 1Y"
[[skew.columns]]
measure = "skew 2Y"
[[skew.columns]]
measure = "skew 3Y"
[[skew.columns]]
measure = "skew 5Y"
[[skew.columns]]
measure = "wing skew 3M"

# Varswap strikes across the ladder.
[varswap]
[[varswap.columns]]
measure = "varswap 1W"
[[varswap.columns]]
measure = "varswap 1M"
[[varswap.columns]]
measure = "varswap 3M"
[[varswap.columns]]
measure = "varswap 6M"
[[varswap.columns]]
measure = "varswap 1Y"
[[varswap.columns]]
measure = "varswap 2Y"
[[varswap.columns]]
measure = "varswap 3Y"
[[varswap.columns]]
measure = "varswap 5Y"

# The surface in one line: five deltas at five tenors, one number per
# cell; `m` swaps the level for its companion.
[surface]
cells = "single"
[[surface.columns]]
measure = "point 1M 0.05"
[[surface.columns]]
measure = "point 1M 0.25"
[[surface.columns]]
measure = "point 1M 0.5"
[[surface.columns]]
measure = "point 1M 0.75"
[[surface.columns]]
measure = "point 1M 0.95"
[[surface.columns]]
measure = "point 3M 0.05"
[[surface.columns]]
measure = "point 3M 0.25"
[[surface.columns]]
measure = "point 3M 0.5"
[[surface.columns]]
measure = "point 3M 0.75"
[[surface.columns]]
measure = "point 3M 0.95"
[[surface.columns]]
measure = "point 6M 0.05"
[[surface.columns]]
measure = "point 6M 0.25"
[[surface.columns]]
measure = "point 6M 0.5"
[[surface.columns]]
measure = "point 6M 0.75"
[[surface.columns]]
measure = "point 6M 0.95"
[[surface.columns]]
measure = "point 1Y 0.05"
[[surface.columns]]
measure = "point 1Y 0.25"
[[surface.columns]]
measure = "point 1Y 0.5"
[[surface.columns]]
measure = "point 1Y 0.75"
[[surface.columns]]
measure = "point 1Y 0.95"
[[surface.columns]]
measure = "point 2Y 0.05"
[[surface.columns]]
measure = "point 2Y 0.25"
[[surface.columns]]
measure = "point 2Y 0.5"
[[surface.columns]]
measure = "point 2Y 0.75"
[[surface.columns]]
measure = "point 2Y 0.95"
"#;

/// The builtin tabs, resolved with no table datasets (none is named).
pub fn builtin() -> VolViews {
    let layer = LayerDoc::builtin(VOL_VIEWS_DOC, BUILTIN_VOL_VIEWS)
        .expect("BUILTIN_VOL_VIEWS is well-formed TOML");
    let (doc, diags) = from_doc(&merge_docs(VOL_VIEWS_DOC, &[layer]));
    debug_assert!(diags.is_empty(), "{diags:?}");
    let (views, diags) = resolve(&doc, &|_| None);
    debug_assert!(diags.is_empty(), "{diags:?}");
    views
}

/// What a column paints. Every variant but `Underlying`, `Spark` and
/// `Vega` names its board measure; a measure column carries its glyphs
/// resolved, so the model never asks what the file's default was.
#[derive(Debug, Clone, PartialEq)]
pub enum ColumnKind {
    Underlying,
    /// The level of a pair view, plain unless the file says otherwise.
    Level {
        measure: Measure,
        level: LevelGlyph,
    },
    /// The pair view's companion: the Δ, z or rank the lens picks, as a
    /// chip unless the file says otherwise.
    Companion {
        measure: Measure,
        companion: CompanionGlyph,
    },
    /// A single view's one number: the level (over the companion's sign
    /// tint by default), or the companion itself after `m`.
    Single {
        measure: Measure,
        level: LevelGlyph,
        companion: CompanionGlyph,
    },
    Spark {
        measure: Measure,
        span: Duration,
    },
    Vega,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Column {
    pub id: String,
    pub group: Option<String>,
    pub head: String,
    pub kind: ColumnKind,
}

/// Whether a column of `kind` takes a sort: `s`, a header press and
/// `:sort` all refuse the rest with `<head> is not sortable`.
pub fn sortable(kind: &ColumnKind) -> bool {
    !matches!(kind, ColumnKind::Underlying | ColumnKind::Spark { .. })
}

/// The tenors a list's surface holds, ordered by `tenor_days` with
/// unparsable labels last (lexically among themselves); `None` until listed
/// gives `ALL_TENORS`.
pub fn order_tenors(listed: Option<&[String]>) -> Vec<String> {
    let Some(listed) = listed else {
        return ALL_TENORS.iter().map(|t| t.to_string()).collect();
    };
    let mut tenors = listed.to_vec();
    tenors.sort_by(|a, b| {
        let (da, db) = (tenor_days(a), tenor_days(b));
        (da.is_none(), da, a).cmp(&(db.is_none(), db, b))
    });
    tenors.dedup();
    tenors
}

fn text_of(m: &Measure) -> String {
    match m {
        Measure::Table { .. } => m.name(),
        _ => measure_text(&MeasureText::Builtin(m.clone())),
    }
}

/// A column id stem: the measure text with its separators as dashes.
pub fn measure_id(m: &Measure) -> String {
    text_of(m).replace([' ', '/'], "-")
}

/// A measure as the desk names it: the group head and the readout.
pub fn family_label(m: &Measure) -> String {
    match m {
        Measure::Point { tenor, delta } if *delta == 0.5 => format!("ATFM {tenor}"),
        Measure::Point { tenor, delta } => format!("{tenor} {}\u{394}", delta_text(*delta)),
        Measure::Forward { tenor } => format!("fwd {tenor}"),
        Measure::Skew { tenor } => format!("25\u{394} skew {tenor}"),
        Measure::WingSkew { tenor } => format!("10\u{394} skew {tenor}"),
        Measure::Slope { long, short } => format!("{long}\u{2212}{short}"),
        Measure::Realized { window } => format!("rlz {window}"),
        Measure::IvRv { tenor, window } => format!("IV {tenor} \u{2212} RV {window}"),
        Measure::Varswap { tenor } => format!("vs {tenor}"),
        Measure::VsSpread { tenor } => format!("vs spread {tenor}"),
        Measure::Table { .. } => m.name(),
    }
}

/// The level's unit word, the pair sub-head.
pub fn unit(m: &Measure) -> &'static str {
    match m {
        Measure::Point { .. } | Measure::Forward { .. } | Measure::Realized { .. } => "vol",
        Measure::Skew { .. }
        | Measure::WingSkew { .. }
        | Measure::Slope { .. }
        | Measure::IvRv { .. }
        | Measure::VsSpread { .. } => "pts",
        Measure::Varswap { .. } => "strike",
        Measure::Table { .. } => "value",
    }
}

/// `30d`: a span in whole days.
pub fn span_label(d: Duration) -> String {
    format!("{}d", d.as_secs() / 86_400)
}

/// A call delta as the desk says it: `0.05` is `5Δ`, `0.5` `50Δ`.
fn delta_head(delta: f64) -> String {
    format!("{}\u{394}", (delta * 100.0).round())
}

/// The columns of `view` under `lens`, `Underlying` first; a `Vega`
/// column only with `vega` (the setting names a view and the query was
/// not refused). A single view whose measure columns are all surface
/// points groups by tenor and heads by delta; with the companion shown the
/// group head says which.
pub fn columns(view: &VolView, lens: &LensState, vega: bool) -> Vec<Column> {
    let mut cols = vec![Column {
        id: "underlying".into(),
        group: None,
        head: "Underlying".into(),
        kind: ColumnKind::Underlying,
    }];
    let all_points = view.columns.iter().all(|c| match c {
        VolColumn::Measure { measure, .. } => matches!(measure, Measure::Point { .. }),
        _ => true,
    });
    for c in &view.columns {
        match c {
            VolColumn::Measure {
                measure,
                label,
                group,
                level,
                companion,
            } => {
                let stem = measure_id(measure);
                let companion = companion.unwrap_or(CompanionGlyph::Chip);
                match view.cells {
                    Cells::Pair => {
                        let g = group.clone().unwrap_or_else(|| family_label(measure));
                        cols.push(Column {
                            id: format!("{stem}-level"),
                            group: Some(g.clone()),
                            head: label.clone().unwrap_or_else(|| unit(measure).into()),
                            kind: ColumnKind::Level {
                                measure: measure.clone(),
                                level: level.unwrap_or(LevelGlyph::Plain),
                            },
                        });
                        cols.push(Column {
                            id: format!("{stem}-companion"),
                            group: Some(g),
                            head: lens.companion_head().into(),
                            kind: ColumnKind::Companion {
                                measure: measure.clone(),
                                companion,
                            },
                        });
                    }
                    Cells::Single => {
                        let (g, h) = match measure {
                            Measure::Point { tenor, delta } if all_points => {
                                (tenor.clone(), delta_head(*delta))
                            }
                            _ => (family_label(measure), unit(measure).into()),
                        };
                        let mut g = group.clone().unwrap_or(g);
                        if lens.shown == Shown::Companion {
                            g = format!("{g} \u{b7} {}", lens.companion_head());
                        }
                        cols.push(Column {
                            id: format!("{stem}-single"),
                            group: Some(g),
                            head: label.clone().unwrap_or(h),
                            kind: ColumnKind::Single {
                                measure: measure.clone(),
                                level: level.unwrap_or(LevelGlyph::Tint),
                                companion,
                            },
                        });
                    }
                }
            }
            VolColumn::Spark { measure, span } => cols.push(Column {
                id: format!("spark-{}", measure_id(measure)),
                group: None,
                head: format!("{} \u{b7} {}", family_label(measure), span_label(*span)),
                kind: ColumnKind::Spark {
                    measure: measure.clone(),
                    span: *span,
                },
            }),
            VolColumn::Vega if vega => cols.push(Column {
                id: "vega".into(),
                group: None,
                head: "vega k".into(),
                kind: ColumnKind::Vega,
            }),
            VolColumn::Vega => {}
        }
    }
    cols
}

/// What a board request must carry.
#[derive(Debug, Clone, PartialEq)]
pub struct Ask {
    pub measures: Vec<Measure>,
    pub lookback: Lookback,
    pub window: Option<Duration>,
    pub sparklines: Vec<(Measure, Duration)>,
}

impl Default for Ask {
    fn default() -> Self {
        Ask {
            measures: Vec::new(),
            lookback: Lookback::D1,
            window: None,
            sparklines: Vec::new(),
        }
    }
}

impl Ask {
    /// The table datasets the measures and sparklines read, distinct.
    pub fn tables(&self) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        for m in self
            .measures
            .iter()
            .chain(self.sparklines.iter().map(|(m, _)| m))
        {
            if let Measure::Table { dataset, .. } = m
                && !out.contains(dataset)
            {
                out.push(dataset.clone());
            }
        }
        out
    }
}

fn skew(t: &str) -> Measure {
    Measure::Skew { tenor: t.into() }
}

fn realized(w: &str) -> Measure {
    Measure::Realized { window: w.into() }
}

/// The measures the inspection band reads: every surface point, each
/// tenor's skew, and the 3M rich/cheap set.
pub fn band_measures(tenors: &[String]) -> Vec<Measure> {
    let mut out = Vec::new();
    for t in tenors {
        for delta in DELTAS {
            out.push(Measure::Point {
                tenor: t.clone(),
                delta,
            });
        }
        out.push(skew(t));
    }
    out.push(realized("20D"));
    out.push(Measure::IvRv {
        tenor: "3M".into(),
        window: "20D".into(),
    });
    out.push(Measure::Varswap { tenor: "3M".into() });
    out.push(Measure::VsSpread { tenor: "3M".into() });
    out.push(Measure::atfm("3M"));
    out
}

fn push_unique(into: &mut Vec<Measure>, m: Measure) {
    if !into.contains(&m) {
        into.push(m);
    }
}

/// Add a sparkline; a measure already present keeps the longer span (a
/// spark column decimates its tail from the band's longer series).
fn merge_spark(into: &mut Vec<(Measure, Duration)>, m: Measure, span: Duration) {
    match into.iter_mut().find(|(have, _)| *have == m) {
        Some((_, have)) => *have = (*have).max(span),
        None => into.push((m, span)),
    }
}

/// The union of what the view's measure columns read, the window in
/// force whenever any measure column exists (the readout shows z and the
/// rank under either lens, so a lens flip never requeries), each spark,
/// and the band's measures and three sparklines at the window while open.
/// `Vega` asks nothing of the board.
pub fn ask(view: &VolView, lens: &LensState, tenors: &[String], band: bool) -> Ask {
    let mut measures = Vec::new();
    let mut window = None;
    let mut sparklines = Vec::new();
    for c in &view.columns {
        match c {
            VolColumn::Measure { measure, .. } => {
                push_unique(&mut measures, measure.clone());
                window = Some(lens.window.duration());
            }
            VolColumn::Spark { measure, span } => {
                merge_spark(&mut sparklines, measure.clone(), *span)
            }
            VolColumn::Vega => {}
        }
    }
    if band {
        for m in band_measures(tenors) {
            push_unique(&mut measures, m);
        }
        window = Some(lens.window.duration());
        for m in [Measure::atfm("3M"), skew("3M"), realized("20D")] {
            merge_spark(&mut sparklines, m, lens.window.duration());
        }
    }
    Ask {
        measures,
        lookback: lens.lookback,
        window,
        sparklines,
    }
}

/// True when `next` asks anything `last` did not: a different lookback; a
/// window the last ask lacked or a different one (dropping the window never
/// requeries); a measure `last` lacks; or a sparkline for which no `last`
/// entry has the same measure with a span at least as long (the model slices
/// the tail of a longer series). A narrower ask never requeries.
pub fn needs_requery(last: &Ask, next: &Ask) -> bool {
    next.lookback != last.lookback
        || next.window.is_some_and(|w| last.window != Some(w))
        || next.measures.iter().any(|m| !last.measures.contains(m))
        || next
            .sparklines
            .iter()
            .any(|(m, d)| !last.sparklines.iter().any(|(lm, ld)| lm == m && ld >= d))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::lens::{Lens, Metric, Shown, Window};
    use geode_core::volviews::{VolColumnSpec, VolViewSpec, VolViewsDoc};

    fn view_of(name: &str, cells: Cells, measures: &[&str]) -> VolView {
        let doc = VolViewsDoc {
            views: vec![VolViewSpec {
                name: name.into(),
                cells,
                columns: measures
                    .iter()
                    .map(|m| VolColumnSpec::Measure {
                        measure: geode_core::volviews::parse_measure(m).unwrap(),
                        label: None,
                        group: None,
                        level: None,
                        companion: None,
                    })
                    .collect(),
            }],
            default: None,
        };
        let (v, d) = resolve(&doc, &|name| {
            (name == "ssr").then_some(geode_core::volviews::TableShape {
                key_len: 2,
                values: 1,
            })
        });
        assert!(d.is_empty(), "{d:?}");
        v.views.into_iter().next().unwrap()
    }

    #[test]
    fn the_builtin_document_ships_four_tabs_opening_on_vol() {
        let b = builtin();
        let names: Vec<&str> = b.views.iter().map(|v| v.name.as_str()).collect();
        assert_eq!(names, ["vol", "skew", "varswap", "surface"]);
        assert_eq!(b.default, 0);
        let vol = b.get("vol").unwrap();
        assert_eq!(vol.cells, Cells::Pair);
        assert_eq!(vol.columns.len(), 10, "eight tenors, a spark and vega");
        assert!(matches!(vol.columns[8], VolColumn::Spark { .. }));
        assert_eq!(vol.columns[9], VolColumn::Vega);
        let surface = b.get("surface").unwrap();
        assert_eq!(surface.cells, Cells::Single);
        assert_eq!(surface.columns.len(), 25, "five tenors by five deltas");
        let skew = b.get("skew").unwrap();
        assert_eq!(skew.columns.len(), 9, "eight tenors and the 3M wing");
        assert_eq!(b.get("varswap").unwrap().columns.len(), 8);
    }

    #[test]
    fn a_pair_view_lays_a_level_and_a_companion_under_one_group_head() {
        let v = view_of("vol", Cells::Pair, &["atfm 3M", "skew 3M", "ssr 3M"]);
        let lens = LensState::default();
        let cols = columns(&v, &lens, true);
        let ids: Vec<&str> = cols.iter().map(|c| c.id.as_str()).collect();
        assert_eq!(
            ids,
            [
                "underlying",
                "atfm-3M-level",
                "atfm-3M-companion",
                "skew-3M-level",
                "skew-3M-companion",
                "ssr-3M-level",
                "ssr-3M-companion"
            ]
        );
        assert_eq!(cols[1].group.as_deref(), Some("ATFM 3M"));
        assert_eq!(cols[1].head, "vol");
        assert_eq!(cols[2].head, "\u{394}");
        assert_eq!(cols[3].head, "pts");
        assert_eq!(cols[5].group.as_deref(), Some("ssr 3M"));
        assert_eq!(cols[5].head, "value");
        assert!(matches!(cols[2].kind, ColumnKind::Companion { .. }));
        let rich = LensState {
            lens: Lens::Rich,
            metric: Metric::Pctl,
            ..lens
        };
        assert_eq!(columns(&v, &rich, true)[2].head, "pctl");
        assert!(
            columns(&v, &lens, true)
                .iter()
                .all(|c| c.kind != ColumnKind::Vega),
            "no vega column without a vega entry"
        );
    }

    #[test]
    fn a_single_surface_view_groups_by_tenor_and_heads_by_delta() {
        let v = view_of(
            "surface",
            Cells::Single,
            &[
                "point 1M 0.05",
                "point 1M 0.5",
                "point 1M 0.95",
                "point 3M 0.5",
            ],
        );
        let mut lens = LensState::default();
        let cols = columns(&v, &lens, false);
        assert_eq!(cols[1].id, "point-1M-0.05-single");
        assert_eq!(cols[1].group.as_deref(), Some("1M"));
        assert_eq!(cols[1].head, "5\u{394}");
        assert_eq!(cols[2].head, "50\u{394}");
        assert_eq!(cols[3].head, "95\u{394}");
        assert_eq!(cols[4].group.as_deref(), Some("3M"));
        assert!(matches!(cols[1].kind, ColumnKind::Single { .. }));
        lens.shown = Shown::Companion;
        assert_eq!(
            columns(&v, &lens, false)[1].group.as_deref(),
            Some("1M \u{b7} \u{394}")
        );
        lens.lens = Lens::Rich;
        assert_eq!(
            columns(&v, &lens, false)[1].group.as_deref(),
            Some("1M \u{b7} z")
        );
        // A single view of mixed families keeps the family heads.
        let mixed = view_of("mix", Cells::Single, &["atfm 3M", "skew 3M"]);
        let cols = columns(&mixed, &LensState::default(), false);
        assert_eq!(cols[1].group.as_deref(), Some("ATFM 3M"));
        assert_eq!(cols[1].head, "vol");
    }

    #[test]
    fn label_and_group_overrides_win_and_a_spark_names_its_span() {
        let doc = VolViewsDoc {
            views: vec![VolViewSpec {
                name: "v".into(),
                cells: Cells::Pair,
                columns: vec![
                    VolColumnSpec::Measure {
                        measure: geode_core::volviews::parse_measure("atfm 3M").unwrap(),
                        label: Some("3m vol".into()),
                        group: Some("ATM".into()),
                        level: None,
                        companion: None,
                    },
                    VolColumnSpec::Spark {
                        measure: geode_core::volviews::parse_measure("atfm 3M").unwrap(),
                        span: Duration::from_secs(30 * 86_400),
                    },
                    VolColumnSpec::Vega,
                ],
            }],
            default: None,
        };
        let (v, _) = resolve(&doc, &|_| None);
        let cols = columns(&v.views[0], &LensState::default(), true);
        assert_eq!(cols[1].group.as_deref(), Some("ATM"));
        assert_eq!(cols[1].head, "3m vol");
        assert_eq!(cols[3].id, "spark-atfm-3M");
        assert_eq!(cols[3].head, "ATFM 3M \u{b7} 30d");
        assert_eq!(cols[4].id, "vega");
        assert_eq!(cols[4].head, "vega k");
        assert!(!sortable(&cols[3].kind) && !sortable(&cols[0].kind) && sortable(&cols[4].kind));
    }

    #[test]
    fn the_columns_carry_the_files_glyphs_or_the_views_defaults() {
        use geode_core::volviews::{CompanionGlyph, LevelGlyph};
        let glyphs =
            |level: Option<LevelGlyph>, companion: Option<CompanionGlyph>| VolColumnSpec::Measure {
                measure: geode_core::volviews::parse_measure("atfm 3M").unwrap(),
                label: None,
                group: None,
                level,
                companion,
            };
        let doc = |cells: Cells| VolViewsDoc {
            views: vec![VolViewSpec {
                name: "v".into(),
                cells,
                columns: vec![
                    glyphs(None, None),
                    glyphs(Some(LevelGlyph::Heat), Some(CompanionGlyph::Slider)),
                ],
            }],
            default: None,
        };
        let (pair, _) = resolve(&doc(Cells::Pair), &|_| None);
        let cols = columns(&pair.views[0], &LensState::default(), false);
        assert!(matches!(
            cols[1].kind,
            ColumnKind::Level {
                level: LevelGlyph::Plain,
                ..
            }
        ));
        assert!(matches!(
            cols[2].kind,
            ColumnKind::Companion {
                companion: CompanionGlyph::Chip,
                ..
            }
        ));
        assert!(matches!(
            cols[3].kind,
            ColumnKind::Level {
                level: LevelGlyph::Heat,
                ..
            }
        ));
        assert!(matches!(
            cols[4].kind,
            ColumnKind::Companion {
                companion: CompanionGlyph::Slider,
                ..
            }
        ));
        let (single, _) = resolve(&doc(Cells::Single), &|_| None);
        let cols = columns(&single.views[0], &LensState::default(), false);
        assert!(matches!(
            cols[1].kind,
            ColumnKind::Single {
                level: LevelGlyph::Tint,
                companion: CompanionGlyph::Chip,
                ..
            }
        ));
        assert!(matches!(
            cols[2].kind,
            ColumnKind::Single {
                level: LevelGlyph::Heat,
                companion: CompanionGlyph::Slider,
                ..
            }
        ));
    }

    #[test]
    fn a_missing_tenor_keeps_its_blank_column() {
        // The document, not the surface's listing, lays out the columns:
        // a tenor the surface lacks is a blank column, not a shifted grid.
        let v = view_of("vol", Cells::Pair, &["atfm 3M", "atfm 5Y"]);
        let tenors = order_tenors(Some(&["1M".to_string(), "3M".to_string()]));
        let cols = columns(&v, &LensState::default(), false);
        assert_eq!(cols.len(), 5);
        assert_eq!(cols[3].id, "atfm-5Y-level");
        let a = ask(&v, &LensState::default(), &tenors, false);
        assert!(a.measures.contains(&Measure::atfm("5Y")));
    }

    #[test]
    fn the_ask_is_the_union_with_the_window_in_force_and_the_tables_read() {
        let v = view_of("vol", Cells::Pair, &["atfm 3M", "ssr 3M", "atfm 3M"]);
        let lens = LensState::default();
        let a = ask(&v, &lens, &order_tenors(None), false);
        assert_eq!(
            a.measures,
            vec![
                Measure::atfm("3M"),
                Measure::Table {
                    dataset: "ssr".into(),
                    suffix: vec!["3M".into()]
                }
            ]
        );
        assert_eq!(a.lookback, Lookback::D1);
        assert_eq!(
            a.window,
            Some(Window::Y1.duration()),
            "the window is asked under every lens so r never requeries"
        );
        assert_eq!(a.tables(), vec!["ssr".to_string()]);
        assert!(a.sparklines.is_empty());
        let w6 = LensState {
            window: Window::M6,
            ..lens
        };
        assert_eq!(
            ask(&v, &w6, &order_tenors(None), false).window,
            Some(Window::M6.duration())
        );
    }

    #[test]
    fn a_lens_or_shown_flip_never_requeries_but_a_window_change_does() {
        let v = view_of("vol", Cells::Single, &["atfm 3M"]);
        let t = order_tenors(None);
        let base = ask(&v, &LensState::default(), &t, false);
        let rich = LensState {
            lens: Lens::Rich,
            metric: Metric::Pctl,
            shown: Shown::Companion,
            ..LensState::default()
        };
        assert!(!needs_requery(&base, &ask(&v, &rich, &t, false)));
        let w = LensState {
            window: Window::Y2,
            ..LensState::default()
        };
        assert!(needs_requery(&base, &ask(&v, &w, &t, false)));
        let d = LensState {
            lookback: Lookback::W1,
            ..LensState::default()
        };
        assert!(needs_requery(&base, &ask(&v, &d, &t, false)));
    }

    #[test]
    fn the_band_joins_the_ask_at_the_window_in_force_and_closing_never_requeries() {
        let v = view_of("vol", Cells::Pair, &["atfm 3M"]);
        let t = order_tenors(None);
        let lens = LensState {
            window: Window::Y2,
            ..LensState::default()
        };
        let closed = ask(&v, &lens, &t, false);
        let open = ask(&v, &lens, &t, true);
        assert!(open.measures.len() > closed.measures.len());
        assert_eq!(open.window, Some(Window::Y2.duration()));
        assert_eq!(open.sparklines.len(), 3);
        assert!(
            open.sparklines
                .iter()
                .all(|(_, span)| *span == Window::Y2.duration())
        );
        assert!(needs_requery(&closed, &open));
        assert!(!needs_requery(&open, &closed));
    }

    #[test]
    fn measure_ids_and_units_are_pinned() {
        assert_eq!(measure_id(&Measure::atfm("3M")), "atfm-3M");
        assert_eq!(
            measure_id(&Measure::Point {
                tenor: "3M".into(),
                delta: 0.25
            }),
            "point-3M-0.25"
        );
        assert_eq!(
            measure_id(&Measure::IvRv {
                tenor: "3M".into(),
                window: "20D".into()
            }),
            "iv-rv-3M-20D"
        );
        assert_eq!(
            measure_id(&Measure::Slope {
                long: "1Y".into(),
                short: "1M".into()
            }),
            "slope-1Y-1M"
        );
        assert_eq!(
            measure_id(&Measure::Table {
                dataset: "ssr".into(),
                suffix: vec!["3M".into()]
            }),
            "ssr-3M"
        );
        assert_eq!(unit(&Measure::atfm("3M")), "vol");
        assert_eq!(unit(&Measure::Skew { tenor: "3M".into() }), "pts");
        assert_eq!(unit(&Measure::Varswap { tenor: "3M".into() }), "strike");
        assert_eq!(
            unit(&Measure::Table {
                dataset: "ssr".into(),
                suffix: vec![]
            }),
            "value"
        );
        assert_eq!(span_label(Duration::from_secs(30 * 86_400)), "30d");
    }
}
