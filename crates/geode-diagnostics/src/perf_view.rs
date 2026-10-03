//! Prepared performance readouts. Frame intervals describe render cadence;
//! the UI work budget is guidance, not a threshold for coloring those intervals.

use geode_shell::fonts;
use geode_shell::memory;
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

/// Help suffix on every row read from the catalog rather than sampled.
const FROM_SNAPSHOT: &str = "from the last catalog snapshot";

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
    memory: [(&'static str, SharedString, SharedString); 4],
    resources: [(&'static str, SharedString, SharedString); 3],
    has_samples: bool,
    dropped: bool,
    refused: bool,
    overlay: bool,
}

impl PerformanceView {
    #[cfg(test)]
    pub(crate) fn is_overlay_visible(&self) -> bool {
        self.overlay
    }

    #[cfg(test)]
    pub(crate) fn resource(&self, label: &str) -> Option<&str> {
        self.metric(label).map(|(value, _)| value)
    }

    /// A Memory or Storage row's `(value, help)` by label.
    #[cfg(test)]
    pub(crate) fn metric(&self, label: &str) -> Option<(&str, &str)> {
        self.memory
            .iter()
            .chain(&self.resources)
            .find(|(l, _, _)| *l == label)
            .map(|(_, value, help)| (value.as_ref(), help.as_ref()))
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
        let waiting = || SharedString::from("Waiting for a catalog snapshot");
        let snapshot = |text: String| SharedString::from(format!("{text} · {FROM_SNAPSHOT}"));
        let catalog = !m.threads.is_empty();
        let memory = [
            match &m.process {
                Some(p) => (
                    "Process memory",
                    p.current.clone().into(),
                    format!("Peak {} at {} · {}", p.peak, p.peak_at, memory::MEASURE).into(),
                ),
                None => (
                    "Process memory",
                    unavailable(),
                    if memory::SUPPORTED {
                        "Waiting for the first sample".into()
                    } else {
                        "Not measured on this platform".into()
                    },
                ),
            },
            (
                "DuckDB memory",
                if !catalog {
                    unavailable()
                } else if m.memory_limit.is_empty() {
                    format!("{} of unknown limit", m.memory).into()
                } else if m.memory_limit == crate::model::UNLIMITED {
                    format!("{} · no limit", m.memory).into()
                } else {
                    format!("{} of {}", m.memory, m.memory_limit).into()
                },
                if catalog {
                    snapshot(format!("{} threads", m.threads))
                } else {
                    waiting()
                },
            ),
            (
                "Temporary files",
                if catalog {
                    m.temp.clone().into()
                } else {
                    unavailable()
                },
                if catalog {
                    snapshot("Spilled to disk".into())
                } else {
                    waiting()
                },
            ),
            (
                "Largest DuckDB tags",
                match m.memory_top.first() {
                    _ if !catalog => unavailable(),
                    Some((tag, _)) => tag.clone().into(),
                    None => "None".into(),
                },
                if !catalog {
                    waiting()
                } else if m.memory_top.is_empty() {
                    snapshot("No tag holds memory".into())
                } else {
                    snapshot(
                        m.memory_top
                            .iter()
                            .map(|(tag, size)| format!("{tag} {size}"))
                            .collect::<Vec<_>>()
                            .join(" · "),
                    )
                },
            ),
        ];
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
            memory,
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
                        waiting()
                    } else {
                        format!("{} used · {} blocks", m.used, m.block_size).into()
                    },
                ),
                (
                    "Dropped events",
                    m.dropped.to_string().into(),
                    "Since application start".into(),
                ),
                (
                    "Refused requests",
                    m.refused.to_string().into(),
                    "Data requests refused because the request queue was full, since application start".into(),
                ),
            ],
            has_samples: m.frame_count > 0,
            dropped: m.dropped > 0,
            refused: m.refused > 0,
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
    let metric = |(label, value, help): &(&'static str, SharedString, SharedString)| {
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
                            .when(
                                (*label == "Dropped events" && m.dropped)
                                    || (*label == "Refused requests" && m.refused),
                                |el| {
                                    el.text_color(
                                        chip::chip_paint(theme, chip::Tone::WarningText).text,
                                    )
                                },
                            )
                            .child(value.clone()),
                    ),
            )
            .child(
                div()
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .child(help.clone()),
            )
    };
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
                .child(heading("Memory"))
                .children(m.memory.iter().map(metric))
                .child(heading("Storage and delivery"))
                .children(m.resources.iter().map(metric)),
        )
        .overflow_y_scrollbar()
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;
    use geode_core::clock::Clock;
    use geode_core::log::LogLevels;
    use geode_core::query::CatalogSnapshot;
    use geode_shell::diagnostics::Diagnostics;
    use geode_shell::memory::MemoryReading;
    use geode_shell::perf::{FrameHistogram, RequeryStats};
    use std::time::{Duration, SystemTime};

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
        let view = PerformanceView::new(&crate::model::perf_model(&d, &requery, Clock::utc()));
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
            Clock::utc(),
        ));
        assert!(!empty.has_samples);
        assert_eq!(
            empty.timings[0].values,
            ["—", "—", "—", "0"].map(SharedString::from)
        );
        assert!(empty.bars.iter().all(|b| b.height == 0.0));
    }

    #[test]
    fn the_refused_requests_row_reads_the_status_bar_counter() {
        let mut d = Diagnostics::new(LogLevels::default());
        d.note_refused(3);
        let view = PerformanceView::new(&crate::model::perf_model(
            &d,
            &RequeryStats::new(),
            Clock::utc(),
        ));
        assert_eq!(view.resource("Refused requests"), Some("3"));
        assert!(view.refused);
        assert_eq!(view.resource("Dropped events"), Some("0"));
    }

    #[test]
    fn the_memory_rows_wait_then_read_the_sample_and_the_catalog() {
        const GIB: u64 = 1024 * 1024 * 1024;
        let mut d = Diagnostics::new(LogLevels::default());
        let view = PerformanceView::new(&crate::model::perf_model(
            &d,
            &RequeryStats::new(),
            Clock::utc(),
        ));
        let waiting_sample = if memory::SUPPORTED {
            "Waiting for the first sample"
        } else {
            "Not measured on this platform"
        };
        assert_eq!(
            view.metric("Process memory"),
            Some(("Not available", waiting_sample))
        );
        for label in ["DuckDB memory", "Temporary files", "Largest DuckDB tags"] {
            assert_eq!(
                view.metric(label),
                Some(("Not available", "Waiting for a catalog snapshot")),
                "{label}"
            );
        }

        d.watch();
        d.refresh_memory(&MemoryReading {
            current_bytes: 3 * GIB,
            peak_bytes: 7 * GIB,
            peak_at: SystemTime::UNIX_EPOCH + Duration::from_secs(3_723),
        });
        d.set_catalog(
            CatalogSnapshot {
                memory_bytes: 2 * GIB,
                memory_limit_bytes: 38 * GIB,
                temp_bytes: 0,
                memory_top: vec![("BASE_TABLE".into(), GIB), ("HASH_TABLE".into(), GIB / 2)],
                threads: 8,
                ..CatalogSnapshot::default()
            },
            SystemTime::now(),
        );
        let view = PerformanceView::new(&crate::model::perf_model(
            &d,
            &RequeryStats::new(),
            Clock::utc(),
        ));
        let peak_help = format!("Peak 7.0GB at 01:02:03 · {}", memory::MEASURE);
        assert_eq!(
            view.metric("Process memory"),
            Some(("3.0GB", peak_help.as_str()))
        );
        assert_eq!(
            view.metric("DuckDB memory"),
            Some((
                "2.0GB of 38.0GB",
                "8 threads · from the last catalog snapshot"
            ))
        );
        let mut unlimited = d.catalog.clone().unwrap();
        unlimited.memory_limit_bytes = u64::MAX;
        d.set_catalog(unlimited, SystemTime::now());
        let unlimited_view = PerformanceView::new(&crate::model::perf_model(
            &d,
            &RequeryStats::new(),
            Clock::utc(),
        ));
        assert_eq!(
            unlimited_view
                .metric("DuckDB memory")
                .map(|(value, _)| value),
            Some("2.0GB · no limit")
        );
        assert_eq!(
            view.metric("Temporary files"),
            Some(("0B", "Spilled to disk · from the last catalog snapshot"))
        );
        assert_eq!(
            view.metric("Largest DuckDB tags"),
            Some((
                "BASE_TABLE",
                "BASE_TABLE 1.0GB · HASH_TABLE 512.0MB · from the last catalog snapshot"
            ))
        );
    }
}
