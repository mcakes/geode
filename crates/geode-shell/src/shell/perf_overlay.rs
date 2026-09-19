//! The frame-time debug overlay (spec §7.4: "visible via a debug overlay
//! toggle"): a small palette-style instant panel over the top-right corner
//! showing live p50/p95/max frame intervals and the sample count since the
//! last reset, read straight from `ShellView`'s [`crate::perf::FrameHistogram`].
//!
//! **Refresh semantics** (recorded decision): this overlay adds NO timer and
//! never forces frames — that would violate the render discipline it exists
//! to observe (an always-animating diagnostic would turn an idle app into a
//! 60Hz one and poison its own measurements). The numbers shown are as-of
//! the last invalidation: while the shell is idle the panel simply keeps
//! showing the stats from the last frame that painted, and any interaction
//! that redraws the window refreshes it for free.
//!
//! Rendering cost: four short `String`s per painted frame (the sanctioned
//! small-String class — same as the status bar's pending-keystroke text);
//! percentile queries are a walk of 37 integer counters. Theme tokens only
//! (popover/border/muted — no raw colors); values in the mono data face.

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
