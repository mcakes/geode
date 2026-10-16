//! The prepared model: a landed [`VolBoard`] turned into everything render
//! reads — each cell's text, glyph core, id and muted flag, the column
//! widths, the cursor readout and the header model — built once per board,
//! view or paint change, never per frame.
//!
//! The only arithmetic here is display: a forward change as a percent of
//! its `from`, a rank times a hundred, and the scales and ranges glyphs are
//! measured against. Every number comes from a board cell.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use chrono::{DateTime, Utc};
use geode_chart::core::spark::shared_range;
use geode_chart::spark::sizes;
use geode_core::clock::Clock;
use geode_core::format::{Formatted, Sign, format_number};
use geode_core::view::{Colour, ColumnFormat, Negative, Scale};
use geode_core::volboard::{Cell, Change, Lookback, Measure, VolBoard, VolRow};
use geode_tile::rows::GridRow;
use geode_widgets::cell::delta_chip::{Chip, chip};
use geode_widgets::cell::heat::{heat, range};
use geode_widgets::cell::percent_slider::{Slider, slider};
use geode_widgets::cell::zero_bar::{Bar, bar};
use geode_widgets::cell::{column_scale, widths};
use gpui::SharedString;

use super::views::{Ask, Column, ColumnKind, PCTL_MIN_COUNT, TITLE};

/// Fill alpha of a change chip at the column's largest move.
pub const CHIP_CEILING: f32 = 0.55;
/// Heat tint alpha at the top of the grid's range.
pub const HEAT_CEILING: f32 = 0.40;
/// Zero bar fill alpha over its sign color.
pub const BAR_ALPHA: f32 = 0.75;

/// Levels, changes and bars: one decimal.
pub const ONE_DP: ColumnFormat = ColumnFormat {
    precision: 1,
    thousands: false,
    negative: Negative::Minus,
    colour: Colour::Sign,
    scale: Scale::None,
};

/// Book vega in thousands, no decimals.
const VEGA: ColumnFormat = ColumnFormat {
    precision: 0,
    thousands: true,
    negative: Negative::Minus,
    colour: Colour::Sign,
    scale: Scale::Thousands,
};

/// One member's row as the board answered it, joined with the list and reference.
#[derive(Debug, Clone, PartialEq)]
pub struct Row {
    pub name: String,
    pub reference: Option<String>,
    /// In the board and not among its `missing`.
    pub present: bool,
    pub as_of: Option<DateTime<Utc>>,
    pub board: Option<VolRow>,
    pub vega: Option<f64>,
}

impl GridRow for Row {
    fn name(&self) -> &str {
        &self.name
    }

    fn narrow_fields(&self) -> Vec<&str> {
        vec![&self.name, self.reference.as_deref().unwrap_or("")]
    }
}

/// Rows in member order, present members first (board order), missing ones at the foot, muted.
/// A board row for a name no longer listed is dropped; a listed member the
/// board has no row for is missing.
pub fn rows(
    members: &[String],
    board: Option<&VolBoard>,
    reference: &dyn Fn(&str) -> Option<String>,
    vega: Option<&HashMap<String, f64>>,
) -> Vec<Row> {
    let make = |name: &str, board_row: Option<&VolRow>, present: bool| Row {
        name: name.to_string(),
        reference: reference(name),
        present,
        as_of: board_row.and_then(|r| r.as_of),
        board: board_row.cloned(),
        vega: vega.and_then(|v| v.get(name)).copied(),
    };
    let missing = |name: &str| board.is_some_and(|b| b.missing.iter().any(|m| m == name));
    let mut out: Vec<Row> = board
        .map(|b| b.rows.as_slice())
        .unwrap_or_default()
        .iter()
        .filter(|r| members.contains(&r.underlying) && !missing(&r.underlying))
        .map(|r| make(&r.underlying, Some(r), true))
        .collect();
    for name in members {
        if !out.iter().any(|r| r.name == *name) {
            let board_row = board.and_then(|b| b.rows.iter().find(|r| r.underlying == *name));
            out.push(make(name, board_row, false));
        }
    }
    out
}

#[derive(Debug, Clone, PartialEq)]
pub enum Glyph {
    None,
    Chip(Option<Chip>),
    Slider(Option<Slider>),
    Bar(Option<Bar>),
    Heat(Option<f32>),
    Spark {
        values: Arc<[f64]>,
        range: Option<(f64, f64)>,
        mark: Option<usize>,
        end_dot: bool,
    },
}

