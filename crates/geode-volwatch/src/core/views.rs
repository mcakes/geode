//! The five views as column tables, and the board ask each implies.
//!
//! A view is data: [`columns`] lists what it paints, and [`ask`] derives
//! the board request from those columns, so a column cannot read a measure
//! the request omitted.

use std::time::Duration;

use geode_core::schema::axis::tenor_days;
use geode_core::volboard::{Lookback, Measure};

pub const KIND: &str = "volwatch";
pub const TITLE: &str = "Vol";
pub const SURFACE: &str = "vol_surface";
pub const REALIZED: &str = "realized_vol";
pub const VARSWAP: &str = "varswap";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum View {
    Changes,
    Levels,
    RichCheap,
    Term,
    Skew,
}

impl View {
    pub const ALL: [View; 5] = [
        View::Changes,
        View::Levels,
        View::RichCheap,
        View::Term,
        View::Skew,
    ];

    /// 1..=5 in `ALL` order.
    pub fn digit(self) -> u8 {
        Self::ALL
            .iter()
            .position(|v| *v == self)
            .map_or(0, |i| i as u8 + 1)
    }

    pub fn from_digit(d: u8) -> Option<View> {
        usize::from(d)
            .checked_sub(1)
            .and_then(|i| Self::ALL.get(i))
            .copied()
    }

    /// The next view in `ALL` order (tab), wrapping.
    pub fn next(self) -> View {
        Self::ALL[usize::from(self.digit()) % Self::ALL.len()]
    }

    pub fn label(self) -> &'static str {
        match self {
            View::Changes => "Changes",
            View::Levels => "Levels",
            View::RichCheap => "Rich / cheap",
            View::Term => "Term",
            View::Skew => "Skew",
        }
    }

    /// The session word.
    pub fn key(self) -> &'static str {
        match self {
            View::Changes => "changes",
            View::Levels => "levels",
            View::RichCheap => "richcheap",
            View::Term => "term",
            View::Skew => "skew",
        }
    }

    pub fn parse(s: &str) -> Option<View> {
        Self::ALL.into_iter().find(|v| v.key() == s)
    }
}

pub const ATFM_TENORS: [&str; 5] = ["1M", "3M", "6M", "1Y", "2Y"];
pub const ALL_TENORS: [&str; 8] = ["1M", "2M", "3M", "6M", "1Y", "18M", "2Y", "3Y"];
pub const WINDOWS: [&str; 2] = ["5D", "20D"];
pub const DELTAS: [f64; 5] = [0.10, 0.25, 0.50, 0.75, 0.90];
pub const WINDOW_1Y: Duration = Duration::from_secs(365 * 86_400);
pub const SPARK_30D: Duration = Duration::from_secs(30 * 86_400);
pub const PCTL_MIN_COUNT: usize = 60;

/// Whether a column of `kind` takes a sort: `s`, a header press and
/// `:sort` all refuse the rest with `<head> is not sortable`.
pub fn sortable(kind: &ColumnKind) -> bool {
    !matches!(
        kind,
        ColumnKind::Underlying | ColumnKind::Spark { .. } | ColumnKind::Term
    )
}

