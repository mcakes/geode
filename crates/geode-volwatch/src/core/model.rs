//! The prepared model: a landed [`VolBoard`] turned into everything render
//! reads — each cell's text, glyph core, id and muted flag, the column
//! widths, the cursor readout and the header model — built once per board,
//! view, lens or paint change, never per frame.
//!
//! The only arithmetic here is display: a rank times a hundred, a number
//! rounded to what the cell shows, and the scales glyphs are measured
//! against. The companion numbers themselves (a change, a z, a rank) come
//! from the lens over a board cell.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use chrono::{DateTime, Utc};
use geode_chart::spark::sizes;
use geode_core::clock::Clock;
use geode_core::format::{Formatted, Sign, format_number};
use geode_core::view::{Colour, ColumnFormat, Negative, Scale};
use geode_core::volboard::{Cell, Change, Lookback, Measure, Role, VolBoard, VolRow};
use geode_core::volviews::Cells;
use geode_tile::rows::GridRow;
use geode_widgets::cell::delta_chip::{Chip, chip};
use geode_widgets::cell::heat::{heat, range};
use geode_widgets::cell::{DESIGN_REM, column_scale, widths};
use gpui::SharedString;

use super::grouping::GroupInfo;
use super::lens::{
    Lens, LensState, Metric, PCTL_MIN_COUNT, Shown, Window, Z_CEILING, companion, pctl_side,
    z_score,
};
use super::views::{
    Ask, Column, ColumnKind, REALIZED, SURFACE, TITLE, VARSWAP, family_label, sortable,
};

/// Fill alpha of a chip at its scale's top (the column's largest move, a z
/// of `Z_CEILING`, a rank at either end): a tint under a colored number;
/// at 0.55 the chips read as filled pills and hid the grid's rhythm.
pub const CHIP_CEILING: f32 = 0.25;
/// Heat tint alpha at the top of the band's surface range. The band's
/// own: the grid paints no heat.
pub const BAND_HEAT_CEILING: f32 = 0.40;
/// Design px a sortable head keeps clear beside its text for the table's
/// sort mark: an `Icon::size_3` (0.75 rem) in `p(px(2.))`, right of the
/// head. A head that forgot it loses its leading letters under the mark.
pub const SORT_MARK_DESIGN_PX: f32 = 0.75 * DESIGN_REM + 4.0;

/// Levels, changes and z: one decimal.
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

/// One member's row as the board answered it, joined with the list and
/// reference; or a group row over such members (`group` is `Some`), whose
/// cells are the members' means.
#[derive(Debug, Clone, PartialEq)]
pub struct Row {
    pub name: String,
    pub reference: Option<String>,
    /// In the board and not among its `missing`. A group row is present.
    pub present: bool,
    pub as_of: Option<DateTime<Utc>>,
    pub board: Option<VolRow>,
    /// Book vega; a group's is its members' sum.
    pub vega: Option<f64>,
    /// The enclosing group path: one label per grouping level above this
    /// row. Empty for a flat list's member and for a root group.
    pub path: Vec<String>,
    /// `Some` for a group row: its label, its members and its averaged
    /// cells.
    pub group: Option<GroupInfo>,
}

impl GridRow for Row {
    fn name(&self) -> &str {
        &self.name
    }

    /// `/` searches members; a group row stays while a member under it
    /// matches, and its own label never matches.
    fn narrow_fields(&self) -> Vec<&str> {
        if self.group.is_some() {
            return Vec::new();
        }
        vec![&self.name, self.reference.as_deref().unwrap_or("")]
    }

    fn depth(&self) -> usize {
        self.path.len()
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
        path: Vec::new(),
        group: None,
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
    /// A companion number in its chip: the pair's companion column, or a
    /// single column after `m`.
    Chip(Option<Chip>),
    /// A level over its companion's sign tint: a single column showing
    /// the level. `None` when the cell has no companion.
    Tint(Option<Chip>),
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
    /// The lookback the board was asked with; a change paints muted when
    /// it differs from the lens's.
    pub board_lookback: Lookback,
    /// The window the board was asked with (`None` before any, or for an
    /// ask without a measure column); a rank or z paints muted when it
    /// differs from the lens's window.
    pub board_window: Option<Duration>,
    /// The lens in force: which companion stands beside a level, and
    /// which number a single column shows.
    pub lens: LensState,
    /// The view's cell shape, carried beside its columns.
    pub cells: Cells,
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

/// The board cell a column of `kind` reads, for a row the surface holds.
fn column_cell<'r>(row: &'r Row, kind: &ColumnKind, asked: &Ask) -> Option<&'r Cell> {
    if !row.present {
        return None;
    }
    match kind {
        ColumnKind::Level { measure }
        | ColumnKind::Companion { measure }
        | ColumnKind::Single { measure } => cell(row, measure, asked),
        ColumnKind::Underlying | ColumnKind::Spark { .. } | ColumnKind::Vega => None,
    }
}

/// Whether `kind` shows a companion number under `lens` (the pair's
/// companion column, or a single column after `m`).
fn shows_companion(kind: &ColumnKind, lens: &LensState) -> bool {
    matches!(kind, ColumnKind::Companion { .. })
        || (matches!(kind, ColumnKind::Single { .. }) && lens.shown == Shown::Companion)
}