#[derive(Debug, Clone, PartialEq)]
pub struct PreparedCell {
    pub id: SharedString,
    pub text: SharedString,
    pub value: Option<f64>,
    /// The sign of the displayed (rounded) text.
    pub sign: Sign,
    pub glyph: Glyph,
    pub muted: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct PreparedRow {
    pub name: SharedString,
    pub present: bool,
    pub stale: bool,
    pub cells: Vec<PreparedCell>,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct Prepared {
    pub rows: Vec<PreparedRow>,
    /// design px per column, from the glyph widths and the widest text
    pub widths: Vec<f32>,
}

pub struct Inputs<'a> {
    pub tile: u64,
    pub rows: &'a [Row],
    pub columns: &'a [Column],
    /// The ask the board ANSWERED, not the view's current one: cells and
    /// sparklines are found by their position in it, so reading a board
    /// through any other ask misplaces every cell.
    pub asked: &'a Ask,
    /// the lookback the board was asked with; cells paint muted when it differs from `shown`
    pub board_lookback: Lookback,
    pub shown: Lookback,
    pub paint: bool,
    pub stale_after: Duration,
    pub now: DateTime<Utc>,
    pub mono_advance_px: f32,
}

/// The board cell a measure reads: its position in the ask, checked
/// against the cell's own measure. A measure the board was not asked for
/// (a narrower ask kept after a view switch) has none.
fn cell<'r>(row: &'r Row, measure: &Measure, asked: &Ask) -> Option<&'r Cell> {
    let pos = asked.measures.iter().position(|m| m == measure)?;
    row.board
        .as_ref()?
        .cells
        .get(pos)
        .filter(|c| c.measure == *measure)
}

/// A measure's sparkline and the span the board answered it over, found by
/// measure in the ask as cells are; `None` for a row the surface lacks.
fn spark<'r>(
    row: &'r Row,
    measure: &Measure,
    asked: &Ask,
) -> Option<(&'r Option<Vec<f64>>, Duration)> {
    let pos = asked.sparklines.iter().position(|(m, _)| m == measure)?;
    let series = row
        .present
        .then_some(row.board.as_ref())??
        .sparks
        .get(pos)?;
    Some((series, asked.sparklines[pos].1))
}

/// The change over the one lookback the tile asks.
fn change_of<'r>(row: &'r Row, measure: &Measure, asked: &Ask) -> Option<&'r Change> {
    cell(row, measure, asked)?.changes.first()?.as_ref()
}

fn finite(v: f64) -> Option<f64> {
    v.is_finite().then_some(v)
}

/// The number a cell shows, before formatting; `None` for a blank and for
/// any row the surface lacks.
fn value(row: &Row, kind: &ColumnKind, asked: &Ask) -> Option<f64> {
    if !row.present {
        return None;
    }
    match kind {
        ColumnKind::Level { measure, .. } | ColumnKind::Bar { measure } => {
            cell(row, measure, asked)?.current.and_then(finite)
        }
        ColumnKind::Change { measure } => finite(change_of(row, measure, asked)?.value),
        ColumnKind::ForwardChange { measure } => {
            let c = change_of(row, measure, asked)?;
            finite(c.value / c.from * 100.0)
        }
        ColumnKind::Pctl { measure } => {
            cell(row, measure, asked)?.pctl.and_then(|(r, _)| finite(r))
        }
        ColumnKind::Vega => row.vega.and_then(finite),
        ColumnKind::Underlying | ColumnKind::Spark { .. } | ColumnKind::Term => None,
    }
}

/// The text with an explicit `+` on a positive displayed sign.
fn signed(v: f64, format: &ColumnFormat) -> Formatted {
    let mut f = format_number(v, format);
    if f.sign == Sign::Positive {
        f.text.insert(0, '+');
    }
    f
}

/// A rank as its displayed percent: `0.8951` → `90`.
fn rank_percent(rank: f64) -> f64 {
    (rank * 100.0).round()
}

/// The displayed text of a value in a column of `kind`.
fn text(kind: &ColumnKind, v: f64) -> Formatted {
    match kind {
        ColumnKind::Level { .. } => format_number(v, &ONE_DP),
        ColumnKind::Change { .. } | ColumnKind::ForwardChange { .. } | ColumnKind::Bar { .. } => {
            signed(v, &ONE_DP)
        }
        ColumnKind::Pctl { .. } => Formatted {
            text: format!("{:.0}", rank_percent(v)),
            sign: Sign::Zero,
        },
        ColumnKind::Vega => format_number(v, &VEGA),
        ColumnKind::Underlying | ColumnKind::Spark { .. } | ColumnKind::Term => Formatted {
            text: String::new(),
            sign: Sign::Zero,
        },
    }
}

/// The tail of a board sparkline a column of `wanted` span shows: the
/// board answers the longest span any column or the band asked.
fn spark_tail(values: &[f64], board_span: Duration, wanted: Duration) -> &[f64] {
    let (have, want) = (board_span.as_secs(), wanted.as_secs());
    if have <= want || have == 0 {
        return values;
    }
    let n = values.len();
    let keep = (n as u128 * u128::from(want)).div_ceil(u128::from(have)) as usize;
    &values[n - keep.min(n)..]
}

/// The ATFM measures a Term glyph draws, in column order (the view lists
/// a level per tenor).
fn term_measures(columns: &[Column]) -> Vec<&Measure> {
    columns
        .iter()
        .filter_map(|c| match &c.kind {
            ColumnKind::Level {
                measure: m @ Measure::Point { delta, .. },
                ..
            } if *delta == 0.5 => Some(m),
            _ => None,
        })
        .collect()
}

fn term_values(row: &Row, measures: &[&Measure], asked: &Ask) -> Option<Vec<f64>> {
    if !row.present {
        return None;
    }
    let values: Vec<f64> = measures
        .iter()
        .map(|m| {
            cell(row, m, asked)
                .and_then(|c| c.current)
                .filter(|v| v.is_finite())
                .unwrap_or(f64::NAN)
        })
        .collect();
    values.iter().any(|v| v.is_finite()).then_some(values)
}

