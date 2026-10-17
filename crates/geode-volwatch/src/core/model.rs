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
use geode_core::volboard::{Cell, Change, Lookback, Measure, Role, VolBoard, VolRow};
use geode_tile::rows::GridRow;
use geode_widgets::cell::delta_chip::{Chip, chip};
use geode_widgets::cell::heat::{heat, range};
use geode_widgets::cell::percent_slider::{Slider, slider};
use geode_widgets::cell::zero_bar::{Bar, bar};
use geode_widgets::cell::{DESIGN_REM, column_scale, widths};
use gpui::SharedString;

use super::views::{
    Ask, Column, ColumnKind, PCTL_MIN_COUNT, REALIZED, SURFACE, TITLE, VARSWAP, WINDOW_1Y, sortable,
};

/// Fill alpha of a change chip at the column's largest move: a tint under a
/// colored number; at 0.55 the chips read as filled pills and hid the grid's
/// rhythm.
pub const CHIP_CEILING: f32 = 0.25;
/// Heat tint alpha at the top of the grid's range.
pub const HEAT_CEILING: f32 = 0.40;
/// Zero bar fill alpha over its sign color.
pub const BAR_ALPHA: f32 = 0.75;
/// Design px a sortable head keeps clear beside its text for the table's
/// sort mark: an `Icon::size_3` (0.75 rem) in `p(px(2.))`, right of the
/// head. A head that forgot it loses its leading letters under the mark.
pub const SORT_MARK_DESIGN_PX: f32 = 0.75 * DESIGN_REM + 4.0;

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
///
/// The cut is by proportion and assumes the values fill `board_span` (one
/// a day across it). A member whose history is shorter than the span is
/// cut too hard: under the band's 1Y ask, ~40 days of history leave ~4 in
/// a 30d column until the next ask narrows the span. A known limit,
/// listed in `docs/open-items.md`; the spark shape stays as it is.
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
pub(crate) fn glyph_width(kind: &ColumnKind, text_px: f32) -> f32 {
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
        .map(|c| {
            let head = c.head.chars().count() as f32 * i.mono_advance_px;
            if sortable(&c.kind) {
                head + SORT_MARK_DESIGN_PX
            } else {
                head
            }
        })
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
    share_group_labels(i.columns, &mut col_px, i.mono_advance_px);
    Prepared {
        rows,
        widths: col_px,
    }
}

/// A group label spans its run of equal `Some` groups (as the table's
/// group row is built); a label wider than those columns' texts would
/// clip, so the deficit is split evenly across the run.
fn share_group_labels(columns: &[Column], col_px: &mut [f32], advance_px: f32) {
    let mut start = 0;
    while start < columns.len() {
        let Some(group) = columns[start].group.as_deref() else {
            start += 1;
            continue;
        };
        let end = start
            + columns[start..]
                .iter()
                .take_while(|c| c.group.as_deref() == Some(group))
                .count();
        let label_px = group.chars().count() as f32 * advance_px;
        let have: f32 = col_px[start..end].iter().sum();
        if label_px > have {
            let share = (label_px - have) / (end - start) as f32;
            for w in &mut col_px[start..end] {
                *w += share;
            }
        }
        start = end;
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

/// The dataset a measure's change is read from: its first input's role,
/// under the dataset names the tile asks with.
fn measure_dataset(m: &Measure) -> &'static str {
    match m.inputs().first().map(|i| i.role) {
        Some(Role::Realized) => REALIZED,
        Some(Role::Varswap) => VARSWAP,
        Some(Role::Surface) | None => SURFACE,
    }
}

/// The board note for `dataset` with its `<dataset>: ` prefix removed
/// (`history starts 2026-09-01`), if the board carries one.
fn note_for<'n>(notes: &'n [String], dataset: &str) -> Option<&'n str> {
    notes
        .iter()
        .find_map(|n| n.strip_prefix(dataset)?.strip_prefix(": "))
}