/// The number a cell shows, before formatting; `None` for a blank and for
/// any row the surface lacks.
fn value(row: &Row, kind: &ColumnKind, asked: &Ask, lens: &LensState) -> Option<f64> {
    if !row.present {
        return None;
    }
    if shows_companion(kind, lens) {
        return companion(column_cell(row, kind, asked)?, lens);
    }
    match kind {
        ColumnKind::Level { .. } | ColumnKind::Single { .. } => {
            column_cell(row, kind, asked)?.current.and_then(finite)
        }
        ColumnKind::Vega => row.vega.and_then(finite),
        ColumnKind::Underlying | ColumnKind::Companion { .. } | ColumnKind::Spark { .. } => None,
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

/// A companion as the cell shows it: a change or a z with its sign at
/// one decimal, a rank as a whole percent with no sign.
fn companion_text(v: f64, lens: &LensState) -> Formatted {
    match (lens.lens, lens.metric) {
        (Lens::Rich, Metric::Pctl) => Formatted {
            text: format!("{:.0}", rank_percent(v)),
            sign: Sign::Zero,
        },
        _ => signed(v, &ONE_DP),
    }
}

/// The displayed text of a value in a column of `kind`.
fn text(kind: &ColumnKind, v: f64, lens: &LensState) -> Formatted {
    if shows_companion(kind, lens) {
        return companion_text(v, lens);
    }
    match kind {
        ColumnKind::Level { .. } | ColumnKind::Single { .. } => format_number(v, &ONE_DP),
        ColumnKind::Vega => format_number(v, &VEGA),
        ColumnKind::Underlying | ColumnKind::Companion { .. } | ColumnKind::Spark { .. } => {
            Formatted {
                text: String::new(),
                sign: Sign::Zero,
            }
        }
    }
}

/// A companion at the magnitude its text shows: a change or a z rounded
/// to one decimal, a rank to its whole percent. The chip tints what the
/// eye reads, so a z displayed `1.4` tints as 1.4 and a move displayed
/// `0.0` tints as nothing.
fn displayed_companion(v: f64, lens: &LensState) -> f64 {
    match (lens.lens, lens.metric) {
        (Lens::Rich, Metric::Pctl) => rank_percent(v) / 100.0,
        _ => (v * 10.0).round() / 10.0,
    }
}

/// The companion's chip: a Δ against the column's scale, a z against
/// `Z_CEILING`, a rank by its side of the median. `v` is the displayed
/// companion and `sign` its text's sign.
fn companion_chip(v: f64, sign: Sign, scale: f64, lens: &LensState) -> Option<Chip> {
    match (lens.lens, lens.metric) {
        (Lens::Moves, _) => chip(v, sign, scale, CHIP_CEILING),
        (Lens::Rich, Metric::Z) => chip(v, sign, Z_CEILING, CHIP_CEILING),
        (Lens::Rich, Metric::Pctl) => {
            let side = pctl_side(v);
            let sign = if side > 0.0 {
                Sign::Positive
            } else if side < 0.0 {
                Sign::Negative
            } else {
                Sign::Zero
            };
            chip(side, sign, 1.0, CHIP_CEILING)
        }
    }
}

/// The tail of a board sparkline a column of `wanted` span shows: the
/// board answers the longest span any column or the band asked.
///
/// The cut is by proportion and assumes the values fill `board_span` (one
/// a day across it). A member whose history is shorter than the span is
/// cut too hard: under the band's window-long ask, ~40 days of history
/// leave ~4 in a 30d column until the next ask narrows the span. A known
/// limit, listed in `docs/open-items.md`; the spark shape stays as it is.
fn spark_tail(values: &[f64], board_span: Duration, wanted: Duration) -> &[f64] {
    let (have, want) = (board_span.as_secs(), wanted.as_secs());
    if have <= want || have == 0 {
        return values;
    }
    let n = values.len();
    let keep = (n as u128 * u128::from(want)).div_ceil(u128::from(have)) as usize;
    &values[n - keep.min(n)..]
}

fn is_stale(row: &Row, now: DateTime<Utc>, stale_after: Duration) -> bool {
    row.present
        && row
            .as_of
            .is_some_and(|t| (now - t).to_std().is_ok_and(|age| age > stale_after))
}

/// Text px → the narrowest cell a column of `kind` paints it in. A
/// single column keeps the chip's room under either shown number, so `m`
/// never resizes the grid.
pub(crate) fn glyph_width(kind: &ColumnKind, text_px: f32) -> f32 {
    match kind {
        ColumnKind::Companion { .. } | ColumnKind::Single { .. } => widths::chip(text_px),
        ColumnKind::Spark { .. } => text_px.max(sizes::WIDTH_30D),
        ColumnKind::Level { .. } | ColumnKind::Underlying | ColumnKind::Vega => text_px,
    }
}

/// The scale a column's chips tint against: under moves, the largest
/// displayed companion among the rows (so the column's largest move takes
/// the ceiling itself); under rich/cheap the chip has its own ceiling and
/// the column none.
fn companion_scale(i: &Inputs<'_>, kind: &ColumnKind) -> f64 {
    let (ColumnKind::Companion { .. } | ColumnKind::Single { .. }) = kind else {
        return 0.0;
    };
    if i.lens.lens != Lens::Moves {
        return 0.0;
    }
    column_scale(
        i.rows
            .iter()
            .filter_map(|r| column_cell(r, kind, i.asked))
            .filter_map(|c| companion(c, &i.lens))
            .map(|v| displayed_companion(v, &i.lens)),
    )
}

/// Every row's cells, prepared once.
pub fn prepare(i: &Inputs<'_>) -> Prepared {
    let scales: Vec<f64> = i
        .columns
        .iter()
        .map(|c| companion_scale(i, &c.kind))
        .collect();

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
        .map(|row| {
            let stale = is_stale(row, i.now, i.stale_after);
            let cells = i
                .columns
                .iter()
                .enumerate()
                .map(|(ci, column)| {
                    let v = value(row, &column.kind, i.asked, &i.lens);
                    let mut cell = prepare_cell(i, row, column, v, scales[ci]);
                    if column.kind == ColumnKind::Underlying {
                        cell.text = SharedString::from(row.name.clone());
                        cell.muted = !row.present;
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

/// Whether a companion over `c` paints muted: under moves, a change from
/// another lookback than the lens's; under rich/cheap, a rank or z from
/// another window than the lens's (the header chip names the new window
/// while the old board's numbers are still up), or over too short a
/// history. A level is never muted by its companion; a single column's
/// tint is dropped instead.
fn companion_muted(i: &Inputs<'_>, c: &Cell) -> bool {
    match i.lens.lens {
        Lens::Moves => i.board_lookback != i.lens.lookback,
        Lens::Rich => {
            i.board_window != Some(i.lens.window.duration())
                || c.pctl.is_some_and(|(_, count)| count < PCTL_MIN_COUNT)
        }
    }
}

/// One cell's text, sign, glyph and muted flag; the id and the
/// Underlying name are the caller's.
fn prepare_cell(
    i: &Inputs<'_>,
    row: &Row,
    column: &Column,
    v: Option<f64>,
    scale: f64,
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
    let Formatted { text: t, sign } = text(&column.kind, v, &i.lens);
    let board_cell = column_cell(row, &column.kind, i.asked);
    let shows_companion = shows_companion(&column.kind, &i.lens);
    let glyph = if !i.paint {
        Glyph::None
    } else if shows_companion {
        Glyph::Chip(companion_chip(
            displayed_companion(v, &i.lens),
            sign,
            scale,
            &i.lens,
        ))
    } else if matches!(column.kind, ColumnKind::Single { .. }) {
        // A tint the pair would paint muted (a stale lookback, a short
        // history) is a wrong color under a right number: it goes
        // entirely, and the level keeps its text and tone.
        Glyph::Tint(
            board_cell
                .filter(|c| !companion_muted(i, c))
                .and_then(|c| companion(c, &i.lens))
                .and_then(|cv| {
                    let f = companion_text(cv, &i.lens);
                    companion_chip(displayed_companion(cv, &i.lens), f.sign, scale, &i.lens)
                }),
        )
    } else {
        Glyph::None
    };
    let muted = shows_companion && board_cell.is_some_and(|c| companion_muted(i, c));
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
        ColumnKind::Level { measure }
        | ColumnKind::Companion { measure }
        | ColumnKind::Single { measure }
        | ColumnKind::Spark { measure, .. } => Some(measure),
        ColumnKind::Underlying | ColumnKind::Vega => None,
    }
}

/// The cell's value for sorting: the number the column shows under
/// `lens` (a rank as 0..1); `None` sorts last.
pub fn sort_value(row: &Row, column: &Column, asked: &Ask, lens: &LensState) -> Option<f64> {
    value(row, &column.kind, asked, lens)
}

/// The dataset a measure's change is read from: its first input's role,
/// under the dataset names the tile asks with.
fn measure_dataset(m: &Measure) -> String {
    match m.inputs().first().map(|i| i.role.clone()) {
        Some(Role::Realized) => REALIZED.into(),
        Some(Role::Varswap) => VARSWAP.into(),
        Some(Role::Table(dataset)) => dataset,
        Some(Role::Surface) | None => SURFACE.into(),
    }
}

/// The board note for `dataset` with its `<dataset>: ` prefix removed
/// (`history starts 2026-09-01`), if the board carries one.
fn note_for<'n>(notes: &'n [String], dataset: &str) -> Option<&'n str> {
    notes
        .iter()
        .find_map(|n| n.strip_prefix(dataset)?.strip_prefix(": "))
}

/// The window word of the ask the board answered: `1Y`, or `window` for
/// a span that is none of the three.
fn window_word(asked: &Ask) -> &'static str {
    asked
        .window
        .and_then(Window::from_duration)
        .map_or("window", Window::label)
}

/// `SX5E · ATFM 3M 19.6 · Δ1d +0.8 from 18.8 on 2026-10-22 · z 1Y +1.4 ·
/// pctl 1Y 91 of 251 · mean 16.9 · sd 1.9`, or `SX5E · not in surface`.
/// Every part reads the ask the board answered, whatever the lens shows,
/// so the readout never names a lookback or window the numbers were not
/// computed over; the z, the rank and the stats appear only when the cell
/// carries them, and a z blank for a short history says so: `z 1Y — (12
/// of 60)`. A cell with a level but no change says why when the board
/// knows: `SX5E · ATFM 3M 15.9 · Δ1d — · history starts 2026-09-01` (the
/// board note for the measure's dataset).
pub fn readout(
    row: &Row,
    column: &Column,
    asked: &Ask,
    notes: &[String],
    clock: &Clock,
    _lens: &LensState,
) -> String {
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
            parts.push(format!("{} {current}", family_label(m)));
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
                if let Some(note) = note_for(notes, &measure_dataset(m)) {
                    parts.push(note.to_string());
                }
            }
            if let Some(c) = c {
                let window = window_word(asked);
                if let Some(stats) = c.stats {
                    match z_score(c) {
                        Some(z) => parts.push(format!("z {window} {}", signed(z, &ONE_DP).text)),
                        None if stats.count < PCTL_MIN_COUNT => parts.push(format!(
                            "z {window} {} ({} of {PCTL_MIN_COUNT})",
                            widths::DASH,
                            stats.count
                        )),
                        None => {}
                    }
                }
                if let Some(text) = pctl_text(&format!("pctl {window}"), Some(c)) {
                    parts.push(text);
                }
                if let Some(stats) = c.stats {
                    parts.push(format!(
                        "mean {} \u{b7} sd {}",
                        format_number(stats.mean, &ONE_DP).text,
                        format_number(stats.sd, &ONE_DP).text
                    ));
                }
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
/// The band's three window-long sparklines: ATFM 3M, 25Δ skew 3M, realized 20D.
fn band_sparks() -> [Measure; 3] {
    [
        Measure::atfm("3M"),
        Measure::Skew { tenor: "3M".into() },
        Measure::Realized {
            window: "20D".into(),
        },
    ]
}

/// One band surface cell: a prepared cell and, for the five vol points,
/// its heat tint over the band's one range. The heat is the band's own
/// glyph: the grid tints by sign, the band by level.
#[derive(Debug, Clone, PartialEq)]
pub struct BandCell {
    pub cell: PreparedCell,
    /// Alpha in `0..=BAND_HEAT_CEILING`; `None` for a blank, the skew and
    /// its Δ.
    pub heat: Option<f32>,
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
    pub surface: Vec<(SharedString, Vec<BandCell>)>,
    /// The Δ column's head over the lookback shown: `Δ1d`.
    pub delta_head: SharedString,
    /// ATFM 3M, 25Δ skew 3M, RV 20d over the window the board answered
    /// (spark glyphs with an end dot).
    pub sparks: [Option<PreparedCell>; 3],
    /// `pctl 1Y 91 of 251 · range 11.2 – 24.6`, `pctl 1Y 64 of 251 · range
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
    let at = |role: &Role| -> Option<DateTime<Utc>> {
        if *role == Role::Surface {
            return row.as_of;
        }
        row.board
            .as_ref()?
            .cells
            .iter()
            .filter(|c| c.measure.roles().contains(role))
            .filter_map(|c| c.current_at)
            .min()
    };
    [Role::Surface, Role::Realized, Role::Varswap]
        .iter()
        .map(|role| match at(role) {
            Some(t) => format!("{} {}", role.name(), clock.hm(t)),
            None => format!("{} {}", role.name(), widths::DASH),
        })
        .collect::<Vec<_>>()
        .join(" \u{b7} ")
}

/// The cursor row's band: the fixed-delta surface over `tenors` (a tenor
/// the surface lacks shows dashes), heat-tinted over one range for the
/// whole table; the three window-long sparks and their captions; the
/// context pairs. A row the surface lacks is all dashes. A stale row's
/// band keeps its tone and colors like its grid row; its name (`stale`)
/// and the header's surface run carry the staleness.
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
    let blank = |id: SharedString| BandCell {
        cell: PreparedCell {
            id,
            text: SharedString::new_static(widths::DASH),
            value: None,
            sign: Sign::Zero,
            glyph: Glyph::None,
            muted: true,
        },
        heat: None,
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
            let mut cells: Vec<BandCell> = vols
                .iter()
                .zip(BAND_IDS)
                .map(|(v, col)| match v {
                    Some(v) => {
                        let f = format_number(*v, &ONE_DP);
                        BandCell {
                            cell: PreparedCell {
                                id: id(t, col),
                                text: f.text.into(),
                                value: Some(*v),
                                sign: f.sign,
                                glyph: Glyph::None,
                                muted: false,
                            },
                            heat: if i.paint {
                                heat_range.and_then(|(lo, hi)| heat(*v, lo, hi, BAND_HEAT_CEILING))
                            } else {
                                None
                            },
                        }
                    }
                    None => blank(id(t, col)),
                })
                .collect();
            cells.push(match current(&skew(t)) {
                Some(v) => {
                    let f = format_number(v, &ONE_DP);
                    BandCell {
                        cell: PreparedCell {
                            id: id(t, BAND_IDS[5]),
                            text: f.text.into(),
                            value: Some(v),
                            sign: f.sign,
                            glyph: Glyph::None,
                            muted: false,
                        },
                        heat: None,
                    }
                }
                None => blank(id(t, BAND_IDS[5])),
            });
            cells.push(match delta {
                Some(v) => {
                    let f = signed(*v, &ONE_DP);
                    BandCell {
                        cell: PreparedCell {
                            id: id(t, BAND_IDS[6]),
                            text: f.text.into(),
                            value: Some(*v),
                            sign: f.sign,
                            glyph: if i.paint {
                                Glyph::Chip(chip(*v, f.sign, delta_scale, CHIP_CEILING))
                            } else {
                                Glyph::None
                            },
                            muted: i.board_lookback != i.lens.lookback,
                        },
                        heat: None,
                    }
                }
                None => blank(id(t, BAND_IDS[6])),
            });
            (SharedString::from(t.clone()), cells)
        })
        .collect();

    // The band asks its sparks at the window in force, so the board's
    // span is the span shown: no tail to cut.
    let spark_values: [Option<Arc<[f64]>>; 3] = band_sparks().map(|m| {
        let (Some(series), _) = spark(row, &m, i.asked)? else {
            return None;
        };
        (!series.is_empty()).then(|| Arc::from(series.as_slice()))
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
    let pctl_label = format!("pctl {}", window_word(i.asked));
    let captions = [
        caption([pctl_text(&pctl_label, read(&atfm)), spark_range(0)]),
        caption([pctl_text(&pctl_label, read(&skew3m)), spark_range(1)]),
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
        delta_head: format!("\u{394}{}", i.lens.lookback.label()).into(),
        sparks,
        captions,
        context,
        stale: is_stale(row, i.now, i.stale_after),
    }
}

/// What the header's three lens chips are: the lookbacks under moves,
/// the windows under rich/cheap.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChipKind {
    Lookback,
    Window,
}

/// The header's left side, every string formatted when its inputs change
/// so paint only clones: the switch control's title, the member count,
/// the lens chips (`Δ 1d` lit, `1w`, `1m`; or `6M`, `z · 1Y` lit, `2Y`),
/// the sort chip, the missing members (`1 not in surface`) and the failed
/// datasets.
#[derive(Debug, Clone, PartialEq)]
pub struct HeaderModel {
    pub title: SharedString,
    pub count: usize,
    pub count_text: SharedString,
    /// The three lens chips in paint order, the one in force true: the
    /// lookbacks under moves, the windows under rich/cheap. The lit chip
    /// alone carries its head (`Δ`, or the metric word).
    pub chips: [(String, bool); 3],
    pub chip_kind: ChipKind,
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
    /// count, the lens chips, the sort, the missing, the failed.
    #[cfg(test)]
    pub fn chip_texts(&self) -> Vec<String> {
        let mut out = vec![MARK.to_string(), format!("{} {CHEVRON}", self.title)];
        out.push(self.count_text.to_string());
        out.extend(self.chips.iter().map(|(t, _)| t.clone()));
        out.extend(self.sort.iter().map(ToString::to_string));
        out.extend(self.missing_text.iter().map(ToString::to_string));
        out.extend(self.failed.iter().map(ToString::to_string));
        out
    }
}

impl Default for HeaderModel {
    fn default() -> Self {
        header(None, 0, &LensState::default(), None, None)
    }
}

/// The header's text and counts. With no list the title reads `Vol · New…`.
pub fn header(
    list: Option<&str>,
    count: usize,
    lens: &LensState,
    sort: Option<&str>,
    board: Option<&VolBoard>,
) -> HeaderModel {
    let (chips, chip_kind) = match lens.lens {
        Lens::Moves => (
            Lookback::ALL.map(|l| {
                let lit = l == lens.lookback;
                let text = if lit {
                    format!("\u{394} {}", l.label())
                } else {
                    l.label().to_string()
                };
                (text, lit)
            }),
            ChipKind::Lookback,
        ),
        Lens::Rich => (
            Window::ALL.map(|w| {
                let lit = w == lens.window;
                let text = if lit {
                    format!("{} \u{b7} {}", lens.metric.label(), w.label())
                } else {
                    w.label().to_string()
                };
                (text, lit)
            }),
            ChipKind::Window,
        ),
    };
    let not_in_surface = board.map_or(0, |b| b.missing.len());
    HeaderModel {
        title: format!("{TITLE} \u{b7} {}", list.unwrap_or("New\u{2026}")).into(),
        count,
        count_text: count.to_string().into(),
        chips,
        chip_kind,
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
    use chrono::TimeZone as _;
    use geode_core::volboard::{Cell, Change, Stats};
    use geode_core::volviews::VolView;

    use super::*;
    use crate::core::lens::{Lens, Metric, Shown, Window, Z_CEILING};
    use crate::core::views::{ALL_TENORS, ask, builtin, columns, order_tenors};

    fn now() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 10, 12, 16, 0, 0).unwrap()
    }

    fn tenors() -> Vec<String> {
        order_tenors(None)
    }

    fn vol() -> VolView {
        builtin().get("vol").unwrap().clone()
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

    /// The first column with `id` across the builtin views under the
    /// default lens.
    fn col(id: &str) -> Column {
        builtin()
            .views
            .iter()
            .flat_map(|v| columns(v, &LensState::default(), true))
            .find(|c| c.id == id)
            .expect("a column with that id")
    }

    fn column_index(view: &VolView, lens: &LensState, id: &str) -> usize {
        columns(view, lens, true)
            .iter()
            .position(|c| c.id == id)
            .expect("the column in the view inputs laid out")
    }

    fn inputs<'r>(rows: &'r [Row], view: &VolView, lens: LensState) -> Inputs<'r> {
        Inputs {
            tile: 7,
            rows,
            columns: Vec::leak(columns(view, &lens, true)),
            asked: Box::leak(Box::new(ask(view, &lens, &tenors(), false))),
            board_lookback: Lookback::D1,
            board_window: Some(lens.window.duration()),
            lens,
            cells: view.cells,
            paint: true,
            stale_after: Duration::from_secs(3 * 86_400),
            now: now(),
            mono_advance_px: 7.0,
        }
    }

    /// What a fixture puts in one cell: current, change, pctl, stats.
    type CellData = (
        Option<f64>,
        Option<Change>,
        Option<(f64, usize)>,
        Option<Stats>,
    );

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
                    let (current, change, pctl, stats) = set(m).unwrap_or((None, None, None, None));
                    Cell {
                        measure: m.clone(),
                        current,
                        current_at: current.map(|_| now() - chrono::Duration::hours(1)),
                        changes: vec![change],
                        pctl,
                        stats,
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
    /// 251; NEW missing. Answered for the vol tab's ask.
    fn board_two() -> VolBoard {
        let asked = ask(&vol(), &LensState::default(), &tenors(), false);
        let atfm3m = Measure::atfm("3M");
        VolBoard {
            rows: vec![
                row_of(&asked, "SX5E", true, &|m| {
                    (*m == atfm3m)
                        .then(|| (Some(15.9), Some(change(1.1, 14.8)), Some((0.91, 251)), None))
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

    /// One member per change, ATFM 3M moved by it (the vol tab's ask).
    fn rows_with_changes(changes: &[f64]) -> &'static [Row] {
        let asked = ask(&vol(), &LensState::default(), &tenors(), false);
        let atfm3m = Measure::atfm("3M");
        rows_from(VolBoard {
            rows: changes
                .iter()
                .enumerate()
                .map(|(i, v)| {
                    row_of(&asked, &format!("U{i}"), true, &|m| {
                        (*m == atfm3m).then(|| (Some(15.0), Some(change(*v, 15.0 - v)), None, None))
                    })
                })
                .collect(),
            ..VolBoard::default()
        })
    }

    /// One member whose ATFM 3M ranks `rank` of `count` (the vol tab's ask).
    fn rows_with_pctl(rank: f64, count: usize) -> &'static [Row] {
        let asked = ask(&vol(), &LensState::default(), &tenors(), false);
        let atfm3m = Measure::atfm("3M");
        rows_from(VolBoard {
            rows: vec![row_of(&asked, "SX5E", true, &|m| {
                (*m == atfm3m).then_some((Some(15.0), None, Some((rank, count)), None))
            })],
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
        let p = prepare(&inputs(&rows, &vol(), LensState::default()));
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
        let vol = vol();
        let lens = LensState::default();
        let p = prepare(&inputs(rows_with_changes(&[0.04, -2.0]), &vol, lens));
        let col = column_index(&vol, &lens, "atfm-3M-companion");
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
    fn the_percentile_chip_takes_the_rank_at_display_precision() {
        // 0.8951 shows `90`: the chip sits at the displayed rank's side of
        // the median (0.80), not the raw rank's (0.79).
        let vol = vol();
        let pctl = LensState {
            lens: Lens::Rich,
            metric: Metric::Pctl,
            ..LensState::default()
        };
        let p = prepare(&inputs(rows_with_pctl(0.8951, 251), &vol, pctl));
        let comp = &p.rows[0].cells[column_index(&vol, &pctl, "atfm-3M-companion")];
        assert_eq!(comp.text.as_ref(), "90");
        let Glyph::Chip(Some(chip)) = comp.glyph else {
            panic!("{:?}", comp.glyph)
        };
        assert!(
            (chip.alpha - 0.80 * CHIP_CEILING).abs() < 1e-4,
            "the tint follows the displayed rank: {}",
            chip.alpha
        );
    }

    #[test]
    fn the_chip_ceiling_is_a_tint_not_a_fill() {
        // The column's largest move takes the ceiling itself.
        let vol = vol();
        let lens = LensState::default();
        let p = prepare(&inputs(rows_with_changes(&[-2.0, 0.5]), &vol, lens));
        let col = column_index(&vol, &lens, "atfm-3M-companion");
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
        let vol = vol();
        let lens = LensState::default();
        let p = prepare(&inputs(rows_with_changes(&[1.1]), &vol, lens));
        assert_eq!(
            p.rows[0].cells[column_index(&vol, &lens, "atfm-3M-companion")]
                .text
                .as_ref(),
            "+1.1"
        );
    }

    #[test]
    fn a_pair_cell_is_a_level_and_a_companion_that_follows_the_lens() {
        let vol = builtin().get("vol").unwrap().clone();
        let lens = LensState::default();
        let asked = ask(&vol, &lens, &tenors(), false);
        let rows = vec![Row {
            name: "SX5E".into(),
            reference: None,
            present: true,
            as_of: Some(now()),
            board: Some(row_of(&asked, "SX5E", true, &|m| {
                (*m == Measure::atfm("3M")).then_some((
                    Some(19.6),
                    Some(Change {
                        value: 0.8,
                        from: 18.8,
                        at: now(),
                    }),
                    Some((0.91, 251)),
                    Some(Stats {
                        mean: 16.9,
                        sd: 1.9,
                        count: 251,
                    }),
                ))
            })),
            vega: None,
            path: Vec::new(),
            group: None,
        }];
        let p = prepare(&inputs(&rows, &vol, lens));
        let level = &p.rows[0].cells[column_index(&vol, &lens, "atfm-3M-level")];
        let comp = &p.rows[0].cells[column_index(&vol, &lens, "atfm-3M-companion")];
        assert_eq!(level.text.as_ref(), "19.6");
        assert_eq!(level.glyph, Glyph::None);
        assert_eq!(comp.text.as_ref(), "+0.8");
        assert!(matches!(comp.glyph, Glyph::Chip(Some(_))));
        let rich = LensState {
            lens: Lens::Rich,
            ..lens
        };
        let p = prepare(&inputs(&rows, &vol, rich));
        let comp = &p.rows[0].cells[column_index(&vol, &rich, "atfm-3M-companion")];
        assert_eq!(comp.text.as_ref(), "+1.4");
        let Glyph::Chip(Some(chip)) = comp.glyph else {
            panic!("{:?}", comp.glyph)
        };
        assert!(
            (chip.alpha - (1.4_f64 / Z_CEILING) as f32 * CHIP_CEILING).abs() < 1e-3,
            "z scales against Z_CEILING, not the column"
        );
        let pctl = LensState {
            lens: Lens::Rich,
            metric: Metric::Pctl,
            ..lens
        };
        let p = prepare(&inputs(&rows, &vol, pctl));
        let comp = &p.rows[0].cells[column_index(&vol, &pctl, "atfm-3M-companion")];
        assert_eq!(comp.text.as_ref(), "91", "a rank has no sign");
        let Glyph::Chip(Some(chip)) = comp.glyph else {
            panic!()
        };
        assert_eq!(chip.sign, Sign::Positive, "above the median");
        assert!((chip.alpha - 0.82 * CHIP_CEILING).abs() < 1e-3);
    }

    #[test]
    fn a_single_cell_shows_the_level_over_the_tint_and_m_swaps_in_the_companion() {
        let surface = builtin().get("surface").unwrap().clone();
        let lens = LensState::default();
        let asked = ask(&surface, &lens, &tenors(), false);
        let m3 = Measure::atfm("3M");
        let rows = vec![Row {
            name: "SX5E".into(),
            reference: None,
            present: true,
            as_of: Some(now()),
            board: Some(row_of(&asked, "SX5E", true, &|m| {
                (*m == m3).then_some((
                    Some(19.6),
                    Some(Change {
                        value: -0.9,
                        from: 20.5,
                        at: now(),
                    }),
                    None,
                    None,
                ))
            })),
            vega: None,
            path: Vec::new(),
            group: None,
        }];
        let p = prepare(&inputs(&rows, &surface, lens));
        let cell = &p.rows[0].cells[column_index(&surface, &lens, "atfm-3M-single")];
        assert_eq!(cell.text.as_ref(), "19.6");
        let Glyph::Tint(Some(chip)) = cell.glyph else {
            panic!("{:?}", cell.glyph)
        };
        assert_eq!(chip.sign, Sign::Negative);
        assert_eq!(
            cell.sign,
            Sign::Positive,
            "the text's own sign is the level's"
        );
        let swapped = LensState {
            shown: Shown::Companion,
            ..lens
        };
        let p = prepare(&inputs(&rows, &surface, swapped));
        let cell = &p.rows[0].cells[column_index(&surface, &swapped, "atfm-3M-single")];
        assert_eq!(cell.text.as_ref(), "\u{2212}0.9".replace('\u{2212}', "-"));
        assert!(matches!(cell.glyph, Glyph::Chip(Some(_))));
        // A level with no companion keeps its number and takes no tint.
        let bare = vec![Row {
            board: Some(row_of(&asked, "SX5E", true, &|m| {
                (*m == m3).then_some((Some(19.6), None, None, None))
            })),
            ..rows[0].clone()
        }];
        let p = prepare(&inputs(&bare, &surface, lens));
        let cell = &p.rows[0].cells[column_index(&surface, &lens, "atfm-3M-single")];
        assert_eq!(cell.text.as_ref(), "19.6");
        assert_eq!(cell.glyph, Glyph::Tint(None));
    }

    #[test]
    fn a_tint_from_another_lookback_goes_while_the_level_stays() {
        // The board holds W1; the lens shows D1 until it re-answers. The
        // pair's companion greys; the single's level keeps its number and
        // tone but takes no tint from the wrong lookback.
        let surface = builtin().get("surface").unwrap().clone();
        let vol = vol();
        let lens = LensState::default();
        let m3 = Measure::atfm("3M");
        let set =
            |m: &Measure| (*m == m3).then_some((Some(19.6), Some(change(-0.9, 20.5)), None, None));
        let single = vec![Row {
            name: "SX5E".into(),
            reference: None,
            present: true,
            as_of: Some(now()),
            board: Some(row_of(
                &ask(&surface, &lens, &tenors(), false),
                "SX5E",
                true,
                &set,
            )),
            vega: None,
            path: Vec::new(),
            group: None,
        }];
        let mut i = inputs(&single, &surface, lens);
        i.board_lookback = Lookback::W1;
        let p = prepare(&i);
        let cell = &p.rows[0].cells[column_index(&surface, &lens, "atfm-3M-single")];
        assert_eq!(cell.text.as_ref(), "19.6");
        assert!(!cell.muted, "the level keeps its tone");
        assert_eq!(cell.glyph, Glyph::Tint(None));
        let pair = vec![Row {
            board: Some(row_of(
                &ask(&vol, &lens, &tenors(), false),
                "SX5E",
                true,
                &set,
            )),
            ..single[0].clone()
        }];
        let mut i = inputs(&pair, &vol, lens);
        i.board_lookback = Lookback::W1;
        let p = prepare(&i);
        assert!(p.rows[0].cells[column_index(&vol, &lens, "atfm-3M-companion")].muted);
    }

    #[test]
    fn a_tint_over_a_short_history_goes_while_the_level_stays() {
        let surface = builtin().get("surface").unwrap().clone();
        let pctl = LensState {
            lens: Lens::Rich,
            metric: Metric::Pctl,
            ..LensState::default()
        };
        let asked = ask(&surface, &pctl, &tenors(), false);
        let m3 = Measure::atfm("3M");
        let rows = vec![Row {
            name: "SX5E".into(),
            reference: None,
            present: true,
            as_of: Some(now()),
            board: Some(row_of(&asked, "SX5E", true, &|m| {
                (*m == m3).then_some((Some(19.6), None, Some((0.9, 12)), None))
            })),
            vega: None,
            path: Vec::new(),
            group: None,
        }];
        let p = prepare(&inputs(&rows, &surface, pctl));
        let cell = &p.rows[0].cells[column_index(&surface, &pctl, "atfm-3M-single")];
        assert_eq!(cell.text.as_ref(), "19.6");
        assert!(!cell.muted);
        assert_eq!(
            cell.glyph,
            Glyph::Tint(None),
            "no tint under 60 observations"
        );
    }

    #[test]
    fn a_short_count_mutes_the_rank_and_blanks_the_z() {
        let vol = builtin().get("vol").unwrap().clone();
        let rich = LensState {
            lens: Lens::Rich,
            ..LensState::default()
        };
        let asked = ask(&vol, &rich, &tenors(), false);
        let rows = vec![Row {
            name: "SX5E".into(),
            reference: None,
            present: true,
            as_of: Some(now()),
            board: Some(row_of(&asked, "SX5E", true, &|m| {
                (*m == Measure::atfm("3M")).then_some((
                    Some(19.6),
                    None,
                    Some((0.9, 12)),
                    Some(Stats {
                        mean: 16.9,
                        sd: 1.9,
                        count: 12,
                    }),
                ))
            })),
            vega: None,
            path: Vec::new(),
            group: None,
        }];
        let p = prepare(&inputs(&rows, &vol, rich));
        let comp = &p.rows[0].cells[column_index(&vol, &rich, "atfm-3M-companion")];
        assert_eq!(
            comp.text.as_ref(),
            widths::DASH,
            "no z under 60 observations"
        );
        let pctl = LensState {
            metric: Metric::Pctl,
            ..rich
        };
        let p = prepare(&inputs(&rows, &vol, pctl));
        let comp = &p.rows[0].cells[column_index(&vol, &pctl, "atfm-3M-companion")];
        assert_eq!(comp.text.as_ref(), "90");
        assert!(comp.muted);
    }

    #[test]
    fn a_short_count_names_itself_in_the_readout() {
        let vol = vol();
        let rows = rows_with_pctl(0.5, 12);
        let asked = ask(&vol, &LensState::default(), &tenors(), false);
        let mut rows = rows.to_vec();
        let board = rows[0].board.as_mut().unwrap();
        let pos = asked
            .measures
            .iter()
            .position(|m| *m == Measure::atfm("3M"))
            .unwrap();
        board.cells[pos].stats = Some(Stats {
            mean: 16.9,
            sd: 1.9,
            count: 12,
        });
        assert_eq!(
            readout(
                &rows[0],
                &col("atfm-3M-companion"),
                &asked,
                &[],
                &clock_utc(),
                &LensState::default()
            ),
            "SX5E · ATFM 3M 15.0 · Δ1d — · z 1Y — (12 of 60) · pctl 1Y 50 of 12 · mean 16.9 · sd 1.9"
        );
    }

    #[test]
    fn a_change_from_another_lookback_than_the_one_shown_paints_muted() {
        let vol = vol();
        let lens = LensState::default();
        let mut i = inputs(rows_with_changes(&[1.0]), &vol, lens);
        assert!(!prepare(&i).rows[0].cells[column_index(&vol, &lens, "atfm-3M-companion")].muted);
        i.lens.lookback = Lookback::W1; // the board holds D1
        let p = prepare(&i);
        assert!(p.rows[0].cells[column_index(&vol, &lens, "atfm-3M-companion")].muted);
        assert!(
            !p.rows[0].cells[column_index(&vol, &lens, "atfm-3M-level")].muted,
            "a level has no lookback"
        );
    }

    #[test]
    fn a_companion_from_another_window_than_the_one_shown_paints_muted() {
        // The board holds 1Y; the lens shows 2Y until it re-answers. The
        // pair's rank greys; a single view's level keeps its number and tone
        // and drops the tint, since a 1Y rank under a 2Y chip misleads.
        let vol = vol();
        let rich = LensState {
            lens: Lens::Rich,
            metric: Metric::Pctl,
            ..LensState::default()
        };
        let mut i = inputs(rows_with_pctl(0.91, 251), &vol, rich);
        i.board_window = Some(Window::Y1.duration());
        assert!(!prepare(&i).rows[0].cells[column_index(&vol, &rich, "atfm-3M-companion")].muted);
        i.lens.window = Window::Y2; // the board holds 1Y
        let p = prepare(&i);
        assert!(p.rows[0].cells[column_index(&vol, &rich, "atfm-3M-companion")].muted);
        assert!(
            !p.rows[0].cells[column_index(&vol, &rich, "atfm-3M-level")].muted,
            "a level has no window"
        );
        let surface = builtin().get("surface").unwrap().clone();
        let asked = ask(&surface, &rich, &tenors(), false);
        let m3 = Measure::atfm("3M");
        let rows = vec![Row {
            name: "SX5E".into(),
            reference: None,
            present: true,
            as_of: Some(now()),
            board: Some(row_of(&asked, "SX5E", true, &|m| {
                (*m == m3).then_some((Some(19.6), None, Some((0.91, 251)), None))
            })),
            vega: None,
            path: Vec::new(),
            group: None,
        }];
        let mut i = inputs(&rows, &surface, rich);
        i.board_window = Some(Window::Y1.duration());
        let single = column_index(&surface, &rich, "atfm-3M-single");
        assert!(
            matches!(
                prepare(&i).rows[0].cells[single].glyph,
                Glyph::Tint(Some(_))
            ),
            "the rank tints the level while the windows agree"
        );
        i.lens.window = Window::Y2;
        let cell = &prepare(&i).rows[0].cells[single];
        assert_eq!(cell.text.as_ref(), "19.6");
        assert!(!cell.muted, "the level keeps its tone");
        assert_eq!(cell.glyph, Glyph::Tint(None));
    }

    #[test]
    fn a_stale_row_keeps_its_cells_tone_and_mutes_only_its_flag() {
        let vol = vol();
        let lens = LensState::default();
        let mut i = inputs(rows_with_changes(&[1.0]), &vol, lens);
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
        assert!(!p.rows[0].cells[column_index(&vol, &lens, "atfm-3M-level")].muted);
        assert!(!p.rows[0].cells[column_index(&vol, &lens, "atfm-3M-companion")].muted);
        assert!(!p.rows[0].cells[0].muted, "the model leaves the name too");
    }

    /// A surface publishes once a day: last evening's row is fresh this
    /// afternoon, and only a row a day old is stale.
    #[test]
    fn the_default_stale_threshold_is_a_day() {
        let day = crate::content::DEFAULT_STALE_AFTER;
        let aged = |hours: i64| -> &'static [Row] {
            let asked = ask(&vol(), &LensState::default(), &tenors(), false);
            let atfm3m = Measure::atfm("3M");
            let mut row = row_of(&asked, "U0", true, &|m| {
                (*m == atfm3m).then(|| (Some(15.0), Some(change(1.0, 14.0)), None, None))
            });
            row.as_of = Some(now() - chrono::Duration::hours(hours));
            rows_from(VolBoard {
                rows: vec![row],
                ..VolBoard::default()
            })
        };
        let mut i = inputs(aged(16), &vol(), LensState::default());
        i.stale_after = day;
        assert!(
            !prepare(&i).rows[0].stale,
            "yesterday's 17:00 publish is fresh at 09:00"
        );
        let mut i = inputs(aged(25), &vol(), LensState::default());
        i.stale_after = day;
        assert!(
            prepare(&i).rows[0].stale,
            "a day without a publish is stale"
        );
    }

    #[test]
    fn paint_off_keeps_the_numbers_and_drops_the_chips_and_tints() {
        let vol = vol();
        let pctl = LensState {
            lens: Lens::Rich,
            metric: Metric::Pctl,
            ..LensState::default()
        };
        let mut i = inputs(rows_with_pctl(0.9, 251), &vol, pctl);
        i.paint = false;
        let p = prepare(&i);
        assert!(
            p.rows[0]
                .cells
                .iter()
                .all(|c| matches!(c.glyph, Glyph::None | Glyph::Spark { .. }))
        );
        assert_eq!(
            p.rows[0].cells[column_index(&vol, &pctl, "atfm-3M-companion")]
                .text
                .as_ref(),
            "90"
        );
        // A single view's tint goes the same way.
        let surface = builtin().get("surface").unwrap().clone();
        let lens = LensState::default();
        let asked = ask(&surface, &lens, &tenors(), false);
        let m3 = Measure::atfm("3M");
        let rows = vec![Row {
            name: "SX5E".into(),
            reference: None,
            present: true,
            as_of: Some(now()),
            board: Some(row_of(&asked, "SX5E", true, &|m| {
                (*m == m3).then_some((Some(19.6), Some(change(-0.9, 20.5)), None, None))
            })),
            vega: None,
            path: Vec::new(),
            group: None,
        }];
        let mut i = inputs(&rows, &surface, lens);
        i.paint = false;
        let p = prepare(&i);
        let cell = &p.rows[0].cells[column_index(&surface, &lens, "atfm-3M-single")];
        assert_eq!(cell.text.as_ref(), "19.6");
        assert_eq!(cell.glyph, Glyph::None);
    }

    #[test]
    fn widths_come_from_the_glyph_minimums_and_the_widest_text() {
        let vol = vol();
        let lens = LensState::default();
        let p = prepare(&inputs(rows_with_changes(&[1.0]), &vol, lens));
        let comp = column_index(&vol, &lens, "atfm-3M-companion");
        assert!(
            p.widths[comp] >= widths::chip(4.0 * 7.0),
            "`+1.0` at 7 px advance inside the chip's insets, or the chip's minimum"
        );
        assert!(p.widths[column_index(&vol, &lens, "spark-atfm-3M")] >= sizes::WIDTH_30D);
        // A level column is as wide as its widest text or its head with
        // the sort mark, whichever is wider.
        assert_eq!(
            p.widths[column_index(&vol, &lens, "atfm-3M-level")],
            (3.0 * 7.0 + SORT_MARK_DESIGN_PX).max(4.0 * 7.0),
            "`vol` plus the sort mark, wider than `15.0`"
        );
    }

    #[test]
    fn a_sortable_head_reserves_the_sort_mark() {
        let vol = vol();
        let lens = LensState::default();
        let p = prepare(&inputs(rows_with_changes(&[1.0]), &vol, lens));
        let level = column_index(&vol, &lens, "atfm-1M-level");
        let head_px = "vol".chars().count() as f32 * 7.0;
        assert!(p.widths[level] >= head_px + SORT_MARK_DESIGN_PX);
        // A spark column is not sortable: as wide as its label, no mark.
        let spark = column_index(&vol, &lens, "spark-atfm-3M");
        let label_px = "ATFM 3M \u{b7} 30d".chars().count() as f32 * 7.0;
        assert!(p.widths[spark] >= label_px);
        assert!(p.widths[spark] < label_px + SORT_MARK_DESIGN_PX);
    }

    #[test]
    fn a_group_label_wider_than_its_columns_is_shared_across_them() {
        // Skew: "25Δ skew 3M" over `pts` and `Δ`, both blank in this board.
        let skew = builtin().get("skew").unwrap().clone();
        let lens = LensState::default();
        let rows = rows_from(VolBoard {
            rows: vec![row_of(
                &ask(&skew, &lens, &tenors(), false),
                "SX5E",
                true,
                &|_| None,
            )],
            ..VolBoard::default()
        });
        let p = prepare(&inputs(rows, &skew, lens));
        let (a, b) = (
            column_index(&skew, &lens, "skew-3M-level"),
            column_index(&skew, &lens, "skew-3M-companion"),
        );
        let label_px = "25\u{394} skew 3M".chars().count() as f32 * 7.0;
        assert!(p.widths[a] + p.widths[b] >= label_px);
    }

    #[test]
    fn a_long_board_sparkline_is_cut_to_the_columns_span() {
        let vol = vol();
        let lens = LensState::default();
        let mut asked = ask(&vol, &lens, &tenors(), false);
        asked.sparklines = vec![(Measure::atfm("3M"), Window::Y1.duration())];
        let mut board = board_two();
        board.rows[0].sparks = vec![Some((0..365).map(f64::from).collect())];
        let rows = rows(&members(["SX5E"]), Some(&board), &no_reference, None);
        let mut i = inputs(&rows, &vol, lens);
        i.asked = &asked;
        let p = prepare(&i);
        let Glyph::Spark { values, .. } =
            &p.rows[0].cells[column_index(&vol, &lens, "spark-atfm-3M")].glyph
        else {
            panic!("a spark");
        };
        assert_eq!(values.len(), 30);
        assert_eq!(values[0], 335.0, "the last thirty points");
        assert_eq!(values[29], 364.0);
    }

    #[test]
    fn a_spark_column_reads_its_measures_series_wherever_the_ask_lists_it() {
        // A band-open ask lists skew 3M first: the vol tab's spark still shows ATFM 3M.
        let vol = vol();
        let lens = LensState::default();
        let skew = Measure::Skew { tenor: "3M".into() };
        let mut asked = ask(&vol, &lens, &tenors(), false);
        asked.sparklines = vec![
            (skew, Window::Y1.duration()),
            (Measure::atfm("3M"), Window::Y1.duration()),
        ];
        let mut board = board_two();
        board.rows[0].sparks = vec![
            Some((0..365).map(|v| -f64::from(v)).collect()),
            Some((0..365).map(f64::from).collect()),
        ];
        let rows = rows(&members(["SX5E"]), Some(&board), &no_reference, None);
        let mut i = inputs(&rows, &vol, lens);
        i.asked = &asked;
        let p = prepare(&i);
        let Glyph::Spark { values, .. } =
            &p.rows[0].cells[column_index(&vol, &lens, "spark-atfm-3M")].glyph
        else {
            panic!("a spark");
        };
        assert_eq!(values.len(), 30);
        assert_eq!(values[29], 364.0, "the ATFM series' tail, not the skew's");
    }

    #[test]
    fn a_measure_the_board_was_not_asked_for_is_blank() {
        // A skew column read against the vol tab's board: skew was not asked.
        let skew = builtin().get("skew").unwrap().clone();
        let vol = vol();
        let lens = LensState::default();
        let rows = rows(&members(["SX5E"]), Some(&board_two()), &no_reference, None);
        let mut i = inputs(&rows, &skew, lens);
        let asked = ask(&vol, &lens, &tenors(), false);
        i.asked = &asked;
        let p = prepare(&i);
        let c = &p.rows[0].cells[column_index(&skew, &lens, "skew-3M-level")];
        assert_eq!(c.text.as_ref(), widths::DASH);
        assert!(c.muted);
        let p = prepare(&inputs(&rows, &vol, lens));
        assert_eq!(
            p.rows[0].cells[column_index(&vol, &lens, "atfm-3M-level")]
                .text
                .as_ref(),
            "15.9"
        );
    }

    #[test]
    fn vega_is_in_thousands() {
        let vol = vol();
        let lens = LensState::default();
        let vega = HashMap::from([("SX5E".to_string(), -312_400.0)]);
        let rows = rows(
            &members(["SX5E"]),
            Some(&board_two()),
            &no_reference,
            Some(&vega),
        );
        let p = prepare(&inputs(&rows, &vol, lens));
        let vega = column_index(&vol, &lens, "vega");
        assert_eq!(p.rows[0].cells[vega].text.as_ref(), "-312");
        assert_eq!(col("vega").head, "vega k");
        assert_eq!(
            readout(
                &rows[0],
                &col("vega"),
                &ask(&vol, &lens, &tenors(), false),
                &[],
                &clock_utc(),
                &lens
            ),
            "SX5E · vega k -312"
        );
        assert_eq!(p.rows[0].cells[vega].id.as_ref(), "7-SX5E-vega");
    }

    #[test]
    fn the_readout_names_the_cell_its_change_and_its_rank() {
        let asked = ask(&vol(), &LensState::default(), &tenors(), false);
        let rows = rows(
            &members(["SX5E", "NEW"]),
            Some(&board_two()),
            &no_reference,
            None,
        );
        let text = readout(
            &rows[0],
            &col("atfm-3M-level"),
            &asked,
            &[],
            &clock_utc(),
            &LensState::default(),
        );
        assert_eq!(
            text, "SX5E · ATFM 3M 15.9 · Δ1d +1.1 from 14.8 on 2026-10-09 · pctl 1Y 91 of 251",
            "no stats, no z and no mean"
        );
        let text = readout(
            &rows[1],
            &col("atfm-3M-level"),
            &asked,
            &[],
            &clock_utc(),
            &LensState::default(),
        );
        assert_eq!(text, "NEW · not in surface");
    }

    #[test]
    fn the_readout_carries_the_change_the_z_the_rank_and_the_stats() {
        let vol = builtin().get("vol").unwrap().clone();
        let lens = LensState::default();
        let asked = ask(&vol, &lens, &tenors(), false);
        let at = chrono::Utc
            .with_ymd_and_hms(2026, 10, 22, 21, 0, 0)
            .unwrap();
        let row = Row {
            name: "SX5E".into(),
            reference: None,
            present: true,
            as_of: Some(now()),
            board: Some(row_of(&asked, "SX5E", true, &|m| {
                (*m == Measure::atfm("3M")).then_some((
                    Some(19.6),
                    Some(Change {
                        value: 0.8,
                        from: 18.8,
                        at,
                    }),
                    Some((0.91, 251)),
                    Some(Stats {
                        mean: 16.9,
                        sd: 1.9,
                        count: 251,
                    }),
                ))
            })),
            vega: None,
            path: Vec::new(),
            group: None,
        };
        let col = columns(&vol, &lens, true)
            .into_iter()
            .find(|c| c.id == "atfm-3M-companion")
            .unwrap();
        assert_eq!(
            readout(&row, &col, &asked, &[], &clock_utc(), &lens),
            "SX5E \u{b7} ATFM 3M 19.6 \u{b7} \u{394}1d +0.8 from 18.8 on 2026-10-22 \u{b7} z 1Y +1.4 \u{b7} pctl 1Y 91 of 251 \u{b7} mean 16.9 \u{b7} sd 1.9"
        );
    }

    #[test]
    fn a_cell_without_a_change_names_where_its_history_starts() {
        let asked = ask(&vol(), &LensState::default(), &tenors(), false);
        let atfm3m = Measure::atfm("3M");
        let board = VolBoard {
            rows: vec![row_of(&asked, "SX5E", true, &|m| {
                (*m == atfm3m).then_some((Some(15.9), None, None, None))
            })],
            notes: vec![
                "realized_vol: history starts 2026-08-01".into(),
                "vol_surface: history starts 2026-09-01".into(),
            ],
            ..VolBoard::default()
        };
        let rows = rows(&members(["SX5E"]), Some(&board), &no_reference, None);
        let lens = LensState::default();
        for id in ["atfm-3M-level", "atfm-3M-companion"] {
            assert_eq!(
                readout(
                    &rows[0],
                    &col(id),
                    &asked,
                    &board.notes,
                    &clock_utc(),
                    &lens
                ),
                "SX5E · ATFM 3M 15.9 · Δ1d — · history starts 2026-09-01",
                "{id}"
            );
        }
        assert_eq!(
            readout(
                &rows[0],
                &col("atfm-3M-companion"),
                &asked,
                &[],
                &clock_utc(),
                &lens
            ),
            "SX5E · ATFM 3M 15.9 · Δ1d —",
            "no note, no reason"
        );
    }

    #[test]
    fn sort_values_put_blanks_last_and_follow_the_lens() {
        let asked = ask(&vol(), &LensState::default(), &tenors(), false);
        let rows = rows(
            &members(["NEW", "SX5E"]),
            Some(&board_two()),
            &no_reference,
            None,
        );
        let lens = LensState::default();
        assert_eq!(
            sort_value(&rows[1], &col("atfm-3M-level"), &asked, &lens),
            None
        );
        assert_eq!(
            sort_value(&rows[0], &col("atfm-3M-companion"), &asked, &lens),
            Some(1.1),
            "the move under moves"
        );
        let pctl = LensState {
            lens: Lens::Rich,
            metric: Metric::Pctl,
            ..lens
        };
        assert_eq!(
            sort_value(&rows[0], &col("atfm-3M-companion"), &asked, &pctl),
            Some(0.91),
            "the rank under rich/cheap · pctl"
        );
    }

    #[test]
    fn the_header_chips_are_the_lookbacks_under_moves_and_the_windows_under_rich() {
        let moves = header(Some("europe_risk"), 3, &LensState::default(), None, None);
        assert_eq!(moves.chip_kind, ChipKind::Lookback);
        assert_eq!(
            moves.chips,
            [
                ("\u{394} 1d".to_string(), true),
                ("1w".into(), false),
                ("1m".into(), false)
            ]
        );
        let rich = header(
            Some("europe_risk"),
            3,
            &LensState {
                lens: Lens::Rich,
                window: Window::Y2,
                ..LensState::default()
            },
            None,
            None,
        );
        assert_eq!(rich.chip_kind, ChipKind::Window);
        assert_eq!(
            rich.chips,
            [
                ("6M".to_string(), false),
                ("1Y".into(), false),
                ("z \u{b7} 2Y".into(), true)
            ]
        );
    }

    /// SX5E answered for the vol tab's ask over `listed` tenors, band open
    /// or not (the board omits 2Y either way):
    /// every vol point at 10 + its tenor index + 10 × its delta (ATFM 3M
    /// at 16.0), skew 3M 5.5 moved +0.4,
    /// ATFM 3M ranked 0.91 of 251, IV−RV 3.4 ranked 0.77, varswap 18.1,
    /// vs spread 2.2; sparks rise from 15.0 to 17.9.
    fn band_row(listed: &[String], band: bool) -> (Ask, Vec<Row>) {
        let mut asked = ask(&vol(), &LensState::default(), listed, band);
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
                    Some((Some(16.0), None, Some((0.91, 251)), None))
                }
                Measure::Point { tenor, delta } => {
                    let at = listed.iter().position(|t| t == tenor).unwrap();
                    Some((Some(10.0 + at as f64 + 10.0 * delta), None, None, None))
                }
                Measure::Skew { tenor } if tenor == "3M" => {
                    Some((Some(5.5), Some(change(0.4, 5.1)), Some((0.64, 251)), None))
                }
                m if *m == ivrv => Some((Some(3.4), None, Some((0.77, 251)), None)),
                Measure::Varswap { .. } => Some((Some(18.1), None, None, None)),
                Measure::VsSpread { .. } => Some((Some(2.2), None, None, None)),
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
        let mut i = inputs(&rows, &vol(), LensState::default());
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
        let three_m = &b.surface[3].1;
        assert_eq!(three_m[2].cell.text.as_ref(), "16.0", "atm is ATFM");
        // 3M is tenor 3: each column reads its own call delta's point.
        assert_eq!(three_m[0].cell.text.as_ref(), "22.0", "10p reads 0.90");
        assert_eq!(three_m[1].cell.text.as_ref(), "20.5", "25p reads 0.75");
        assert_eq!(three_m[3].cell.text.as_ref(), "15.5", "25c reads 0.25");
        assert_eq!(three_m[4].cell.text.as_ref(), "14.0", "10c reads 0.10");
        assert_eq!(three_m[5].cell.text.as_ref(), "5.5", "25Δ skew");
        assert_eq!(
            three_m[6].cell.text.as_ref(),
            "+0.4",
            "its Δ over the lookback"
        );
        assert_eq!(three_m[6].cell.id.as_ref(), "7-band-3M-delta");
        assert_eq!(b.delta_head.as_ref(), "Δ1d");
        let two_y = &b.surface[7].1;
        assert!(
            two_y
                .iter()
                .all(|c| c.cell.text.as_ref() == widths::DASH && c.cell.muted),
            "a tenor the board lacks is dashes"
        );
        // One heat range over the whole table: 1W 10c's 11.0 is its floor
        // and 5Y 10p's 28.0 its ceiling; the skew and its Δ take none.
        assert_eq!(b.surface[0].1[4].heat, Some(0.0));
        assert!(matches!(b.surface[9].1[0].heat, Some(a) if (a - BAND_HEAT_CEILING).abs() < 1e-6));
        assert_eq!(three_m[5].heat, None);
        assert_eq!(three_m[6].heat, None);
        assert!(matches!(three_m[6].cell.glyph, Glyph::Chip(Some(_))));
        assert!(b.sparks.iter().all(Option::is_some));
        assert_eq!(
            b.captions[0].as_ref(),
            "pctl 1Y 91 of 251 · range 15.0 – 17.9"
        );
        assert_eq!(
            b.captions[1].as_ref(),
            "pctl 1Y 64 of 251 · range 15.0 – 17.9"
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

    #[test]
    fn the_band_names_the_window_the_board_answered() {
        let listed = tenors();
        let lens = LensState {
            window: Window::Y2,
            ..LensState::default()
        };
        let mut asked = ask(&vol(), &lens, &listed, true);
        let (_, rows) = band_row(&listed, true);
        asked
            .measures
            .retain(|m| !matches!(m, Measure::Point { tenor, .. } if tenor == "2Y"));
        let mut i = inputs(&rows, &vol(), lens);
        i.asked = &asked;
        let b = prepare_band(&i, &rows[0], &listed, &clock_utc());
        assert!(
            b.captions[0].starts_with("pctl 2Y 91 of 251"),
            "{}",
            b.captions[0]
        );
    }

    /// The band follows the grid: a stale row's surface, skew, delta chip
    /// and sparks keep their tone and its name carries the staleness; only
    /// a lookback mismatch mutes the chip.
    #[test]
    fn a_stale_rows_band_keeps_its_cells_tone() {
        let listed = tenors();
        let (asked, rows) = band_row(&listed, true);
        let mut i = inputs(&rows, &vol(), LensState::default());
        i.asked = &asked;
        i.stale_after = Duration::from_secs(60);
        assert!(prepare(&i).rows[0].stale, "the fixture row is an hour old");
        let b = prepare_band(&i, &rows[0], &listed, &clock_utc());
        let three_m = &b.surface[3].1;
        assert!(three_m[2].heat.is_some());
        assert!(!three_m[2].cell.muted, "a heat tint keeps its tone");
        assert!(!three_m[5].cell.muted, "the skew keeps its tone");
        assert!(matches!(three_m[6].cell.glyph, Glyph::Chip(_)));
        assert!(!three_m[6].cell.muted, "the delta chip keeps its color");
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
        i.lens.lookback = Lookback::W1;
        let b = prepare_band(&i, &rows[0], &listed, &clock_utc());
        assert!(
            b.surface[3].1[6].cell.muted,
            "a chip from another lookback still paints muted"
        );
        assert_eq!(
            b.delta_head.as_ref(),
            "Δ1w",
            "the head names the lookback shown"
        );
        assert!(!b.surface[3].1[2].cell.muted);
    }

    #[test]
    fn the_band_of_a_board_asked_without_it_is_dashes() {
        let listed = tenors();
        // The board answered the closed ask: no point but ATFM, no skew, no
        // window-long spark.
        let (asked, rows) = band_row(&listed, false);
        let mut i = inputs(&rows, &vol(), LensState::default());
        i.asked = &asked;
        let b = prepare_band(&i, &rows[0], &listed, &clock_utc());
        assert_eq!(b.surface[0].1[0].cell.text.as_ref(), widths::DASH, "10p 1M");
        assert_eq!(
            b.surface[0].1[5].cell.text.as_ref(),
            widths::DASH,
            "skew 1M"
        );
        assert!(b.sparks[1].is_none() && b.sparks[2].is_none());
        assert_eq!(
            b.captions[0].as_ref(),
            "pctl 1Y 91 of 251 · range 15.0 – 17.9",
            "the tab's own 30d ATFM spark serves the band"
        );
        assert_eq!(b.captions[1].as_ref(), widths::DASH, "skew was not asked");
    }

    #[test]
    fn the_header_counts_the_missing_and_names_the_failed_datasets() {
        let mut b = board_two();
        b.dataset_errors
            .push(("realized_vol".into(), "drifted".into()));
        let w1 = LensState {
            lookback: Lookback::W1,
            ..LensState::default()
        };
        let h = header(Some("europe_risk"), 2, &w1, Some("|Δ 3M|"), Some(&b));
        assert_eq!(h.title, "Vol · europe_risk");
        assert_eq!(h.not_in_surface, 1);
        assert_eq!(h.failed, ["realized_vol"]);
        assert_eq!(h.chips[1], ("\u{394} 1w".to_string(), true));
        assert_eq!(h.sort.as_deref(), Some("|Δ 3M|"));
        assert_eq!(
            header(None, 0, &LensState::default(), None, None).title,
            "Vol · New…"
        );
        assert_eq!(
            h.chip_texts()[3..6],
            ["1d".to_string(), "\u{394} 1w".into(), "1m".into()]
        );
    }
}
