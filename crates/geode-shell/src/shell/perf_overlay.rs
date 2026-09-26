//! Performance readout over the top-right of the tile area.
//!
//! Shows frame interval count, p50, p95, and maximum, plus the latest
//! requery timings and their medians. Values come from
//! [`FrameHistogram`] and [`RequeryStats`].
//!
//! The panel adds no timer and requests no frames: while idle it keeps the
//! last painted values. Any shell redraw refreshes the readout. Forcing
//! frames would alter the render intervals being measured.
//!
//! Rendering formats short metric strings and queries fixed-size histogram
//! buckets. Colours come from the theme; values use the monospace data face.

use gpui::prelude::*;
use gpui::{App, IntoElement, Pixels, div, px};
use gpui_component::{ActiveTheme as _, h_flex, v_flex};

use super::scale;
use crate::fonts;
use crate::perf::{FrameHistogram, RequeryStats, format_ms};

/// Panel width and margin from the window edges, in px.
const WIDTH: f32 = 168.0;
const MARGIN: f32 = 8.0;

/// One `label → value` row of the readout.
fn row(label: &'static str, value: String, cx: &App) -> impl IntoElement {
    let theme = cx.theme();
    h_flex()
        .w_full()
        .justify_between()
        .gap_3()
        .child(div().text_color(theme.muted_foreground).child(label))
        .child(div().font_family(fonts::MONO).child(value))
}

/// Build the overlay panel. Pure function of the histogram + requery + theme — no
/// stored state, no side effects — mirroring `whichkey::render`'s shape
/// (absolute-positioned instant panel, theme tokens, test-only
/// `debug_selector` hook).
pub fn render(
    hist: &FrameHistogram,
    requery: &RequeryStats,
    toolbar_height: f32,
    rem_size: Pixels,
    cx: &App,
) -> impl IntoElement {
    let theme = cx.theme();
    let margin = scale::design_px(MARGIN, rem_size);

    let dash = || "—".to_string();
    let p50 = hist.percentile_micros(50.0).map_or_else(dash, format_ms);
    let p95 = hist.percentile_micros(95.0).map_or_else(dash, format_ms);
    let max = if hist.count() == 0 {
        dash()
    } else {
        format_ms(hist.max_micros())
    };

    div()
        .absolute()
        .right(px(margin))
        .top(px(toolbar_height + margin))
        .w(scale::design(WIDTH))
        .flex()
        .flex_col()
        .gap_1()
        .p_2()
        .text_sm()
        .bg(theme.popover)
        .text_color(theme.popover_foreground)
        .border_1()
        .border_color(theme.border)
        .rounded(theme.radius_lg)
        // Test-only hook (no-op outside test builds — same pattern as
        // "whichkey-overlay") so a #[gpui::test] can confirm it painted.
        .debug_selector(|| "perf-overlay".to_string())
        .child(
            v_flex()
                .w_full()
                .gap_1()
                .child(row("frames", hist.count().to_string(), cx))
                .child(row("p50", p50, cx))
                .child(row("p95", p95, cx))
                .child(row("max", max, cx))
                .child(row(
                    "requery",
                    match requery.last() {
                        Some((q, p)) => format!("{} + {}", format_ms(q), format_ms(p)),
                        None => dash(),
                    },
                    cx,
                ))
                .child(row(
                    "q p50",
                    requery
                        .submit_to_snapshot()
                        .percentile_micros(50.0)
                        .map_or_else(dash, format_ms),
                    cx,
                ))
                .child(row(
                    "paint p50",
                    requery
                        .snapshot_to_paint()
                        .percentile_micros(50.0)
                        .map_or_else(dash, format_ms),
                    cx,
                )),
        )
}