/// `SX5E · ATFM 3M 15.9 · Δ1d +1.1 from 14.8 on 2026-10-09 · 1Y pctl 91 of 251`, or `SX5E · not in surface`.
/// A cell with a level but no change says why when the board knows:
/// `SX5E · ATFM 3M 15.9 · Δ1d — · history starts 2026-09-01` (the board
/// note for the measure's dataset).
pub fn readout(row: &Row, column: &Column, asked: &Ask, notes: &[String], clock: &Clock) -> String {
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
            } else if c.is_some_and(|c| c.current.is_some()) {
                parts.push(format!(
                    "\u{394}{} {}",
                    asked.lookback.label(),
                    widths::DASH
                ));
                if let Some(note) = note_for(notes, measure_dataset(m)) {
                    parts.push(note.to_string());
                }
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

/// The band's surface columns as call deltas, in the desk's reading order
/// `10p 25p atm 25c 10c`.
pub const BAND_DELTAS: [f64; 5] = [0.90, 0.75, 0.50, 0.25, 0.10];
/// The band surface's column ids, parallel to its cells.
const BAND_IDS: [&str; 7] = ["10p", "25p", "atm", "25c", "10c", "skew", "delta"];
/// The band's three one-year sparklines: ATFM 3M, 25Δ skew 3M, realized 20D.
fn band_sparks() -> [Measure; 3] {
    [
        Measure::atfm("3M"),
        Measure::Skew { tenor: "3M".into() },
        Measure::Realized {
            window: "20D".into(),
        },
    ]
}

/// The inspection band for the cursor row: built when the row, the board
/// or the band's open state changes, never in render. Every number reads a
/// board cell through the ask the board answered; a measure the board
/// lacks shows a dash.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct BandModel {
    pub name: SharedString,
    /// Rows = tenors, columns = DELTAS (10p 25p atm 25c 10c as call deltas
    /// 0.90 0.75 0.50 0.25 0.10), then 25Δ skew and its Δ.
    pub surface: Vec<(SharedString, Vec<PreparedCell>)>,
    /// The Δ column's head over the lookback shown: `Δ1d`.
    pub delta_head: SharedString,
    /// ATFM 3M · 1Y, 25Δ skew 3M · 1Y, RV 20d · 1Y (spark glyphs with an end dot).
    pub sparks: [Option<PreparedCell>; 3],
    /// `1Y pctl 91 of 251 · range 11.2 – 24.6`, `1Y pctl 64 of 251 · range
    /// 4.9 – 7.1`, `IV−RV +3.4 · pctl 77 of 251`.
    pub captions: [SharedString; 3],
    /// `(Varswap 3M, 18.1)`, `(vs ATM, +2.2)`, `(Book vega, -312k)`,
    /// `(Source, surface 10:58 · realized 10:57 · varswap 10:58)`.
    pub context: Vec<(SharedString, SharedString)>,
    /// The row is older than `stale_after`: the name paints in the muted
    /// tone, as the grid's Underlying cell does; every number keeps its
    /// tone.
    pub stale: bool,
}

/// A rank as `<label> 91 of 251`.
fn pctl_text(label: &str, c: Option<&Cell>) -> Option<String> {
    let (rank, count) = c?.pctl?;
    finite(rank).map(|r| format!("{label} {:.0} of {count}", rank_percent(r)))
}

/// A sparkline's own `range 11.2 – 24.6`, over its finite points.
fn range_text(values: &[f64]) -> Option<String> {
    let (lo, hi) = range(values.iter().copied().filter(|v| v.is_finite()))?;
    Some(format!(
        "range {} \u{2013} {}",
        format_number(lo, &ONE_DP).text,
        format_number(hi, &ONE_DP).text
    ))
}

/// Parts joined by ` · `; a dash when every part is missing.
fn caption(parts: impl IntoIterator<Item = Option<String>>) -> SharedString {
    let parts: Vec<String> = parts.into_iter().flatten().collect();
    if parts.is_empty() {
        SharedString::new_static(widths::DASH)
    } else {
        parts.join(" \u{b7} ").into()
    }
}

