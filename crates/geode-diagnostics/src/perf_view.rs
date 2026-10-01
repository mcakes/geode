//! Prepared performance readouts. Frame intervals describe render cadence;
//! the UI work budget is guidance, not a threshold for coloring those intervals.

use geode_shell::fonts;
use geode_shell::perf::format_ms;
use geode_shell::shell::{chip, scale};
use gpui::prelude::*;
use gpui::{AnyElement, Context, SharedString, WeakEntity, div};
use gpui_component::scroll::ScrollableElement as _;
use gpui_component::switch::Switch;
use gpui_component::tooltip::Tooltip;
use gpui_component::{ActiveTheme as _, Sizable as _, h_flex, v_flex};

use crate::model::{Percentiles, PerfModel};
use crate::page::DiagnosticsPage;
use crate::page_chrome::probed;

/// Histogram height in design pixels; all bar heights follow the rem scale.
const HISTOGRAM_HEIGHT: f32 = 64.0;

struct Timing {
    title: &'static str,
    help: &'static str,
    values: [SharedString; 4],
}

impl Timing {
    fn new(title: &'static str, help: &'static str, p: Option<&Percentiles>) -> Self {
        Self {
            title,
            help,
            values: p.map_or_else(
                || ["—".into(), "—".into(), "—".into(), "0".into()],
                |p| {
                    [
                        p.p50.clone().into(),
                        p.p95.clone().into(),
                        p.max.clone().into(),
                        p.samples.to_string().into(),
                    ]
                },
            ),
        }
    }
}

struct Bar {
    bound: u64,
    height: f32,
    tooltip: SharedString,
    tick: &'static str,
}

