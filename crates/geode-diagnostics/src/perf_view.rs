//! The Performance section: metric groups with their budgets, a frame-interval
//! histogram as a row of themed bars, storage readouts, and the overlay
//! switch. The page builds the model on the perf counter; this module only
//! paints it and routes the switch back through the `Diagnostics` request
//! channel, which the shell drains and mirrors.

use geode_shell::fonts;
use geode_shell::shell::{chip, scale};
use gpui::prelude::*;
use gpui::{AnyElement, Context, WeakEntity, div};
use gpui_component::scroll::ScrollableElement as _;
use gpui_component::switch::Switch;
use gpui_component::{ActiveTheme as _, Sizable as _, h_flex, v_flex};

use crate::model::{FRAME_BUDGET_MICROS, Percentiles, PerfModel, REQUERY_BUDGET_MICROS};
use crate::page::DiagnosticsPage;
use crate::page_chrome::probed;

/// The histogram's height, in pixels at the design rem.
const HISTOGRAM_HEIGHT: f32 = 56.0;
/// One bar's width, in pixels at the design rem.
const BAR_WIDTH: f32 = 6.0;

/// Bar heights in design px scaled to the tallest bucket, and whether the
/// bucket's upper bound is past the frame budget. The budget's own bucket
/// is within: a frame at exactly 8 ms met it.
pub fn bar_heights(buckets: &[(u64, u32)], max_height: f32) -> Vec<(f32, bool)> {
    let tallest = buckets.iter().map(|(_, n)| *n).max().unwrap_or(0);
    buckets
        .iter()
        .map(|(bound, n)| {
            let h = if tallest == 0 {
                0.0
            } else {
                max_height * (*n as f32) / (tallest as f32)
            };
            (h, *bound > FRAME_BUDGET_MICROS)
        })
        .collect()
}

fn tile(
    label: &'static str,
    value: String,
    detail: String,
    cx: &Context<DiagnosticsPage>,
) -> AnyElement {
    let theme = cx.theme();
    v_flex()
        .flex_1()
        .min_w_0()
        .py_2()
        .gap_1()
        .child(
            div()
                .text_xs()
                .text_color(theme.muted_foreground)
                .child(label),
        )
        .child(
            div()
                .text_sm()
                .font_family(fonts::MONO)
                .child(if value.is_empty() {
                    "Not available".to_string()
                } else {
                    value
                }),
        )
        .child(
            div()
                .text_xs()
                .text_color(theme.muted_foreground)
                .child(detail),
        )
        .into_any_element()
}

fn pct(p: Option<&Percentiles>) -> String {
    match p {
        Some(p) => format!("{} · {} · {}", p.p50, p.p95, p.max),
        None => "no samples yet".to_string(),
    }
}

pub(crate) fn render(
    model: Option<&PerfModel>,
    weak: WeakEntity<DiagnosticsPage>,
    cx: &mut Context<DiagnosticsPage>,
) -> AnyElement {
    let theme = cx.theme();
    let Some(m) = model else {
        return v_flex()
            .p_2()
            .text_xs()
            .text_color(theme.muted_foreground)
            .child("perf: no samples yet")
            .into_any_element();
    };
    let bars = bar_heights(&m.buckets, HISTOGRAM_HEIGHT);
    let over = chip::chip_paint(theme, chip::Tone::WarningText).text;
    let within = theme.primary;
    let histogram = h_flex()
        .items_end()
        .gap_px()
        .h(scale::design(HISTOGRAM_HEIGHT))
        .border_b_1()
        .border_color(theme.border)
        .debug_selector(|| "diagnostics-histogram".to_string())
        .children(bars.iter().enumerate().map(|(i, (h, past))| {
            div()
                .id(("diagnostics-bar", i))
                .w(scale::design(BAR_WIDTH))
                .h(scale::design(*h))
                .bg(if *past { over } else { within })
        }));
    let overlay = probed(
        "diagnostics-overlay-switch",
        Switch::new("diagnostics-overlay")
            .small()
            .checked(m.overlay)
            .label("Performance overlay")
            .on_change(move |_checked, _window, cx| {
                // The shell owns the overlay: the request is drained there
                // and the value mirrored back, which rebuilds this model.
                let _ = weak.update(cx, |p, cx| {
                    p.diagnostics.update(cx, |d, cx| {
                        d.request_overlay_toggle();
                        cx.notify();
                    });
                });
            }),
    );
    div()
        .id("diagnostics-performance")
        .flex_1()
        .min_h_0()
        .child(
            v_flex()
                .p_3()
                .gap_4()
                .child(h_flex().justify_end().child(overlay))
                .child(
                    div()
                        .grid()
                        .grid_cols(2)
                        .gap_4()
                        .child(tile(
                            "Frame p50 · p95 · max",
                            pct(m.frame.as_ref()),
                            format!(
                                "n = {} · budget {} ms",
                                m.frame_count,
                                FRAME_BUDGET_MICROS / 1_000
                            ),
                            cx,
                        ))
                        .child(tile(
                            "Requery submit→snapshot",
                            pct(m.submit.as_ref()),
                            format!("budget {} ms at 1M rows", REQUERY_BUDGET_MICROS / 1_000),
                            cx,
                        ))
                        .child(tile(
                            "Requery snapshot→paint",
                            pct(m.paint.as_ref()),
                            String::new(),
                            cx,
                        ))
                        .child(tile(
                            "Dropped events",
                            m.dropped.to_string(),
                            "since start".into(),
                            cx,
                        )),
                )
                .child(
                    div()
                        .text_xs()
                        .text_color(theme.muted_foreground)
                        .child(format!(
                            "Frame interval histogram (bars past the {} ms budget tinted)",
                            FRAME_BUDGET_MICROS / 1_000
                        )),
                )
                .child(if m.frame_count == 0 {
                    div()
                        .h(scale::design(HISTOGRAM_HEIGHT))
                        .flex()
                        .items_center()
                        .text_xs()
                        .text_color(theme.muted_foreground)
                        .child("Frame samples appear as the app renders.")
                        .into_any_element()
                } else {
                    histogram.into_any_element()
                })
                .child(
                    div()
                        .grid()
                        .grid_cols(2)
                        .gap_4()
                        .child(tile(
                            "Database",
                            m.database.clone(),
                            if m.database.is_empty() {
                                "Waiting for the catalog".into()
                            } else {
                                format!("used {} · block {}", m.used, m.block_size)
                            },
                            cx,
                        ))
                        .child(tile(
                            "DuckDB memory",
                            m.memory.clone(),
                            if m.threads.is_empty() {
                                "Waiting for the catalog".into()
                            } else {
                                format!("threads {}", m.threads)
                            },
                            cx,
                        )),
                ),
        )
        .overflow_y_scrollbar()
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bar_heights_scale_to_the_tallest_bucket_and_tint_past_the_budget() {
        let buckets = vec![(1_000u64, 2u32), (8_000, 4), (10_000, 1), (u64::MAX, 0)];
        let bars = bar_heights(&buckets, 40.0);
        assert_eq!(bars[1].0, 40.0);
        assert_eq!(bars[0].0, 20.0);
        assert!(!bars[1].1, "8 ms is the budget's own bucket: within");
        assert!(bars[2].1, "past the budget");
        assert_eq!(bars[3].0, 0.0);
        assert!(bar_heights(&[], 40.0).is_empty());
    }
}