fn is_stale(row: &Row, now: DateTime<Utc>, stale_after: Duration) -> bool {
    row.present
        && row
            .as_of
            .is_some_and(|t| (now - t).to_std().is_ok_and(|age| age > stale_after))
}

/// Text px → the narrowest cell a column of `kind` paints it in.
fn glyph_width(kind: &ColumnKind, text_px: f32) -> f32 {
    match kind {
        ColumnKind::Change { .. } | ColumnKind::ForwardChange { .. } => widths::chip(text_px),
        ColumnKind::Pctl { .. } => widths::slider(text_px),
        ColumnKind::Bar { .. } => widths::bar(text_px),
        ColumnKind::Level { heat: true, .. } => widths::heat(text_px),
        ColumnKind::Spark { .. } => text_px.max(sizes::WIDTH_30D),
        ColumnKind::Term => text_px.max(sizes::WIDTH_TERM),
        ColumnKind::Level { heat: false, .. } | ColumnKind::Underlying | ColumnKind::Vega => {
            text_px
        }
    }
}

/// Every row's cells, prepared once.
pub fn prepare(i: &Inputs<'_>) -> Prepared {
    // One pass for the numbers, so scales and ranges read them once.
    let values: Vec<Vec<Option<f64>>> = i
        .rows
        .iter()
        .map(|r| {
            i.columns
                .iter()
                .map(|c| value(r, &c.kind, i.asked))
                .collect()
        })
        .collect();
    let scales: Vec<f64> = (0..i.columns.len())
        .map(|ci| column_scale(values.iter().filter_map(|row| row[ci])))
        .collect();
    let heat_range = range(i.columns.iter().enumerate().flat_map(|(ci, c)| {
        let tinted = matches!(c.kind, ColumnKind::Level { heat: true, .. });
        values
            .iter()
            .filter_map(move |row| if tinted { row[ci] } else { None })
    }));
    let has_term = i.columns.iter().any(|c| c.kind == ColumnKind::Term);
    let tenors = if has_term {
        term_measures(i.columns)
    } else {
        Vec::new()
    };
    let terms: Vec<Option<Arc<[f64]>>> = i
        .rows
        .iter()
        .map(|r| {
            if has_term {
                term_values(r, &tenors, i.asked).map(Arc::from)
            } else {
                None
            }
        })
        .collect();
    let term_range = shared_range(terms.iter().flatten().map(|v| &v[..]));
    let term_mark = tenors.iter().position(|m| **m == Measure::atfm("3M"));

    let mut col_px: Vec<f32> = i
        .columns
        .iter()
        .map(|c| c.head.chars().count() as f32 * i.mono_advance_px)
        .collect();

    let rows = i
        .rows
        .iter()
        .enumerate()
        .map(|(ri, row)| {
            let stale = is_stale(row, i.now, i.stale_after);
            let cells = i
                .columns
                .iter()
                .enumerate()
                .map(|(ci, column)| {
                    let v = values[ri][ci];
                    let mut cell = prepare_cell(i, row, column, v, scales[ci], heat_range);
                    match &column.kind {
                        ColumnKind::Underlying => {
                            cell.text = SharedString::from(row.name.clone());
                            cell.muted = !row.present;
                        }
                        ColumnKind::Term => {
                            if let Some(values) = &terms[ri] {
                                cell.text = SharedString::default();
                                cell.glyph = Glyph::Spark {
                                    values: Arc::clone(values),
                                    range: term_range,
                                    mark: term_mark,
                                    end_dot: false,
                                };
                                cell.muted = !row.present;
                            }
                        }
                        _ => {}
                    }
                    cell.muted |= stale;
                    let px = cell.text.chars().count() as f32 * i.mono_advance_px;
                    col_px[ci] = col_px[ci].max(glyph_width(&column.kind, px));
                    cell.id = format!("{}-{}-{}", i.tile, row.name, column.id).into();
                    cell
                })
                .collect();
            PreparedRow {
                name: row.name.clone().into(),
                present: row.present,
                stale,
                cells,
            }
        })
        .collect();
    for (ci, c) in i.columns.iter().enumerate() {
        col_px[ci] = col_px[ci].max(glyph_width(&c.kind, 0.0));
    }
    Prepared {
        rows,
        widths: col_px,
    }
}