pub(crate) struct PerformanceView {
    timings: [Timing; 3],
    bars: Vec<Bar>,
    histogram_summary: SharedString,
    resources: [(&'static str, SharedString, SharedString); 3],
    has_samples: bool,
    dropped: bool,
    overlay: bool,
}

impl PerformanceView {
    #[cfg(test)]
    pub(crate) fn is_overlay_visible(&self) -> bool {
        self.overlay
    }

    pub(crate) fn new(m: &PerfModel) -> Self {
        // Overflow has its own readout: putting it on a logarithmic axis
        // would imply a finite interval that the open-ended bucket cannot give.
        let tallest = m
            .buckets
            .iter()
            .filter(|(bound, _)| *bound != u64::MAX)
            .map(|(_, n)| *n)
            .max()
            .unwrap_or(0);
        let mut lower = 0;
        let bars = m
            .buckets
            .iter()
            .filter(|(bound, _)| *bound != u64::MAX)
            .map(|(bound, n)| {
                let bar = Bar {
                    bound: *bound,
                    height: if tallest == 0 {
                        0.0
                    } else {
                        HISTOGRAM_HEIGHT * *n as f32 / tallest as f32
                    },
                    tooltip: format!("{}–{}: {n} samples", format_ms(lower), format_ms(*bound))
                        .into(),
                    tick: match *bound {
                        1_000 => "1 ms",
                        10_000 => "10 ms",
                        100_000 => "100 ms",
                        _ => "",
                    },
                };
                lower = bound.saturating_add(1);
                bar
            })
            .collect();
        let unavailable = || SharedString::from("Not available");
        Self {
            timings: [
                Timing::new(
                    "Frame interval",
                    "Time between shell renders; includes scheduling and display cadence. Idle gaps of 500 ms or more are excluded.",
                    m.frame.as_ref(),
                ),
                Timing::new(
                    "Query → snapshot",
                    "From query submission until the data snapshot arrives.",
                    m.submit.as_ref(),
                ),
                Timing::new(
                    "Snapshot → paint",
                    "From snapshot arrival until the view first renders it.",
                    m.paint.as_ref(),
                ),
            ],
            bars,
            histogram_summary: format!("{} samples · {} above 100 ms", m.frame_count, m.overflow)
                .into(),
            resources: [
                (
                    "Database",
                    if m.database.is_empty() {
                        unavailable()
                    } else {
                        m.database.clone().into()
                    },
                    if m.database.is_empty() {
                        "Waiting for a catalog snapshot".into()
                    } else {
                        format!("{} used · {} blocks", m.used, m.block_size).into()
                    },
                ),
                (
                    "DuckDB memory",
                    if m.memory.is_empty() {
                        unavailable()
                    } else {
                        m.memory.clone().into()
                    },
                    if m.threads.is_empty() {
                        "Waiting for a catalog snapshot".into()
                    } else {
                        format!("{} threads · from the last catalog snapshot", m.threads).into()
                    },
                ),
                (
                    "Dropped events",
                    m.dropped.to_string().into(),
                    "Since application start".into(),
                ),
            ],
            has_samples: m.frame_count > 0,
            dropped: m.dropped > 0,
            overlay: m.overlay,
        }
    }
}

pub(crate) fn render(
    model: Option<&PerformanceView>,
    weak: WeakEntity<DiagnosticsPage>,
    cx: &mut Context<DiagnosticsPage>,
) -> AnyElement {
    let theme = cx.theme();
    let Some(m) = model else {
        return div()
            .p_3()
            .child("Performance samples are not available yet.")
            .into_any_element();
    };
    let overlay = probed(
        "diagnostics-overlay-switch",
        Switch::new("diagnostics-overlay")
            .small()
            .checked(m.overlay)
            .label("Performance overlay")
            .on_change(move |_, _, cx| {
                let _ = weak.update(cx, |p, cx| {
                    p.diagnostics.update(cx, |d, cx| {
                        d.request_overlay_toggle();
                        cx.notify();
                    });
                });
            }),
    );
    let timings = m.timings.iter().map(|timing| {
        v_flex()
            .gap_2()
            .py_3()
            .border_b_1()
            .border_color(theme.border)
            .child(
                div()
                    .text_sm()
                    .font_weight(gpui::FontWeight::MEDIUM)
                    .child(timing.title),
            )
            .child(
                div()
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .child(timing.help),
            )
            .child(
                h_flex().gap_3().children(
                    ["Median (p50)", "p95", "Maximum", "Samples"]
                        .into_iter()
                        .zip(&timing.values)
                        .map(|(label, value)| {
                            v_flex()
                                .debug_selector(move || {
                                    format!("diagnostics-metric-{}-{label}", timing.title)
                                })
                                .text_right()
                                .flex_1()
                                .min_w_0()
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
                                        .child(value.clone()),
                                )
                        }),
                ),
            )
    });
    let histogram = h_flex()
        .items_end()
        .gap_px()
        .debug_selector(|| "diagnostics-histogram".to_string())
        .children(m.bars.iter().map(|bar| {
            let tooltip = bar.tooltip.clone();
            v_flex()
                .id(("diagnostics-bar", bar.bound))
                .flex_1()
                .min_w_0()
                .tooltip(move |window, cx| Tooltip::new(tooltip.clone()).build(window, cx))
                .child(
                    h_flex()
                        .items_end()
                        .h(scale::design(HISTOGRAM_HEIGHT))
                        .border_b_1()
                        .border_color(theme.border)
                        .child(
                            div()
                                .w_full()
                                .h(scale::design(bar.height))
                                .bg(theme.primary),
                        ),
                )
                .child(div().h_5().relative().when(!bar.tick.is_empty(), |el| {
                    el.child(
                        div()
                            .absolute()
                            .right_0()
                            .text_xs()
                            .whitespace_nowrap()
                            .text_color(theme.muted_foreground)
                            .child(bar.tick),
                    )
                }))
        }));
    let resources = m.resources.iter().map(|(label, value, help)| {
        v_flex()
            .py_2()
            .gap_1()
            .child(
                h_flex()
                    .gap_3()
                    .child(div().flex_1().text_sm().child(*label))
                    .child(
                        div()
                            .font_family(fonts::MONO)
                            .text_sm()
                            .when(*label == "Dropped events" && m.dropped, |el| {
                                el.text_color(chip::chip_paint(theme, chip::Tone::WarningText).text)
                            })
                            .child(value.clone()),
                    ),
            )
            .child(
                div()
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .child(help.clone()),
            )
    });
    let explanation = div()
        .text_xs()
        .text_color(theme.muted_foreground)
        .child("p95 estimates the interval covering 95% of samples. Percentiles use histogram buckets; a dash means no samples yet.");
    let budgets = div()
        .text_xs()
        .text_color(theme.muted_foreground)
        .child("Targets: UI work under 8 ms; a requery at one million rows under 50 ms. Frame intervals measure cadence, not time spent doing UI work.");
    let heading = |text| {
        div()
            .text_sm()
            .font_weight(gpui::FontWeight::SEMIBOLD)
            .child(text)
    };
    let distribution = v_flex()
        .gap_2()
        .py_2()
        .child(heading("Frame interval distribution"))
        .child(
            div()
                .text_xs()
                .text_color(theme.muted_foreground)
                .child(m.histogram_summary.clone()),
        )
        .child(if m.has_samples {
            histogram.into_any_element()
        } else {
            div()
                .py_4()
                .text_xs()
                .text_color(theme.muted_foreground)
                .child("Samples appear as you interact with the app.")
                .into_any_element()
        })
        .child(
            div()
                .text_xs()
                .text_color(theme.muted_foreground)
                .child("Logarithmic bucket spacing · height shows sample count"),
        );
    div()
        .id("diagnostics-performance")
        .flex_1()
        .min_h_0()
        .min_w_0()
        .debug_selector(|| "diagnostics-performance".to_string())
        .child(
            v_flex()
                .p_3()
                .gap_3()
                .child(
                    h_flex()
                        .flex_wrap()
                        .gap_2()
                        .child(heading("Timing").flex_1())
                        .child(overlay),
                )
                .child(explanation)
                .child(v_flex().children(timings))
                .child(budgets)
                .child(distribution)
                .child(heading("Storage and delivery"))
                .children(resources),
        )
        .overflow_y_scrollbar()
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;
    use geode_core::log::LogLevels;
    use geode_shell::diagnostics::Diagnostics;
    use geode_shell::perf::{FrameHistogram, RequeryStats};

    #[test]
    fn performance_readouts_keep_sample_counts_and_overflow_separate() {
        let mut d = Diagnostics::new(LogLevels::default());
        d.watch();
        let mut hist = FrameHistogram::new();
        hist.record_micros_n(1_000, 4);
        hist.record_micros(200_000);
        d.refresh_frame_hist(&hist);
        let mut requery = RequeryStats::new();
        requery.record_submit_to_snapshot(2_000);
        requery.record_snapshot_to_paint(3_000);
        requery.record_snapshot_to_paint(4_000);
        let view = PerformanceView::new(&crate::model::perf_model(&d, &requery));
        assert_eq!(view.timings[0].values[3].as_ref(), "5");
        assert_eq!(view.timings[1].values[3].as_ref(), "1");
        assert_eq!(view.timings[2].values[3].as_ref(), "2");
        assert_eq!(
            view.histogram_summary.as_ref(),
            "5 samples · 1 above 100 ms"
        );
        assert!(view.bars.iter().all(|b| b.bound != u64::MAX));
        let bar = view.bars.iter().find(|b| b.bound == 1_000).unwrap();
        assert_eq!(bar.height, HISTOGRAM_HEIGHT);
        assert!(bar.tooltip.ends_with(": 4 samples"));
        assert_eq!(bar.tick, "1 ms");
        let empty = PerformanceView::new(&crate::model::perf_model(
            &Diagnostics::new(LogLevels::default()),
            &RequeryStats::new(),
        ));
        assert!(!empty.has_samples);
        assert_eq!(
            empty.timings[0].values,
            ["—", "—", "—", "0"].map(SharedString::from)
        );
        assert!(empty.bars.iter().all(|b| b.height == 0.0));
    }
}