/// `surface 10:58 · realized 10:57 · varswap 10:58`: the row's `as_of`,
/// then the oldest `current_at` among its cells reading each other
/// dataset; a dataset with no time reads `—`.
fn source_line(row: &Row, clock: &Clock) -> String {
    let at = |role: Role| -> Option<DateTime<Utc>> {
        if role == Role::Surface {
            return row.as_of;
        }
        row.board
            .as_ref()?
            .cells
            .iter()
            .filter(|c| c.measure.roles().contains(&role))
            .filter_map(|c| c.current_at)
            .min()
    };
    [Role::Surface, Role::Realized, Role::Varswap]
        .into_iter()
        .map(|role| match at(role) {
            Some(t) => format!("{} {}", role.name(), clock.hm(t)),
            None => format!("{} {}", role.name(), widths::DASH),
        })
        .collect::<Vec<_>>()
        .join(" \u{b7} ")
}

/// The cursor row's band: the fixed-delta surface over `tenors` (a tenor
/// the surface lacks shows dashes), heat-tinted over one range for the
/// whole table; the three one-year sparks and their captions; the context
/// pairs. A row the surface lacks is all dashes. A stale row's band keeps
/// its tone and colors like its grid row; its name (`stale`) and the
/// header's surface run carry the staleness.
pub fn prepare_band(i: &Inputs<'_>, row: &Row, tenors: &[String], clock: &Clock) -> BandModel {
    let read = |m: &Measure| -> Option<&Cell> {
        if row.present {
            cell(row, m, i.asked)
        } else {
            None
        }
    };
    let current = |m: &Measure| read(m).and_then(|c| c.current).and_then(finite);
    let skew = |t: &str| Measure::Skew { tenor: t.into() };
    let blank = |id: SharedString| PreparedCell {
        id,
        text: SharedString::new_static(widths::DASH),
        value: None,
        sign: Sign::Zero,
        glyph: Glyph::None,
        muted: true,
    };
    let vols: Vec<[Option<f64>; 5]> = tenors
        .iter()
        .map(|t| {
            BAND_DELTAS.map(|delta| {
                current(&Measure::Point {
                    tenor: t.clone(),
                    delta,
                })
            })
        })
        .collect();
    let deltas: Vec<Option<f64>> = tenors
        .iter()
        .map(|t| {
            if row.present {
                change_of(row, &skew(t), i.asked).and_then(|c| finite(c.value))
            } else {
                None
            }
        })
        .collect();
    let heat_range = range(vols.iter().flatten().flatten().copied());
    let delta_scale = column_scale(deltas.iter().flatten().copied());
    let id = |tenor: &str, col: &str| SharedString::from(format!("{}-band-{tenor}-{col}", i.tile));
    let surface = tenors
        .iter()
        .zip(&vols)
        .zip(&deltas)
        .map(|((t, vols), delta)| {
            let mut cells: Vec<PreparedCell> = vols
                .iter()
                .zip(BAND_IDS)
                .map(|(v, col)| match v {
                    Some(v) => {
                        let f = format_number(*v, &ONE_DP);
                        PreparedCell {
                            id: id(t, col),
                            text: f.text.into(),
                            value: Some(*v),
                            sign: f.sign,
                            glyph: if i.paint {
                                Glyph::Heat(
                                    heat_range.and_then(|(lo, hi)| heat(*v, lo, hi, HEAT_CEILING)),
                                )
                            } else {
                                Glyph::None
                            },
                            muted: false,
                        }
                    }
                    None => blank(id(t, col)),
                })
                .collect();
            cells.push(match current(&skew(t)) {
                Some(v) => {
                    let f = format_number(v, &ONE_DP);
                    PreparedCell {
                        id: id(t, BAND_IDS[5]),
                        text: f.text.into(),
                        value: Some(v),
                        sign: f.sign,
                        glyph: Glyph::None,
                        muted: false,
                    }
                }
                None => blank(id(t, BAND_IDS[5])),
            });
            cells.push(match delta {
                Some(v) => {
                    let f = signed(*v, &ONE_DP);
                    PreparedCell {
                        id: id(t, BAND_IDS[6]),
                        text: f.text.into(),
                        value: Some(*v),
                        sign: f.sign,
                        glyph: if i.paint {
                            Glyph::Chip(chip(*v, f.sign, delta_scale, CHIP_CEILING))
                        } else {
                            Glyph::None
                        },
                        muted: i.board_lookback != i.shown,
                    }
                }
                None => blank(id(t, BAND_IDS[6])),
            });
            (SharedString::from(t.clone()), cells)
        })
        .collect();

    let spark_values: [Option<Arc<[f64]>>; 3] = band_sparks().map(|m| {
        let (Some(series), span) = spark(row, &m, i.asked)? else {
            return None;
        };
        (!series.is_empty()).then(|| Arc::from(spark_tail(series, span, WINDOW_1Y)))
    });
    let sparks = [0, 1, 2].map(|k| {
        spark_values[k].as_ref().map(|values| PreparedCell {
            id: format!("{}-band-spark-{k}", i.tile).into(),
            text: SharedString::default(),
            value: values.last().copied().and_then(finite),
            sign: Sign::Zero,
            glyph: Glyph::Spark {
                values: Arc::clone(values),
                range: None,
                mark: None,
                end_dot: true,
            },
            muted: false,
        })
    });
    let [atfm, skew3m, _] = band_sparks();
    let ivrv = Measure::IvRv {
        tenor: "3M".into(),
        window: "20D".into(),
    };
    let spark_range = |k: usize| spark_values[k].as_deref().and_then(range_text);
    let captions = [
        caption([pctl_text("1Y pctl", read(&atfm)), spark_range(0)]),
        caption([pctl_text("1Y pctl", read(&skew3m)), spark_range(1)]),
        caption([
            current(&ivrv).map(|v| format!("IV\u{2212}RV {}", signed(v, &ONE_DP).text)),
            pctl_text("pctl", read(&ivrv)),
        ]),
    ];

    let dash = || SharedString::new_static(widths::DASH);
    let context = vec![
        (
            SharedString::new_static("Varswap 3M"),
            current(&Measure::Varswap { tenor: "3M".into() })
                .map_or_else(dash, |v| format_number(v, &ONE_DP).text.into()),
        ),
        (
            SharedString::new_static("vs ATM"),
            current(&Measure::VsSpread { tenor: "3M".into() })
                .map_or_else(dash, |v| signed(v, &ONE_DP).text.into()),
        ),
        (
            SharedString::new_static("Book vega"),
            row.vega.and_then(finite).map_or_else(dash, |v| {
                format!("{}k", format_number(v, &VEGA).text).into()
            }),
        ),
        (
            SharedString::new_static("Source"),
            source_line(row, clock).into(),
        ),
    ];
    BandModel {
        name: row.name.clone().into(),
        surface,
        delta_head: format!("\u{394}{}", i.shown.label()).into(),
        sparks,
        captions,
        context,
        stale: is_stale(row, i.now, i.stale_after),
    }
}