/// One cell's text, sign, glyph and muted flag; the id, the Underlying
/// name and the Term glyph are the caller's.
fn prepare_cell(
    i: &Inputs<'_>,
    row: &Row,
    column: &Column,
    v: Option<f64>,
    scale: f64,
    heat_range: Option<(f64, f64)>,
) -> PreparedCell {
    let blank = PreparedCell {
        id: SharedString::default(),
        text: SharedString::new_static(widths::DASH),
        value: None,
        sign: Sign::Zero,
        glyph: Glyph::None,
        muted: true,
    };
    if let ColumnKind::Spark {
        measure,
        span: wanted,
    } = &column.kind
    {
        let Some((Some(series), board_span)) = spark(row, measure, i.asked) else {
            return blank;
        };
        if series.is_empty() {
            return blank;
        }
        return PreparedCell {
            text: SharedString::default(),
            muted: false,
            glyph: Glyph::Spark {
                values: Arc::from(spark_tail(series, board_span, *wanted)),
                range: None,
                mark: None,
                end_dot: true,
            },
            ..blank
        };
    }
    let Some(v) = v else {
        return blank;
    };
    let Formatted { text: t, sign } = text(&column.kind, v);
    let glyph = if !i.paint {
        Glyph::None
    } else {
        match &column.kind {
            ColumnKind::Change { .. } | ColumnKind::ForwardChange { .. } => {
                Glyph::Chip(chip(v, sign, scale, CHIP_CEILING))
            }
            ColumnKind::Bar { .. } => Glyph::Bar(bar(v, sign, scale)),
            ColumnKind::Pctl { .. } => Glyph::Slider(slider(rank_percent(v) / 100.0)),
            ColumnKind::Level { heat: true, .. } => {
                Glyph::Heat(heat_range.and_then(|(lo, hi)| heat(v, lo, hi, HEAT_CEILING)))
            }
            _ => Glyph::None,
        }
    };
    let muted = match &column.kind {
        ColumnKind::Change { .. } | ColumnKind::ForwardChange { .. } => i.board_lookback != i.shown,
        ColumnKind::Pctl { measure } => cell(row, measure, i.asked)
            .and_then(|c| c.pctl)
            .is_some_and(|(_, count)| count < PCTL_MIN_COUNT),
        _ => false,
    };
    PreparedCell {
        text: t.into(),
        value: Some(v),
        sign,
        glyph,
        muted,
        ..blank
    }
}

/// The measure a column reads, for the readout; a sparkline column reads
/// its sparkline's measure.
fn column_measure(column: &Column) -> Option<&Measure> {
    match &column.kind {
        ColumnKind::Level { measure, .. }
        | ColumnKind::Change { measure }
        | ColumnKind::ForwardChange { measure }
        | ColumnKind::Pctl { measure }
        | ColumnKind::Bar { measure } => Some(measure),
        ColumnKind::Spark { measure, .. } => Some(measure),
        ColumnKind::Underlying | ColumnKind::Term | ColumnKind::Vega => None,
    }
}

/// A measure as the desk names it in the readout.
fn measure_label(m: &Measure) -> String {
    match m {
        Measure::Point { tenor, delta } if *delta == 0.5 => format!("ATFM {tenor}"),
        Measure::Point { tenor, delta } => format!(
            "{tenor} {}\u{394}",
            geode_core::volboard::delta_text(*delta)
        ),
        Measure::Forward { tenor } => format!("fwd {tenor}"),
        Measure::Skew { tenor } => format!("25\u{394} skew {tenor}"),
        Measure::WingSkew { tenor } => format!("10\u{394} skew {tenor}"),
        Measure::Slope { long, short } => format!("{long}\u{2212}{short}"),
        Measure::Realized { window } => format!("rlz {window}"),
        Measure::IvRv { tenor, window } => format!("IV {tenor} \u{2212} RV {window}"),
        Measure::Varswap { tenor } => format!("vs {tenor}"),
        Measure::VsSpread { tenor } => format!("vs spread {tenor}"),
    }
}

/// The cell's value for sorting: the number the column shows (rank as 0..1 for Pctl); None sorts last.
pub fn sort_value(row: &Row, column: &Column, asked: &Ask) -> Option<f64> {
    value(row, &column.kind, asked)
}

/// `SX5E · ATFM 3M 15.9 · Δ1d +1.1 from 14.8 on 2026-10-09 · 1Y pctl 91 of 251`, or `SX5E · not in surface`.
pub fn readout(row: &Row, column: &Column, asked: &Ask, clock: &Clock) -> String {
    if !row.present {
        return format!("{} \u{b7} not in surface", row.name);
    }
    let mut parts = vec![row.name.clone()];
    match column_measure(column) {
        Some(m) => {
            let c = cell(row, m, asked);
            let current = c.and_then(|c| c.current).map_or_else(
                || widths::DASH.to_string(),
                |v| format_number(v, &ONE_DP).text,
            );
            parts.push(format!("{} {current}", measure_label(m)));
            if let Some(ch) = change_of(row, m, asked) {
                parts.push(format!(
                    "\u{394}{} {} from {} on {}",
                    asked.lookback.label(),
                    signed(ch.value, &ONE_DP).text,
                    format_number(ch.from, &ONE_DP).text,
                    clock.local(ch.at).format("%Y-%m-%d"),
                ));
            }
            if let Some((rank, count)) = c.and_then(|c| c.pctl) {
                parts.push(format!("1Y pctl {:.0} of {count}", rank_percent(rank)));
            }
        }
        None => match &column.kind {
            ColumnKind::Vega => parts.push(format!(
                "{} {}",
                column.head,
                row.vega.map_or_else(
                    || widths::DASH.to_string(),
                    |v| format_number(v, &VEGA).text
                )
            )),
            ColumnKind::Underlying => parts.extend(row.reference.clone()),
            _ => {}
        },
    }
    parts.join(" \u{b7} ")
}