/// What a column paints. Every variant but `Underlying`, `Spark`, `Term` and
/// `Vega` names its board measure.
#[derive(Debug, Clone, PartialEq)]
pub enum ColumnKind {
    Underlying,
    Level {
        measure: Measure,
        heat: bool,
    },
    /// A delta chip over the lookback in force.
    Change {
        measure: Measure,
    },
    /// The 1M forward's change as a percent of `from`.
    ForwardChange {
        measure: Measure,
    },
    /// Slider and rank.
    Pctl {
        measure: Measure,
    },
    /// Zero bar.
    Bar {
        measure: Measure,
    },
    /// The tail `span` of `measure`'s sparkline, found in the ask by
    /// measure (the board's span may be longer).
    Spark {
        measure: Measure,
        span: Duration,
    },
    /// ATFM across every tenor, on a shared range.
    Term,
    Vega,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Column {
    pub id: String,
    pub group: Option<String>,
    pub head: String,
    pub kind: ColumnKind,
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

fn col(id: String, group: Option<&str>, head: &str, kind: ColumnKind) -> Column {
    Column {
        id,
        group: group.map(str::to_string),
        head: head.to_string(),
        kind,
    }
}

fn level(id: String, group: Option<&str>, head: &str, measure: Measure, heat: bool) -> Column {
    col(id, group, head, ColumnKind::Level { measure, heat })
}

fn change(id: String, group: Option<&str>, head: &str, measure: Measure) -> Column {
    col(id, group, head, ColumnKind::Change { measure })
}

fn pctl(id: String, group: Option<&str>, head: &str, measure: Measure) -> Column {
    col(id, group, head, ColumnKind::Pctl { measure })
}

fn skew(t: &str) -> Measure {
    Measure::Skew { tenor: t.into() }
}

fn slope(long: &str, short: &str) -> Measure {
    Measure::Slope {
        long: long.into(),
        short: short.into(),
    }
}

fn realized(w: &str) -> Measure {
    Measure::Realized { window: w.into() }
}

fn varswap(t: &str) -> Measure {
    Measure::Varswap { tenor: t.into() }
}

/// A level and its change under one group.
fn level_and_change(
    stem: String,
    group: String,
    (level_head, change_head): (&str, &str),
    measure: Measure,
    heat: bool,
) -> [Column; 2] {
    [
        level(
            format!("{stem}-level"),
            Some(&group),
            level_head,
            measure.clone(),
            heat,
        ),
        change(format!("{stem}-change"), Some(&group), change_head, measure),
    ]
}

/// The columns of a view, `Underlying` first. Changes and Levels name
/// tenors by convention (`ATFM_TENORS`), so a tenor the surface lacks keeps
/// a blank column; Term and Skew follow `tenors`.
pub fn columns(view: View, tenors: &[String]) -> Vec<Column> {
    let mut cols = vec![col(
        "underlying".into(),
        None,
        "Underlying",
        ColumnKind::Underlying,
    )];
    match view {
        View::Changes => {
            cols.push(col(
                "fwd-1M-change".into(),
                Some("fwd"),
                "\u{394}%",
                ColumnKind::ForwardChange {
                    measure: Measure::Forward { tenor: "1M".into() },
                },
            ));
            for t in ATFM_TENORS {
                cols.extend(level_and_change(
                    format!("atfm-{t}"),
                    format!("ATFM {t}"),
                    ("vol", "\u{394}"),
                    Measure::atfm(t),
                    false,
                ));
            }
            cols.extend(level_and_change(
                "skew-3M".into(),
                "25\u{394} skew 3M".into(),
                ("pts", "\u{394}"),
                skew("3M"),
                false,
            ));
            cols.extend(level_and_change(
                "slope-1Y-1M".into(),
                "1Y\u{2212}1M".into(),
                ("pts", "\u{394}"),
                slope("1Y", "1M"),
                false,
            ));
            cols.push(col(
                "spark-0".into(),
                Some("ATFM 3M \u{b7} 30d"),
                "ATFM 3M \u{b7} 30d",
                ColumnKind::Spark {
                    measure: Measure::atfm("3M"),
                    span: SPARK_30D,
                },
            ));
            cols.push(col("vega".into(), None, "vega k", ColumnKind::Vega));
        }
        View::Levels => {
            for t in ATFM_TENORS {
                cols.push(level(
                    format!("atfm-{t}-level"),
                    None,
                    &format!("ATFM {t}"),
                    Measure::atfm(t),
                    true,
                ));
            }
            cols.push(level(
                "fwd-3M-level".into(),
                None,
                "fwd 3M",
                Measure::Forward { tenor: "3M".into() },
                false,
            ));
            cols.push(level(
                "skew-3M-level".into(),
                None,
                "skew 3M",
                skew("3M"),
                false,
            ));
            cols.push(level(
                "slope-1Y-1M-level".into(),
                None,
                "1Y\u{2212}1M",
                slope("1Y", "1M"),
                false,
            ));
            for w in WINDOWS {
                cols.push(level(
                    format!("rlz-{w}-level"),
                    None,
                    &format!("rlz {w}"),
                    realized(w),
                    false,
                ));
            }
            cols.push(level(
                "vs-3M-level".into(),
                None,
                "vs 3M",
                varswap("3M"),
                false,
            ));
            cols.push(col("vega".into(), None, "vega k", ColumnKind::Vega));
        }
        View::RichCheap => {
            cols.push(level(
                "atfm-3M-level".into(),
                Some("ATFM 3M"),
                "vol",
                Measure::atfm("3M"),
                false,
            ));
            cols.push(pctl(
                "atfm-3M-pctl".into(),
                Some("ATFM 3M"),
                "1Y pctl",
                Measure::atfm("3M"),
            ));
            cols.push(level(
                "rlz-5D-level".into(),
                Some("Realized"),
                "5d",
                realized("5D"),
                false,
            ));
            cols.push(level(
                "rlz-20D-level".into(),
                Some("Realized"),
                "20d",
                realized("20D"),
                false,
            ));
            let ivrv = Measure::IvRv {
                tenor: "3M".into(),
                window: "20D".into(),
            };
            cols.push(col(
                "ivrv-3M-20D-bar".into(),
                None,
                "IV 3M \u{2212} RV 20d",
                ColumnKind::Bar { measure: ivrv },
            ));
            cols.push(level(
                "vs-3M-level".into(),
                Some("Varswap 3M"),
                "strike",
                varswap("3M"),
                false,
            ));
            cols.push(col(
                "vsspread-3M-bar".into(),
                Some("Varswap 3M"),
                "vs ATM",
                ColumnKind::Bar {
                    measure: Measure::VsSpread { tenor: "3M".into() },
                },
            ));
            cols.push(level(
                "skew-3M-level".into(),
                Some("25\u{394} skew 3M"),
                "pts",
                skew("3M"),
                false,
            ));
            cols.push(pctl(
                "skew-3M-pctl".into(),
                Some("25\u{394} skew 3M"),
                "1Y pctl",
                skew("3M"),
            ));
            cols.push(level(
                "slope-1Y-1M-level".into(),
                Some("Term 1Y\u{2212}1M"),
                "pts",
                slope("1Y", "1M"),
                false,
            ));
            cols.push(pctl(
                "slope-1Y-1M-pctl".into(),
                Some("Term 1Y\u{2212}1M"),
                "1Y pctl",
                slope("1Y", "1M"),
            ));
        }
        View::Term => {
            cols.push(col("term".into(), None, "shape", ColumnKind::Term));
            for t in tenors {
                cols.extend(level_and_change(
                    format!("atfm-{t}"),
                    t.clone(),
                    ("vol", "\u{394}"),
                    Measure::atfm(t),
                    true,
                ));
            }
            cols.extend(level_and_change(
                "slope-1Y-1M".into(),
                "1Y\u{2212}1M".into(),
                ("pts", "\u{394}"),
                slope("1Y", "1M"),
                false,
            ));
            cols.extend(level_and_change(
                "slope-3Y-1Y".into(),
                "3Y\u{2212}1Y".into(),
                ("pts", "\u{394}"),
                slope("3Y", "1Y"),
                false,
            ));
        }
        View::Skew => {
            for t in tenors {
                cols.extend(level_and_change(
                    format!("skew-{t}"),
                    format!("25\u{394} {t}"),
                    ("pts", "\u{394}"),
                    skew(t),
                    false,
                ));
            }
            cols.push(level(
                "wing-3M-level".into(),
                None,
                "10\u{394} 3M",
                Measure::WingSkew { tenor: "3M".into() },
                false,
            ));
            cols.push(pctl(
                "skew-3M-pctl".into(),
                Some("skew 3M"),
                "1Y pctl",
                skew("3M"),
            ));
            cols.push(level(
                "put-3M-level".into(),
                None,
                "25\u{394} put 3M",
                Measure::Point {
                    tenor: "3M".into(),
                    delta: 0.75,
                },
                false,
            ));
            cols.push(level(
                "call-3M-level".into(),
                None,
                "25\u{394} call 3M",
                Measure::Point {
                    tenor: "3M".into(),
                    delta: 0.25,
                },
                false,
            ));
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
    out.push(varswap("3M"));
    out.push(Measure::VsSpread { tenor: "3M".into() });
    out.push(Measure::atfm("3M"));
    out
}

fn push_unique(into: &mut Vec<Measure>, m: Measure) {
    if !into.contains(&m) {
        into.push(m);
    }
}

/// Add a sparkline; a measure already present keeps the longer span (the
/// Changes column decimates its 30-day spark from the one-year series).
fn merge_spark(into: &mut Vec<(Measure, Duration)>, m: Measure, span: Duration) {
    match into.iter_mut().find(|(have, _)| *have == m) {
        Some((_, have)) => *have = (*have).max(span),
        None => into.push((m, span)),
    }
}

/// The union of what the view's columns read, plus the band's measures
/// while open. `Vega` asks nothing of the board.
pub fn ask(view: View, tenors: &[String], lookback: Lookback, band: bool) -> Ask {
    let mut measures = Vec::new();
    let mut window = None;
    let mut sparklines = Vec::new();
    for c in columns(view, tenors) {
        match c.kind {
            ColumnKind::Level { measure, .. }
            | ColumnKind::Change { measure }
            | ColumnKind::ForwardChange { measure }
            | ColumnKind::Bar { measure } => push_unique(&mut measures, measure),
            ColumnKind::Pctl { measure } => {
                push_unique(&mut measures, measure);
                window = Some(WINDOW_1Y);
            }
            ColumnKind::Spark { measure, span } => merge_spark(&mut sparklines, measure, span),
            ColumnKind::Term => {
                for t in tenors {
                    push_unique(&mut measures, Measure::atfm(t));
                }
            }
            ColumnKind::Underlying | ColumnKind::Vega => {}
        }
    }
    if band {
        for m in band_measures(tenors) {
            push_unique(&mut measures, m);
        }
        window = Some(WINDOW_1Y);
        for m in [Measure::atfm("3M"), skew("3M"), realized("20D")] {
            merge_spark(&mut sparklines, m, WINDOW_1Y);
        }
    }
    Ask {
        measures,
        lookback,
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

    #[test]
    fn digits_and_tab_walk_the_five_views_in_order() {
        assert_eq!(View::from_digit(1), Some(View::Changes));
        assert_eq!(View::from_digit(5), Some(View::Skew));
        assert_eq!(View::from_digit(6), None);
        assert_eq!(View::Skew.next(), View::Changes);
        assert_eq!(View::parse("richcheap"), Some(View::RichCheap));
    }

    #[test]
    fn a_missing_conventional_tenor_keeps_its_blank_column() {
        let tenors = order_tenors(Some(&[
            "1M".into(),
            "3M".into(),
            "6M".into(),
            "1Y".into(),
            "9M".into(),
        ]));
        let cols = columns(View::Changes, &tenors);
        assert!(
            cols.iter().any(|c| c.id == "atfm-2Y-level"),
            "2Y stays as a column the board leaves blank"
        );
        assert!(
            cols.iter().all(|c| c.id != "atfm-9M-level"),
            "Changes names tenors by convention"
        );
    }

    #[test]
    fn term_orders_every_listed_tenor() {
        let tenors = order_tenors(Some(&["1Y".into(), "9M".into(), "1M".into(), "odd".into()]));
        assert_eq!(tenors, ["1M", "9M", "1Y", "odd"]);
        let ids: Vec<_> = columns(View::Term, &tenors)
            .iter()
            .map(|c| c.id.clone())
            .collect();
        assert_eq!(ids[0], "underlying");
        assert_eq!(ids[1], "term");
        assert_eq!(ids[2], "atfm-1M-level");
        assert!(ids.contains(&"atfm-odd-level".to_string()));
    }

    #[test]
    fn the_ask_is_the_union_of_the_columns_measures() {
        let tenors = order_tenors(None);
        let a = ask(View::Changes, &tenors, Lookback::D1, false);
        assert!(
            a.measures
                .contains(&Measure::Forward { tenor: "1M".into() })
        );
        assert!(a.measures.contains(&Measure::atfm("3M")));
        assert_eq!(
            a.measures
                .iter()
                .filter(|m| **m == Measure::atfm("3M"))
                .count(),
            1,
            "deduped"
        );
        assert_eq!(a.window, None, "Changes asks no percentile");
        assert_eq!(a.sparklines, vec![(Measure::atfm("3M"), SPARK_30D)]);
        assert!(columns(View::Changes, &tenors).iter().any(|c| c.kind
            == ColumnKind::Spark {
                measure: Measure::atfm("3M"),
                span: SPARK_30D
            }));
        let r = ask(View::RichCheap, &tenors, Lookback::D1, false);
        assert_eq!(r.window, Some(WINDOW_1Y));
        assert!(r.measures.contains(&Measure::IvRv {
            tenor: "3M".into(),
            window: "20D".into()
        }));
    }

    #[test]
    fn the_band_joins_the_ask_only_while_open() {
        let tenors = order_tenors(None);
        let closed = ask(View::Levels, &tenors, Lookback::D1, false);
        let open = ask(View::Levels, &tenors, Lookback::D1, true);
        let wing = Measure::Point {
            tenor: "1M".into(),
            delta: 0.10,
        };
        assert!(!closed.measures.contains(&wing));
        assert!(open.measures.contains(&wing));
        assert_eq!(open.window, Some(WINDOW_1Y));
        assert_eq!(open.sparklines.len(), 3);
        assert!(needs_requery(&closed, &open));
        assert!(!needs_requery(&open, &open.clone()));
        let mut fewer = open.clone();
        fewer.measures.pop();
        assert!(
            !needs_requery(&open, &fewer),
            "a narrower ask does not requery"
        );
    }

    #[test]
    fn closing_the_band_does_not_requery() {
        let tenors = order_tenors(None);
        let closed = ask(View::Levels, &tenors, Lookback::D1, false);
        let open = ask(View::Levels, &tenors, Lookback::D1, true);
        assert!(!needs_requery(&open, &closed));
    }

    #[test]
    fn a_shorter_sparkline_of_the_same_measure_does_not_requery() {
        let long = Ask {
            sparklines: vec![(Measure::atfm("3M"), WINDOW_1Y)],
            ..Ask::default()
        };
        let short = Ask {
            sparklines: vec![(Measure::atfm("3M"), SPARK_30D)],
            ..Ask::default()
        };
        assert!(!needs_requery(&long, &short));
        assert!(needs_requery(&short, &long));
    }

    #[test]
    fn the_longer_span_wins_when_view_and_band_name_a_measure() {
        let tenors = order_tenors(None);
        let a = ask(View::Changes, &tenors, Lookback::D1, true);
        assert_eq!(a.sparklines.len(), 3);
        assert!(a.sparklines.contains(&(Measure::atfm("3M"), WINDOW_1Y)));
        assert!(!a.sparklines.contains(&(Measure::atfm("3M"), SPARK_30D)));
    }

    #[test]
    fn a_view_switch_requeries_only_when_the_ask_changes() {
        let tenors = order_tenors(None);
        let changes = ask(View::Changes, &tenors, Lookback::D1, false);
        assert!(
            !needs_requery(&changes, &changes.clone()),
            "the same ask asks nothing"
        );
        let levels = ask(View::Levels, &tenors, Lookback::D1, false);
        assert!(
            needs_requery(&changes, &levels),
            "Levels reads realized and varswap"
        );
        let w1 = ask(View::Changes, &tenors, Lookback::W1, false);
        assert!(
            needs_requery(&changes, &w1),
            "a lookback is part of the ask (L5)"
        );
    }

    /// The column tables carry the desk's conventions: a 25Δ put is the
    /// 0.75 call delta and a 25Δ call the 0.25, and IV − RV pairs the 3M
    /// surface with the 20-day realized window.
    #[test]
    fn the_skew_points_and_the_ivrv_window_are_pinned() {
        let tenors = order_tenors(None);
        let kind_of = |view: View, id: &str| {
            columns(view, &tenors)
                .into_iter()
                .find(|c| c.id == id)
                .map(|c| c.kind)
                .unwrap()
        };
        let point = |delta: f64| ColumnKind::Level {
            measure: Measure::Point {
                tenor: "3M".into(),
                delta,
            },
            heat: false,
        };
        assert_eq!(kind_of(View::Skew, "put-3M-level"), point(0.75));
        assert_eq!(kind_of(View::Skew, "call-3M-level"), point(0.25));
        assert_eq!(
            kind_of(View::RichCheap, "ivrv-3M-20D-bar"),
            ColumnKind::Bar {
                measure: Measure::IvRv {
                    tenor: "3M".into(),
                    window: "20D".into(),
                },
            }
        );
    }
}