/// The header's left side, every string formatted when its inputs change
/// so paint only clones: the switch control's title, the member count,
/// the lookback chips (`Δ 1d` lit, `1w`, `1m`), the sort chip, the
/// missing members (`1 not in surface`) and the failed datasets.
#[derive(Debug, Clone, PartialEq)]
pub struct HeaderModel {
    pub title: SharedString,
    pub count: usize,
    pub count_text: SharedString,
    /// `Lookback::ALL` in order, the one in force true.
    pub lookbacks: [(Lookback, bool); 3],
    /// Parallel to `lookbacks`: the lit one carries the `Δ ` prefix.
    pub lookback_texts: [SharedString; 3],
    pub sort: Option<SharedString>,
    pub not_in_surface: usize,
    /// `<n> not in surface`; `None` while every member is in it.
    pub missing_text: Option<SharedString>,
    pub failed: Vec<SharedString>,
    pub notes: Vec<String>,
}

/// The glyph ahead of the title, naming the tile's kind at a glance.
pub const MARK: &str = "\u{25a4}";

/// The switch control's chevron.
pub const CHEVRON: &str = "\u{25be}";

impl HeaderModel {
    /// The chips' texts in paint order: the mark, the switch control, the
    /// count, the lookbacks, the sort, the missing, the failed.
    #[cfg(test)]
    pub fn chip_texts(&self) -> Vec<String> {
        let mut out = vec![MARK.to_string(), format!("{} {CHEVRON}", self.title)];
        out.push(self.count_text.to_string());
        out.extend(self.lookback_texts.iter().map(ToString::to_string));
        out.extend(self.sort.iter().map(ToString::to_string));
        out.extend(self.missing_text.iter().map(ToString::to_string));
        out.extend(self.failed.iter().map(ToString::to_string));
        out
    }
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
    let lookbacks = Lookback::ALL.map(|l| (l, l == shown));
    let not_in_surface = board.map_or(0, |b| b.missing.len());
    HeaderModel {
        title: format!("{TITLE} \u{b7} {}", list.unwrap_or("New\u{2026}")).into(),
        count,
        count_text: count.to_string().into(),
        lookbacks,
        lookback_texts: lookbacks.map(|(l, lit)| {
            if lit {
                format!("\u{394} {}", l.label()).into()
            } else {
                SharedString::new_static(l.label())
            }
        }),
        sort: sort.map(|s| s.to_string().into()),
        not_in_surface,
        missing_text: (not_in_surface > 0)
            .then(|| format!("{not_in_surface} not in surface").into()),
        failed: board.map_or_else(Vec::new, |b| {
            b.dataset_errors
                .iter()
                .map(|(d, _)| d.clone().into())
                .collect()
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
    fn the_chip_ceiling_is_a_tint_not_a_fill() {
        // The column's largest move takes the ceiling itself.
        let p = prepare(&inputs(rows_with_changes(&[-2.0, 0.5]), View::Changes));
        let col = column_index("atfm-3M-change");
        let Glyph::Chip(Some(Chip { alpha, .. })) = p.rows[0].cells[col].glyph else {
            panic!("the largest move has a chip");
        };
        assert!(
            alpha <= 0.25,
            "a chip's fill is a hint of magnitude; the number carries the sign: {alpha}"
        );
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
                &[],
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
    fn a_stale_row_keeps_its_cells_tone_and_mutes_only_its_flag() {
        let mut i = inputs(rows_with_changes(&[1.0]), View::Changes);
        i.stale_after = Duration::from_secs(60);
        let p = prepare(&i);
        assert!(p.rows[0].stale);
        // A blank cell is muted in every row; the valued ones keep their tone.
        assert!(
            p.rows[0]
                .cells
                .iter()
                .skip(1)
                .filter(|c| c.value.is_some())
                .all(|c| !c.muted),
            "stale does not mute the cells"
        );
        assert!(!p.rows[0].cells[column_index("atfm-3M-level")].muted);
        assert!(!p.rows[0].cells[column_index("atfm-3M-change")].muted);
        assert!(!p.rows[0].cells[0].muted, "the model leaves the name too");
    }

    /// A surface publishes once a day: last evening's row is fresh this
    /// afternoon, and only a row a day old is stale.
    #[test]
    fn the_default_stale_threshold_is_a_day() {
        assert_eq!(
            crate::content::DEFAULT_STALE_AFTER,
            Duration::from_secs(24 * 60 * 60)
        );
        let aged = |hours: i64| -> &'static [Row] {
            let asked = ask_of(View::Changes);
            let atfm3m = Measure::atfm("3M");
            let mut row = row_of(&asked, "U0", true, &|m| {
                (*m == atfm3m).then(|| (Some(15.0), Some(change(1.0, 14.0)), None))
            });
            row.as_of = Some(now() - chrono::Duration::hours(hours));
            rows_from(VolBoard {
                rows: vec![row],
                ..VolBoard::default()
            })
        };
        let mut i = inputs(aged(16), View::Changes);
        i.stale_after = crate::content::DEFAULT_STALE_AFTER;
        assert!(
            !prepare(&i).rows[0].stale,
            "yesterday's 17:00 publish is fresh at 09:00"
        );
        let mut i = inputs(aged(25), View::Changes);
        i.stale_after = crate::content::DEFAULT_STALE_AFTER;
        assert!(
            prepare(&i).rows[0].stale,
            "a day without a publish is stale"
        );
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
        // A level column without a tint is as wide as its widest text or
        // its head with the sort mark, whichever is wider.
        assert_eq!(
            p.widths[column_index("atfm-3M-level")],
            (3.0 * 7.0 + SORT_MARK_DESIGN_PX).max(4.0 * 7.0),
            "`vol` plus the sort mark, wider than `15.0`"
        );
    }

    #[test]
    fn a_sortable_head_reserves_the_sort_mark() {
        let p = prepare(&inputs(rows_with_changes(&[1.0]), View::Changes));
        let vol = column_index("atfm-1M-level");
        let head_px = "vol".chars().count() as f32 * 7.0;
        assert!(p.widths[vol] >= head_px + SORT_MARK_DESIGN_PX);
        // A spark column is not sortable: as wide as its label, no mark.
        let spark = column_index("spark-0");
        let label_px = "ATFM 3M \u{b7} 30d".chars().count() as f32 * 7.0;
        assert!(p.widths[spark] >= label_px);
        assert!(p.widths[spark] < label_px + SORT_MARK_DESIGN_PX);
    }

    #[test]
    fn a_group_label_wider_than_its_columns_is_shared_across_them() {
        // Changes: "25Δ skew 3M" over `pts` and `Δ`, both blank in this board.
        let p = prepare(&inputs(rows_with_changes(&[1.0]), View::Changes));
        let (a, b) = (
            column_index("skew-3M-level"),
            column_index("skew-3M-change"),
        );
        let label_px = "25\u{394} skew 3M".chars().count() as f32 * 7.0;
        assert!(p.widths[a] + p.widths[b] >= label_px);
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
            readout(
                &rows[0],
                &col("vega"),
                &ask_of(View::Changes),
                &[],
                &clock_utc()
            ),
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
            &[],
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
            &[],
            &clock_utc(),
        );
        assert_eq!(text, "NEW · not in surface");
    }

    #[test]
    fn a_cell_without_a_change_names_where_its_history_starts() {
        let asked = ask_of(View::Changes);
        let atfm3m = Measure::atfm("3M");
        let board = VolBoard {
            rows: vec![row_of(&asked, "SX5E", true, &|m| {
                (*m == atfm3m).then_some((Some(15.9), None, None))
            })],
            notes: vec![
                "realized_vol: history starts 2026-08-01".into(),
                "vol_surface: history starts 2026-09-01".into(),
            ],
            ..VolBoard::default()
        };
        let rows = rows(&members(["SX5E"]), Some(&board), &no_reference, None);
        for id in ["atfm-3M-level", "atfm-3M-change"] {
            assert_eq!(
                readout(&rows[0], &col(id), &asked, &board.notes, &clock_utc()),
                "SX5E · ATFM 3M 15.9 · Δ1d — · history starts 2026-09-01",
                "{id}"
            );
        }
        assert_eq!(
            readout(&rows[0], &col("atfm-3M-change"), &asked, &[], &clock_utc()),
            "SX5E · ATFM 3M 15.9 · Δ1d —",
            "no note, no reason"
        );
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

    /// SX5E answered for the Changes ask over `listed` tenors, band open
    /// or not (the board omits 2Y either way):
    /// every vol point at 10 + its tenor index + 10 × its delta (ATFM 3M
    /// at 16.0), skew 3M 5.5 moved +0.4,
    /// ATFM 3M ranked 0.91 of 251, IV−RV 3.4 ranked 0.77, varswap 18.1,
    /// vs spread 2.2; sparks rise from 15.0 to 17.9.
    fn band_row(listed: &[String], band: bool) -> (Ask, Vec<Row>) {
        let mut asked = ask(View::Changes, listed, Lookback::D1, band);
        asked
            .measures
            .retain(|m| !matches!(m, Measure::Point { tenor, .. } if tenor == "2Y"));
        let ivrv = Measure::IvRv {
            tenor: "3M".into(),
            window: "20D".into(),
        };
        let board = VolBoard {
            rows: vec![row_of(&asked, "SX5E", true, &|m| match m {
                Measure::Point { tenor, delta } if *delta == 0.5 && tenor == "3M" => {
                    Some((Some(16.0), None, Some((0.91, 251))))
                }
                Measure::Point { tenor, delta } => {
                    let at = listed.iter().position(|t| t == tenor).unwrap();
                    Some((Some(10.0 + at as f64 + 10.0 * delta), None, None))
                }
                Measure::Skew { tenor } if tenor == "3M" => {
                    Some((Some(5.5), Some(change(0.4, 5.1)), Some((0.64, 251))))
                }
                m if *m == ivrv => Some((Some(3.4), None, Some((0.77, 251)))),
                Measure::Varswap { .. } => Some((Some(18.1), None, None)),
                Measure::VsSpread { .. } => Some((Some(2.2), None, None)),
                _ => None,
            })],
            ..VolBoard::default()
        };
        let rows = rows(&members(["SX5E"]), Some(&board), &no_reference, None);
        (asked, rows)
    }

    #[test]
    fn the_band_reads_the_surface_by_tenor_and_delta_and_captions_its_sparks() {
        let listed = tenors();
        let (asked, rows) = band_row(&listed, true);
        let mut i = inputs(&rows, View::Changes);
        i.asked = &asked;
        let b = prepare_band(&i, &rows[0], &listed, &clock_utc());
        assert_eq!(b.name.as_ref(), "SX5E");
        assert_eq!(
            b.surface
                .iter()
                .map(|(t, _)| t.as_ref())
                .collect::<Vec<_>>(),
            ALL_TENORS
        );
        let three_m = &b.surface[2].1;
        assert_eq!(three_m[2].text.as_ref(), "16.0", "atm is ATFM");
        // 3M is tenor 2: each column reads its own call delta's point.
        assert_eq!(three_m[0].text.as_ref(), "21.0", "10p reads 0.90");
        assert_eq!(three_m[1].text.as_ref(), "19.5", "25p reads 0.75");
        assert_eq!(three_m[3].text.as_ref(), "14.5", "25c reads 0.25");
        assert_eq!(three_m[4].text.as_ref(), "13.0", "10c reads 0.10");
        assert_eq!(three_m[5].text.as_ref(), "5.5", "25Δ skew");
        assert_eq!(three_m[6].text.as_ref(), "+0.4", "its Δ over the lookback");
        assert_eq!(three_m[6].id.as_ref(), "7-band-3M-delta");
        assert_eq!(b.delta_head.as_ref(), "Δ1d");
        let two_y = &b.surface[6].1;
        assert!(
            two_y
                .iter()
                .all(|c| c.text.as_ref() == widths::DASH && c.muted),
            "a tenor the board lacks is dashes"
        );
        // One heat range over the whole table: 1M 10c's 11.0 is its floor.
        assert!(matches!(b.surface[0].1[4].glyph, Glyph::Heat(Some(a)) if a == 0.0));
        assert!(b.sparks.iter().all(Option::is_some));
        assert_eq!(
            b.captions[0].as_ref(),
            "1Y pctl 91 of 251 · range 15.0 – 17.9"
        );
        assert_eq!(
            b.captions[1].as_ref(),
            "1Y pctl 64 of 251 · range 15.0 – 17.9"
        );
        assert_eq!(b.captions[2].as_ref(), "IV−RV +3.4 · pctl 77 of 251");
        let ctx: Vec<(&str, &str)> = b
            .context
            .iter()
            .map(|(l, v)| (l.as_ref(), v.as_ref()))
            .collect();
        assert_eq!(ctx[0], ("Varswap 3M", "18.1"));
        assert_eq!(ctx[1], ("vs ATM", "+2.2"));
        assert_eq!(ctx[2], ("Book vega", widths::DASH));
        assert_eq!(
            ctx[3],
            ("Source", "surface 15:00 · realized 15:00 · varswap 15:00")
        );
        // With a book vega the pair reads the column's thousands and a `k`.
        let mut with_vega = rows[0].clone();
        with_vega.vega = Some(-312_000.0);
        let b = prepare_band(&i, &with_vega, &listed, &clock_utc());
        assert_eq!(b.context[2].1.as_ref(), "-312k");
    }

    /// The band follows the grid: a stale row's surface, skew, delta chip
    /// and sparks keep their tone and its name carries the staleness; only
    /// a lookback mismatch mutes the chip.
    #[test]
    fn a_stale_rows_band_keeps_its_cells_tone() {
        let listed = tenors();
        let (asked, rows) = band_row(&listed, true);
        let mut i = inputs(&rows, View::Changes);
        i.asked = &asked;
        i.stale_after = Duration::from_secs(60);
        assert!(prepare(&i).rows[0].stale, "the fixture row is an hour old");
        let b = prepare_band(&i, &rows[0], &listed, &clock_utc());
        let three_m = &b.surface[2].1;
        assert!(matches!(three_m[2].glyph, Glyph::Heat(Some(_))));
        assert!(!three_m[2].muted, "a heat tint keeps its tone");
        assert!(!three_m[5].muted, "the skew keeps its tone");
        assert!(matches!(three_m[6].glyph, Glyph::Chip(_)));
        assert!(!three_m[6].muted, "the delta chip keeps its color");
        let spark = b.sparks[0].as_ref().expect("the ATFM spark");
        assert!(!spark.muted, "a spark keeps its tone");
        assert!(
            b.stale,
            "the name carries the staleness, as the grid's does"
        );
        i.stale_after = Duration::from_secs(3 * 86_400);
        assert!(
            !prepare_band(&i, &rows[0], &listed, &clock_utc()).stale,
            "fresh under a wider threshold"
        );
        i.stale_after = Duration::from_secs(60);
        i.shown = Lookback::W1;
        let b = prepare_band(&i, &rows[0], &listed, &clock_utc());
        assert!(
            b.surface[2].1[6].muted,
            "a chip from another lookback still paints muted"
        );
        assert!(!b.surface[2].1[2].muted);
    }

    #[test]
    fn the_band_of_a_board_asked_without_it_is_dashes() {
        let listed = tenors();
        // The board answered the closed ask: no point but ATFM, no 1Y spark.
        let (asked, rows) = band_row(&listed, false);
        let mut i = inputs(&rows, View::Changes);
        i.asked = &asked;
        let b = prepare_band(&i, &rows[0], &listed, &clock_utc());
        assert_eq!(b.surface[0].1[0].text.as_ref(), widths::DASH, "10p 1M");
        assert_eq!(b.surface[0].1[5].text.as_ref(), widths::DASH, "skew 1M");
        assert!(b.sparks[1].is_none() && b.sparks[2].is_none());
        assert_eq!(
            b.captions[1].as_ref(),
            "1Y pctl 64 of 251",
            "no spark, no range"
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
