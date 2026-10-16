//! The inspection band, docked between the grid and the footer while it is
//! open: the cursor row's surface at fixed deltas (heat-tinted over one
//! range for the whole table), its three one-year sparks with their
//! captions, and the context pairs. It is a region under the grid rather
//! than a taller row because the table paints uniform row heights.
//!
//! Every string, glyph core and id comes from the prepared [`BandModel`];
//! every color from [`Paints`] and the theme. Paint clones and lays out.

use geode_chart::spark::{Spark, SparkPaint, sizes};
use geode_shell::fonts;
use geode_widgets::cell::{delta_chip, design, heat};
use gpui::prelude::*;
use gpui::{AnyElement, Div, SharedString, div};
use gpui_component::theme::Theme;
use gpui_component::{h_flex, v_flex};

use super::table::Paints;
use crate::core::model::{BandModel, Glyph, PreparedCell};

/// The surface table's tenor column, in design px: a four-character
/// tenor label beside the `T \ Δ` corner.
const TENOR_WIDTH: f32 = 40.0;
/// A vol column: `widths::heat` of a four-character level at the grid's
/// nominal 7 px advance (28 + the tint's 2 × 6 px inset).
const VOL_WIDTH: f32 = 40.0;
/// The skew column, wide enough for its `25Δ skew` head.
const SKEW_WIDTH: f32 = 60.0;
/// The Δ column: the change chip's minimum width plus its inset.
const DELTA_WIDTH: f32 = 48.0;
/// The context block's label column.
const CONTEXT_LABEL_WIDTH: f32 = 72.0;

const SURFACE_TITLE: &str = "Surface at fixed deltas \u{b7} vol";
const CONTEXT_TITLE: &str = "Context";
const CORNER: &str = "T \\ \u{394}";
const HEADS: [&str; 6] = ["10p", "25p", "atm", "25c", "10c", "25\u{394} skew"];
/// The sparks' titles, parallel to `BandModel::sparks`.
const SPARK_TITLES: [&str; 3] = [
    "ATFM 3M \u{b7} 1Y",
    "25\u{394} skew 3M \u{b7} 1Y",
    "RV 20d \u{b7} 1Y",
];

/// The width of surface column `ix` (0 is the tenor).
fn col_width(ix: usize) -> f32 {
    match ix {
        0 => TENOR_WIDTH,
        1..=5 => VOL_WIDTH,
        6 => SKEW_WIDTH,
        _ => DELTA_WIDTH,
    }
}

/// A right-aligned cell of surface column `ix`.
fn slot(ix: usize) -> Div {
    h_flex()
        .flex_none()
        .w(design(col_width(ix)))
        .justify_end()
        .overflow_hidden()
        .whitespace_nowrap()
}

fn section_title(text: &'static str, theme: &Theme) -> Div {
    div()
        .text_color(theme.muted_foreground)
        .child(SharedString::new_static(text))
}

/// One prepared surface cell: its tint or chip, or plain text.
fn cell_el(cell: &PreparedCell, paints: &Paints, theme: &Theme) -> AnyElement {
    let text = cell.text.clone();
    let id = cell.id.clone();
    match &cell.glyph {
        Glyph::Heat(alpha) => {
            let paint = if cell.muted {
                paints.heat_muted
            } else {
                paints.heat
            };
            heat::paint(text, *alpha, paint, id)
                .w_full()
                .into_any_element()
        }
        Glyph::Chip(chip) => {
            let paint = if cell.muted {
                paints.chip_muted
            } else {
                paints.chip
            };
            delta_chip::paint(text, *chip, paint, id).into_any_element()
        }
        _ => div()
            .text_color(if cell.muted {
                theme.muted_foreground
            } else {
                theme.foreground
            })
            .child(text)
            .into_any_element(),
    }
}

/// The fixed-delta surface: a head row, then one row per tenor.
fn surface(band: &BandModel, paints: &Paints, theme: &Theme) -> Div {
    let muted = theme.muted_foreground;
    let head = h_flex()
        .gap_1()
        .text_color(muted)
        .child(
            slot(0)
                .justify_start()
                .child(SharedString::new_static(CORNER)),
        )
        .children(
            HEADS
                .iter()
                .enumerate()
                .map(|(i, h)| slot(i + 1).child(SharedString::new_static(h))),
        )
        .child(slot(7).child(band.delta_head.clone()));
    let rows = band.surface.iter().map(|(tenor, cells)| {
        h_flex()
            .gap_1()
            .child(
                slot(0)
                    .justify_start()
                    .text_color(muted)
                    .child(tenor.clone()),
            )
            .children(
                cells
                    .iter()
                    .enumerate()
                    .map(|(i, c)| slot(i + 1).child(cell_el(c, paints, theme))),
            )
    });
    v_flex()
        .flex_none()
        .gap_0p5()
        .child(section_title(SURFACE_TITLE, theme))
        .child(head)
        .children(rows)
}

/// The three sparks, each titled with its caption beneath. The end dot's
/// ring is the band's own ground, the tile's background.
fn sparks(band: &BandModel, paints: &Paints, theme: &Theme) -> Div {
    let paint = SparkPaint {
        ring: theme.background,
        ..paints.spark
    };
    v_flex().flex_none().gap_2().children(
        band.sparks
            .iter()
            .zip(&band.captions)
            .zip(SPARK_TITLES)
            .map(|((spark, caption), title)| {
                let glyph = spark.as_ref().and_then(|c| match &c.glyph {
                    Glyph::Spark {
                        values,
                        range,
                        mark,
                        end_dot,
                    } => Some(
                        Spark::new(values.clone(), paint)
                            .range(*range)
                            .mark(*mark)
                            .end_dot(*end_dot),
                    ),
                    _ => None,
                });
                v_flex()
                    .gap_0p5()
                    .child(section_title(title, theme))
                    .child(
                        div()
                            .w(design(sizes::WIDTH_1Y))
                            .h(design(sizes::HEIGHT))
                            .children(glyph),
                    )
                    .child(
                        div()
                            .text_color(theme.muted_foreground)
                            .child(caption.clone()),
                    )
            }),
    )
}

/// The context pairs: a muted label column beside the values.
fn context(band: &BandModel, theme: &Theme) -> Div {
    v_flex()
        .flex_none()
        .gap_0p5()
        .child(section_title(CONTEXT_TITLE, theme))
        .children(band.context.iter().map(|(label, value)| {
            h_flex()
                .gap_2()
                .child(
                    div()
                        .flex_none()
                        .w(design(CONTEXT_LABEL_WIDTH))
                        .text_color(theme.muted_foreground)
                        .child(label.clone()),
                )
                .child(
                    div()
                        .whitespace_nowrap()
                        .text_color(theme.foreground)
                        .child(value.clone()),
                )
        }))
}

/// The band under the grid, above the footer: the member's name, then the
/// surface, the sparks and the context side by side.
pub(crate) fn render(band: &BandModel, paints: &Paints, theme: &Theme, tile: u64) -> Div {
    v_flex()
        .w_full()
        .flex_none()
        .gap_1()
        .px_2()
        .py_1()
        .text_xs()
        .font_family(fonts::MONO)
        .border_t_1()
        .border_color(theme.border)
        .bg(theme.background)
        .overflow_hidden()
        .debug_selector(move || format!("volwatch-band-{tile}"))
        .child(div().text_color(theme.foreground).child(band.name.clone()))
        .child(
            h_flex()
                .items_start()
                .gap_4()
                .min_w_0()
                .child(surface(band, paints, theme))
                .child(sparks(band, paints, theme))
                .child(context(band, theme)),
        )
}
