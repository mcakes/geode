//! A standalone window over a generated three-slot model:
//! `cargo run -p geode-chart --example chart`.
//!
//! The display fixture has two panes (`s3` on the lower left), density bars,
//! three percentiles per slot and a NaN gap every 97 buckets. Its session
//! axis spans five 400-bar days. Example keys: `h`/`l` pan, `=`/`+` zoom in,
//! `-` zoom out and `0` reset; these are handled by the demo view.
//!
//! This binary computes fixture percentiles and bins by sorting generated
//! values. Production callers supply those statistics in the chart model.

use std::sync::Arc;

use geode_chart::View;
use geode_chart::core::axis::{Axis, AxisMode};
use geode_chart::core::palette::Palette;
use geode_chart::timeseries::{ChartElement, ChartModel, ChartSlot};
use gpui::{App, Context, KeyDownEvent, Render, Window, div, prelude::*};
use gpui_component::{ActiveTheme, Root};

struct Demo {
    model: Arc<ChartModel>,
    view: View,
    focus: gpui::FocusHandle,
}

/// Three random walks on a five-session minute axis, each on its own
/// axis, each in its own palette colour.
fn model(cx: &App) -> Arc<ChartModel> {
    let n = 2_000usize;
    let minute = 60_000_000i64;
    let day = 86_400_000_000i64;
    let start = 1_767_621_000_000_000i64; // 2026-01-05 14:30 UTC
    // 400 one-minute bars a session, five sessions: the gaps between
    // them are what `AxisMode::Session` closes up.
    let buckets: Vec<i64> = (0..n as i64)
        .map(|i| start + (i / 400) * day + (i % 400) * minute)
        .collect();
    let t = cx.theme();
    let palette = Palette::from_theme(
        [t.chart_1, t.chart_2, t.chart_3, t.chart_4, t.chart_5],
        t.background,
        t.foreground,
    );

    // A seeded xorshift walk around `base`, with a hole every 97th
    // bucket so the element's NaN breaks are visible — the same shape
    // `element.rs`'s test fixture generates, with the step scaled to the
    // base so all three slots wiggle by the same proportion.
    let walk = |seed: u64, base: f64| -> Vec<f64> {
        let mut s = seed | 1;
        let mut v = base;
        (0..n)
            .map(|i| {
                s ^= s << 13;
                s ^= s >> 7;
                s ^= s << 17;
                v += base * ((s % 200) as f64 - 100.0) / 20_000.0;
                if i % 97 == 50 { f64::NAN } else { v }
            })
            .collect()
    };

    let slot = |number: u8, axis: Axis, seed: u64, base: f64| -> ChartSlot {
        let values = walk(seed, base);
        // Prepare statistics for the fixture. Production models receive
        // these values from the data tier.
        let mut sorted: Vec<f64> = values.iter().copied().filter(|v| v.is_finite()).collect();
        sorted.sort_by(f64::total_cmp);
        // Nearest-rank percentiles over the sorted finite values.
        let at = |fraction: f64| -> f64 {
            let rank = (fraction * sorted.len() as f64).ceil() as usize;
            sorted[rank.saturating_sub(1).min(sorted.len() - 1)]
        };
        let percentiles: Vec<(f64, f64)> = if sorted.is_empty() {
            Vec::new()
        } else {
            [0.05, 0.5, 0.95].iter().map(|f| (*f, at(*f))).collect()
        };
        // Forty equal-width bins over the walk's own range.
        const BINS: usize = 40;
        let mut counts = [0u32; BINS];
        let (lo, hi) = match (sorted.first(), sorted.last()) {
            (Some(lo), Some(hi)) => (*lo, *hi),
            _ => (0.0, 0.0),
        };
        let width = ((hi - lo) / BINS as f64).max(f64::MIN_POSITIVE);
        for v in &sorted {
            counts[(((v - lo) / width).floor() as usize).min(BINS - 1)] += 1;
        }
        ChartSlot {
            number,
            label: format!("s{number}").into(),
            values,
            // Each slot takes its own colour off the theme's five, so
            // the three lines are told apart by hue as well as by pane.
            colour: palette.colour(number as usize - 1),
            axis,
            visible: true,
            percentile_labels: percentiles
                .iter()
                .map(|(f, _)| ChartModel::percentile_label(*f))
                .collect(),
            percentiles,
            bins: if sorted.is_empty() {
                Vec::new()
            } else {
                counts
                    .iter()
                    .enumerate()
                    .map(|(b, c)| (lo + b as f64 * width, lo + (b + 1) as f64 * width, *c))
                    .collect()
            },
        }
    };

    Arc::new(ChartModel {
        version: 1,
        buckets,
        step_us: minute,
        axis_mode: AxisMode::Session,
        offset_secs: 0,
        split: 0.7,
        density: true,
        slots: vec![
            slot(1, Axis::Left, 7, 100.0),
            slot(2, Axis::Right, 11, 20.0),
            slot(3, Axis::BottomLeft, 13, 1.0),
        ],
    })
}

impl Render for Demo {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let full = self.model.full();
        div()
            .track_focus(&self.focus)
            .size_full()
            .bg(cx.theme().background)
            .p_4()
            .on_key_down(cx.listener(move |this, e: &KeyDownEvent, _, cx| {
                match e.keystroke.key.as_str() {
                    "h" => this.view.pan(-0.1, full),
                    "l" => this.view.pan(0.1, full),
                    "=" | "+" => this.view.zoom(1.25, 0.5, full),
                    "-" => this.view.zoom(0.8, 0.5, full),
                    "0" => this.view.reset(full),
                    _ => return,
                }
                cx.notify();
            }))
            .child(ChartElement::new(
                self.model.clone(),
                self.view,
                window.rem_size().as_f32(),
                "chart",
            ))
    }
}

fn main() {
    gpui_platform::application().run(|cx: &mut App| {
        gpui_component::init(cx);
        cx.open_window(gpui::WindowOptions::default(), |window, cx| {
            let model = model(cx);
            let demo = cx.new(|cx| {
                let focus = cx.focus_handle();
                focus.focus(window, cx);
                Demo {
                    view: View::full(model.full()),
                    model,
                    focus,
                }
            });
            cx.new(|cx| Root::new(demo, window, cx))
        })
        .expect("open window");
        cx.activate(true);
    });
}