#[derive(Debug, Clone, PartialEq)]
pub struct HeaderModel {
    pub title: String,
    pub count: usize,
    /// `Lookback::ALL` in order, the one in force true.
    pub lookbacks: [(Lookback, bool); 3],
    pub sort: Option<String>,
    pub not_in_surface: usize,
    pub failed: Vec<String>,
    pub notes: Vec<String>,
}

impl Default for HeaderModel {
    fn default() -> Self {
        header(None, 0, Lookback::D1, None, None)
    }
}

/// The header's text and counts. With no list the title reads `Vol · New…`.
pub fn header(
    list: Option<&str>,
    count: usize,
    shown: Lookback,
    sort: Option<&str>,
    board: Option<&VolBoard>,
) -> HeaderModel {
    HeaderModel {
        title: format!("{TITLE} \u{b7} {}", list.unwrap_or("New\u{2026}")),
        count,
        lookbacks: Lookback::ALL.map(|l| (l, l == shown)),
        sort: sort.map(str::to_string),
        not_in_surface: board.map_or(0, |b| b.missing.len()),
        failed: board.map_or_else(Vec::new, |b| {
            b.dataset_errors.iter().map(|(d, _)| d.clone()).collect()
        }),
        notes: board.map_or_else(Vec::new, |b| b.notes.clone()),
    }
}

#[cfg(test)]
mod tests {
    use std::cell::Cell as StdCell;

    use chrono::TimeZone as _;
    use geode_core::volboard::{Cell, Change};

    use super::*;
    use crate::core::views::{ALL_TENORS, View, WINDOW_1Y, ask, columns, order_tenors};

    thread_local! {
        /// The view the last `inputs` call laid out, so `column_index`
        /// reads that view's columns.
        static VIEW: StdCell<View> = const { StdCell::new(View::Changes) };
    }

    fn now() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 10, 12, 16, 0, 0).unwrap()
    }

    fn tenors() -> Vec<String> {
        order_tenors(None)
    }

    fn ask_of(view: View) -> Ask {
        ask(view, &tenors(), Lookback::D1, false)
    }

    fn members<const N: usize>(names: [&str; N]) -> Vec<String> {
        names.iter().map(|n| n.to_string()).collect()
    }

    fn no_reference(_: &str) -> Option<String> {
        None
    }

    fn clock_utc() -> Clock {
        Clock::utc()
    }

    /// The first column with `id` across the views, in `ALL` order.
    fn col(id: &str) -> Column {
        View::ALL
            .iter()
            .flat_map(|v| columns(*v, &tenors()))
            .find(|c| c.id == id)
            .expect("a column with that id")
    }

    fn column_index(id: &str) -> usize {
        let view = VIEW.with(StdCell::get);
        columns(view, &tenors())
            .iter()
            .position(|c| c.id == id)
            .expect("the column in the view inputs laid out")
    }

    fn inputs(rows: &[Row], view: View) -> Inputs<'_> {
        VIEW.with(|v| v.set(view));
        Inputs {
            tile: 7,
            rows,
            columns: Vec::leak(columns(view, &tenors())),
            asked: Box::leak(Box::new(ask_of(view))),
            board_lookback: Lookback::D1,
            shown: Lookback::D1,
            paint: true,
            stale_after: Duration::from_secs(3 * 86_400),
            now: now(),
            mono_advance_px: 7.0,
        }
    }

    /// What a fixture puts in one cell: current, change, pctl.
    type CellData = (Option<f64>, Option<Change>, Option<(f64, usize)>);

    /// A board row parallel to `asked`, cells from `set` (blank where it
    /// gives `None`), sparks of 30 points where `spark`.
    fn row_of(
        asked: &Ask,
        name: &str,
        present: bool,
        set: &dyn Fn(&Measure) -> Option<CellData>,
    ) -> VolRow {
        VolRow {
            underlying: name.into(),
            as_of: present.then(|| now() - chrono::Duration::hours(1)),
            cells: asked
                .measures
                .iter()
                .map(|m| {
                    let (current, change, pctl) = set(m).unwrap_or((None, None, None));
                    Cell {
                        measure: m.clone(),
                        current,
                        current_at: current.map(|_| now() - chrono::Duration::hours(1)),
                        changes: vec![change],
                        pctl,
                    }
                })
                .collect(),
            sparks: asked
                .sparklines
                .iter()
                .map(|_| present.then(|| (0..30).map(|i| 15.0 + f64::from(i) * 0.1).collect()))
                .collect(),
        }
    }

    fn change(value: f64, from: f64) -> Change {
        Change {
            value,
            from,
            at: Utc.with_ymd_and_hms(2026, 10, 9, 16, 0, 0).unwrap(),
        }
    }

    /// SX5E present with ATFM 3M at 15.9, Δ1d +1.1 from 14.8, rank 0.91 of
    /// 251; NEW missing. Answered for the Changes ask.
    fn board_two() -> VolBoard {
        let asked = ask_of(View::Changes);
        let atfm3m = Measure::atfm("3M");
        VolBoard {
            rows: vec![
                row_of(&asked, "SX5E", true, &|m| {
                    (*m == atfm3m).then(|| (Some(15.9), Some(change(1.1, 14.8)), Some((0.91, 251))))
                }),
                row_of(&asked, "NEW", false, &|_| None),
            ],
            missing: vec!["NEW".into()],
            dataset_errors: Vec::new(),
            notes: Vec::new(),
        }
    }

    fn rows_from(board: VolBoard) -> &'static [Row] {
        let names: Vec<String> = board.rows.iter().map(|r| r.underlying.clone()).collect();
        Vec::leak(rows(&names, Some(&board), &no_reference, None))
    }

    /// One member per change, ATFM 3M moved by it (Changes ask).
    fn rows_with_changes(changes: &[f64]) -> &'static [Row] {
        let asked = ask_of(View::Changes);
        let atfm3m = Measure::atfm("3M");
        rows_from(VolBoard {
            rows: changes
                .iter()
                .enumerate()
                .map(|(i, v)| {
                    row_of(&asked, &format!("U{i}"), true, &|m| {
                        (*m == atfm3m).then(|| (Some(15.0), Some(change(*v, 15.0 - v)), None))
                    })
                })
                .collect(),
            ..VolBoard::default()
        })
    }

    /// One member whose ATFM 3M ranks `rank` of `count` (Rich/cheap ask).
    fn rows_with_pctl(rank: f64, count: usize) -> &'static [Row] {
        let asked = ask_of(View::RichCheap);
        let atfm3m = Measure::atfm("3M");
        rows_from(VolBoard {
            rows: vec![row_of(&asked, "SX5E", true, &|m| {
                (*m == atfm3m).then_some((Some(15.0), None, Some((rank, count))))
            })],
            ..VolBoard::default()
        })
    }

    /// One member per pair: ATFM 1M and 3M levels (Term ask).
    fn rows_with_levels(levels: &[[f64; 2]]) -> &'static [Row] {
        let asked = ask_of(View::Term);
        let (m1, m3) = (Measure::atfm("1M"), Measure::atfm("3M"));
        rows_from(VolBoard {
            rows: levels
                .iter()
                .enumerate()
                .map(|(i, [a, b])| {
                    row_of(&asked, &format!("U{i}"), true, &|m| {
                        if *m == m1 {
                            Some((Some(*a), None, None))
                        } else if *m == m3 {
                            Some((Some(*b), None, None))
                        } else {
                            None
                        }
                    })
                })
                .collect(),
            ..VolBoard::default()
        })
    }

    #[test]
    fn a_member_the_surface_lacks_lands_at_the_foot_muted_and_blank() {
        let rows = rows(
            &members(["NEW", "SX5E"]),
            Some(&board_two()),
            &no_reference,
            None,
        );
        assert_eq!(
            rows.iter().map(|r| r.name.as_str()).collect::<Vec<_>>(),
            ["SX5E", "NEW"]
        );
        assert!(!rows[1].present);
        let p = prepare(&inputs(&rows, View::Changes));
        assert!(
            p.rows[1]
                .cells
                .iter()
                .skip(1)
                .all(|c| c.text.as_ref() == widths::DASH && c.muted)
        );
    }

    #[test]
    fn the_chip_takes_the_displayed_sign_and_the_columns_scale() {
        // 0.04 displays as 0.0 → Sign::Zero and a chip with no fill; -2.0 is
        // the column max → alpha = ceiling.
        let p = prepare(&inputs(rows_with_changes(&[0.04, -2.0]), View::Changes));
        let col = column_index("atfm-3M-change");
        let (a, b) = (&p.rows[0].cells[col], &p.rows[1].cells[col]);
        assert_eq!(a.sign, Sign::Zero);
        assert_eq!(a.text.as_ref(), "0.0");
        assert!(
            matches!(a.glyph, Glyph::Chip(Some(Chip { sign: Sign::Zero, alpha })) if alpha == 0.0)
        );
        assert!(
            matches!(b.glyph, Glyph::Chip(Some(Chip { alpha, .. })) if (alpha - CHIP_CEILING).abs() < 1e-6)
        );
        assert_eq!(b.text.as_ref(), "-2.0");
    }

    #[test]
    fn a_positive_change_carries_an_explicit_plus() {
        let p = prepare(&inputs(rows_with_changes(&[1.1]), View::Changes));
        assert_eq!(
            p.rows[0].cells[column_index("atfm-3M-change")]
                .text
                .as_ref(),
            "+1.1"
        );
    }

    #[test]
    fn the_slider_gets_the_rank_at_display_precision() {
        // rank 0.8951 displays as 90 and must warn.
        let p = prepare(&inputs(rows_with_pctl(0.8951, 251), View::RichCheap));
        let c = &p.rows[0].cells[column_index("atfm-3M-pctl")];
        assert_eq!(c.text.as_ref(), "90");
        assert!(matches!(
            c.glyph,
            Glyph::Slider(Some(Slider { warn: true, .. }))
        ));
    }

    #[test]
    fn a_short_window_paints_muted_with_its_count_in_the_readout() {
        let rows = rows_with_pctl(0.5, 12);
        let p = prepare(&inputs(rows, View::RichCheap));
        assert!(p.rows[0].cells[column_index("atfm-3M-pctl")].muted);
        assert!(
            readout(
                &rows[0],
                &col("atfm-3M-pctl"),
                &ask_of(View::RichCheap),
                &clock_utc()
            )
            .ends_with("1Y pctl 50 of 12")
        );
    }

    #[test]
    fn a_change_from_another_lookback_than_the_one_shown_paints_muted() {
        let mut i = inputs(rows_with_changes(&[1.0]), View::Changes);
        assert!(!prepare(&i).rows[0].cells[column_index("atfm-3M-change")].muted);
        i.shown = Lookback::W1; // the board holds D1
        let p = prepare(&i);
        assert!(p.rows[0].cells[column_index("atfm-3M-change")].muted);
        assert!(
            !p.rows[0].cells[column_index("atfm-3M-level")].muted,
            "a level has no lookback"
        );
    }

    #[test]
    fn a_row_older_than_stale_after_paints_stale_and_muted() {
        let mut i = inputs(rows_with_changes(&[1.0]), View::Changes);
        i.stale_after = Duration::from_secs(60);
        let p = prepare(&i);
        assert!(p.rows[0].stale);
        assert!(p.rows[0].cells[column_index("atfm-3M-level")].muted);
    }

    #[test]
    fn paint_off_keeps_the_numbers_and_drops_tints_sliders_and_bars() {
        let mut i = inputs(rows_with_pctl(0.9, 251), View::RichCheap);
        i.paint = false;
        let p = prepare(&i);
        assert!(
            p.rows[0]
                .cells
                .iter()
                .all(|c| matches!(c.glyph, Glyph::None | Glyph::Spark { .. }))
        );
        assert_eq!(
            p.rows[0].cells[column_index("atfm-3M-pctl")].text.as_ref(),
            "90"
        );
    }

    #[test]
    fn heat_shares_one_range_across_the_grid_and_term_one_y_range_across_the_list() {
        let p = prepare(&inputs(
            rows_with_levels(&[[10.0, 20.0], [30.0, 40.0]]),
            View::Term,
        ));
        // levels 10 and 40 are the range's ends: alpha 0 and the ceiling
        let lo = &p.rows[0].cells[column_index("atfm-1M-level")];
        let hi = &p.rows[1].cells[column_index("atfm-3M-level")];
        assert!(matches!(lo.glyph, Glyph::Heat(Some(a)) if a == 0.0));
        assert!(matches!(hi.glyph, Glyph::Heat(Some(a)) if (a - HEAT_CEILING).abs() < 1e-6));
        let t0 = &p.rows[0].cells[column_index("term")];
        let t1 = &p.rows[1].cells[column_index("term")];
        assert!(
            matches!((&t0.glyph, &t1.glyph), (Glyph::Spark { range: Some(r0), .. }, Glyph::Spark { range: Some(r1), .. }) if r0 == r1)
        );
        // the term glyph marks 3M among the listed tenors
        let mark = ALL_TENORS.iter().position(|t| *t == "3M");
        assert!(matches!(&t0.glyph, Glyph::Spark { mark: m, .. } if *m == mark));
    }

    #[test]
    fn widths_come_from_the_glyph_minimums_and_the_widest_text() {
        let p = prepare(&inputs(rows_with_pctl(0.9, 251), View::RichCheap));
        let pctl = column_index("atfm-3M-pctl");
        assert!(
            p.widths[pctl] >= widths::slider(2.0 * 7.0),
            "track + gap + a two-char label at 7 px advance"
        );
        let p = prepare(&inputs(rows_with_changes(&[1.0]), View::Changes));
        assert!(p.widths[column_index("spark-0")] >= sizes::WIDTH_30D);
        // A level column without a tint is as wide as its widest text or head.
        assert_eq!(
            p.widths[column_index("atfm-3M-level")],
            4.0 * 7.0,
            "`15.0` beside the `vol` head"
        );
    }

    #[test]
    fn a_long_board_sparkline_is_cut_to_the_columns_span() {
        let mut asked = ask_of(View::Changes);
        asked.sparklines = vec![(Measure::atfm("3M"), WINDOW_1Y)];
        let mut board = board_two();
        board.rows[0].sparks = vec![Some((0..365).map(f64::from).collect())];
        let rows = rows(&members(["SX5E"]), Some(&board), &no_reference, None);
        let mut i = inputs(&rows, View::Changes);
        i.asked = &asked;
        let p = prepare(&i);
        let Glyph::Spark { values, .. } = &p.rows[0].cells[column_index("spark-0")].glyph else {
            panic!("a spark");
        };
        assert_eq!(values.len(), 30);
        assert_eq!(values[0], 335.0, "the last thirty points");
        assert_eq!(values[29], 364.0);
    }

    #[test]
    fn a_spark_column_reads_its_measures_series_wherever_the_ask_lists_it() {
        // A band-open ask lists skew 3M first: the Changes spark still shows ATFM 3M.
        let skew = Measure::Skew { tenor: "3M".into() };
        let mut asked = ask_of(View::Changes);
        asked.sparklines = vec![(skew, WINDOW_1Y), (Measure::atfm("3M"), WINDOW_1Y)];
        let mut board = board_two();
        board.rows[0].sparks = vec![
            Some((0..365).map(|v| -f64::from(v)).collect()),
            Some((0..365).map(f64::from).collect()),
        ];
        let rows = rows(&members(["SX5E"]), Some(&board), &no_reference, None);
        let mut i = inputs(&rows, View::Changes);
        i.asked = &asked;
        let p = prepare(&i);
        let Glyph::Spark { values, .. } = &p.rows[0].cells[column_index("spark-0")].glyph else {
            panic!("a spark");
        };
        assert_eq!(values.len(), 30);
        assert_eq!(values[29], 364.0, "the ATFM series' tail, not the skew's");
    }

    #[test]
    fn a_non_finite_level_leaves_the_term_range_finite() {
        let p = prepare(&inputs(
            rows_with_levels(&[[10.0, f64::INFINITY], [30.0, 40.0]]),
            View::Term,
        ));
        let t = &p.rows[1].cells[column_index("term")];
        assert!(
            matches!(&t.glyph, Glyph::Spark { range: Some((lo, hi)), .. } if *lo == 10.0 && *hi == 40.0)
        );
        let t = &p.rows[0].cells[column_index("term")];
        assert!(
            matches!(&t.glyph, Glyph::Spark { values, .. } if values.iter().all(|v| !v.is_infinite())),
            "the infinite 3M is a gap"
        );
    }

    #[test]
    fn a_measure_the_board_was_not_asked_for_is_blank() {
        // A Levels column read against the Changes board: realized was not asked.
        let rows = rows(&members(["SX5E"]), Some(&board_two()), &no_reference, None);
        let mut i = inputs(&rows, View::Levels);
        let changes = ask_of(View::Changes);
        i.asked = &changes;
        let p = prepare(&i);
        let c = &p.rows[0].cells[column_index("rlz-5D-level")];
        assert_eq!(c.text.as_ref(), widths::DASH);
        assert!(c.muted);
        assert_eq!(
            p.rows[0].cells[column_index("atfm-3M-level")].text.as_ref(),
            "15.9"
        );
    }

    #[test]
    fn forward_change_is_a_percent_of_from_and_vega_is_in_thousands() {
        let fwd = Measure::Forward { tenor: "1M".into() };
        let asked = ask_of(View::Changes);
        let board = VolBoard {
            rows: vec![row_of(&asked, "SX5E", true, &|m| {
                (*m == fwd).then(|| (Some(4100.0), Some(change(41.0, 4059.0)), None))
            })],
            ..VolBoard::default()
        };
        let vega = HashMap::from([("SX5E".to_string(), -312_400.0)]);
        let rows = rows(&members(["SX5E"]), Some(&board), &no_reference, Some(&vega));
        let p = prepare(&inputs(&rows, View::Changes));
        assert_eq!(
            p.rows[0].cells[column_index("fwd-1M-change")].text.as_ref(),
            "+1.0"
        );
        assert_eq!(p.rows[0].cells[column_index("vega")].text.as_ref(), "-312");
        assert_eq!(col("vega").head, "vega k");
        assert_eq!(
            readout(&rows[0], &col("vega"), &ask_of(View::Changes), &clock_utc()),
            "SX5E · vega k -312"
        );
        assert_eq!(
            p.rows[0].cells[column_index("vega")].id.as_ref(),
            "7-SX5E-vega"
        );
    }

    #[test]
    fn the_readout_names_the_cell_its_change_and_its_rank() {
        let rows = rows(
            &members(["SX5E", "NEW"]),
            Some(&board_two()),
            &no_reference,
            None,
        );
        let text = readout(
            &rows[0],
            &col("atfm-3M-level"),
            &ask_of(View::Changes),
            &clock_utc(),
        );
        assert_eq!(
            text,
            "SX5E · ATFM 3M 15.9 · Δ1d +1.1 from 14.8 on 2026-10-09 · 1Y pctl 91 of 251"
        );
        let text = readout(
            &rows[1],
            &col("atfm-3M-level"),
            &ask_of(View::Changes),
            &clock_utc(),
        );
        assert_eq!(text, "NEW · not in surface");
    }

    #[test]
    fn sort_values_put_blanks_last_and_rank_pctl_by_rank() {
        let rows = rows(
            &members(["NEW", "SX5E"]),
            Some(&board_two()),
            &no_reference,
            None,
        );
        assert_eq!(
            sort_value(&rows[1], &col("atfm-3M-level"), &ask_of(View::Changes)),
            None
        );
        assert_eq!(
            sort_value(&rows[0], &col("atfm-3M-pctl"), &ask_of(View::Changes)),
            Some(0.91)
        );
    }

    #[test]
    fn the_header_counts_the_missing_and_names_the_failed_datasets() {
        let mut b = board_two();
        b.dataset_errors
            .push(("realized_vol".into(), "drifted".into()));
        let h = header(
            Some("europe_risk"),
            2,
            Lookback::W1,
            Some("|Δ 3M|"),
            Some(&b),
        );
        assert_eq!(h.title, "Vol · europe_risk");
        assert_eq!(h.not_in_surface, 1);
        assert_eq!(h.failed, ["realized_vol"]);
        assert_eq!(h.lookbacks[1], (Lookback::W1, true));
        assert_eq!(h.sort.as_deref(), Some("|Δ 3M|"));
        assert_eq!(
            header(None, 0, Lookback::D1, None, None).title,
            "Vol · New…"
        );
    }
}
