//! The spark element at grid density: a 30-day sparkline column, a term
//! glyph column on a range shared across the rows with the 3M point
//! marked, and a one-year column at band width. The rows include a flat
//! series, a series ending in NaN, a NaN gap, a single value, an empty
//! series and an outlier term structure.
//!
//! Run with `cargo run -p geode-chart --example spark`; `t` toggles the
//! light and dark themes.

use std::sync::Arc;

use geode_chart::core::spark::shared_range;
use geode_chart::spark::{Spark, SparkPaint, sizes};
use gpui::{App, Context, KeyDownEvent, Render, SharedString, Window, div, prelude::*, rems};
use gpui_component::{ActiveTheme, Root, Theme, ThemeMode};

const TENORS: usize = 8;
/// The 3M tenor's position among 1M, 2M, 3M, 6M, 1Y, 18M, 2Y, 3Y.
const MARK_3M: usize = 2;

struct Row {
    name: SharedString,
    last: SharedString,
    month: Arc<[f64]>,
    term: Arc<[f64]>,
    year: Arc<[f64]>,
}

fn walk(n: usize, seed: u64, level: f64) -> Vec<f64> {
    let mut s = seed | 1;
    let mut v = level;
    (0..n)
        .map(|_| {
            s ^= s << 13;
            s ^= s >> 7;
            s ^= s << 17;
            v += ((s % 200) as f64 - 100.0) / 20_000.0;
            v
        })
        .collect()
}

fn term(level: f64, slope: f64) -> Vec<f64> {
    (0..TENORS)
        .map(|i| level + slope * (i as f64).sqrt())
        .collect()
}

fn rows() -> Vec<Row> {
    let names = [
        "SPX", "SX5E", "NDX", "UKX", "NKY", "HSI", "DAX", "CAC", "SMI", "KOSPI", "RTY", "AS51",
    ];
    names
        .iter()
        .enumerate()
        .map(|(i, name)| {
            let seed = i as u64 * 7 + 3;
            let mut month = walk(30, seed, 0.18 + i as f64 * 0.005);
            let mut term = term(0.16 + i as f64 * 0.004, 0.006);
            match *name {
                "NDX" => month = vec![0.21; 30],
                "UKX" => month[29] = f64::NAN,
                "NKY" => month[10..14].fill(f64::NAN),
                "HSI" => month = vec![0.24],
                "DAX" => month = Vec::new(),
                "KOSPI" => term = vec![0.30, 0.31, 0.33, 0.34, 0.35, 0.36, 0.36, 0.37],
                _ => {}
            }
            let last = month
                .iter()
                .rev()
                .find(|v| v.is_finite())
                .map_or("—".to_string(), |v| format!("{:.1}", v * 100.0));
            Row {
                name: (*name).into(),
                last: last.into(),
                month: month.into(),
                term: term.into(),
                year: walk(252, seed + 1, 0.2).into(),
            }
        })
        .collect()
}

struct Demo {
    rows: Vec<Row>,
    term_range: Option<(f64, f64)>,
    focus: gpui::FocusHandle,
}

/// One cell: the glyph's width in rems at the 12 px design rem.
fn cell(width_px: f32) -> gpui::Div {
    div()
        .w(rems(width_px / 12.0))
        .h(rems(sizes::HEIGHT / 12.0))
        .flex_none()
}

impl Render for Demo {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = cx.theme();
        let paint = SparkPaint {
            line: t.muted_foreground,
            end: t.primary,
            mark: t.primary,
            ring: t.background,
        };
        let header = |label: &'static str, width_px: f32| {
            div()
                .w(rems(width_px / 12.0))
                .flex_none()
                .text_color(t.muted_foreground)
                .child(label)
        };
        div()
            .track_focus(&self.focus)
            .size_full()
            .bg(t.background)
            .text_color(t.foreground)
            .font_family("monospace")
            .text_sm()
            .p_4()
            .on_key_down(cx.listener(|_, e: &KeyDownEvent, window, cx| {
                if e.keystroke.key == "t" {
                    let mode = if cx.theme().is_dark() {
                        ThemeMode::Light
                    } else {
                        ThemeMode::Dark
                    };
                    Theme::change(mode, Some(window), cx);
                    cx.notify();
                }
            }))
            .child(
                div()
                    .flex()
                    .gap_3()
                    .pb_1()
                    .child(header("", 48.0))
                    .child(header("30d", sizes::WIDTH_30D))
                    .child(header("", 36.0))
                    .child(header("term", sizes::WIDTH_TERM))
                    .child(header("1y", sizes::WIDTH_1Y)),
            )
            .children(self.rows.iter().map(|r| {
                div()
                    .flex()
                    .items_center()
                    .gap_3()
                    .h(rems(21.0 / 12.0))
                    .child(div().w(rems(4.0)).flex_none().child(r.name.clone()))
                    .child(cell(sizes::WIDTH_30D).child(Spark::new(r.month.clone(), paint)))
                    .child(
                        div()
                            .w(rems(3.0))
                            .flex_none()
                            .flex()
                            .justify_end()
                            .child(r.last.clone()),
                    )
                    .child(
                        cell(sizes::WIDTH_TERM).child(
                            Spark::new(r.term.clone(), paint)
                                .range(self.term_range)
                                .mark(Some(MARK_3M))
                                .end_dot(false),
                        ),
                    )
                    .child(cell(sizes::WIDTH_1Y).child(Spark::new(r.year.clone(), paint)))
            }))
    }
}

fn main() {
    gpui_platform::application().run(|cx: &mut App| {
        gpui_component::init(cx);
        cx.open_window(gpui::WindowOptions::default(), |window, cx| {
            let rows = rows();
            let term_range = shared_range(rows.iter().map(|r| &r.term[..]));
            let demo = cx.new(|cx| {
                let focus = cx.focus_handle();
                focus.focus(window, cx);
                Demo {
                    rows,
                    term_range,
                    focus,
                }
            });
            cx.new(|cx| Root::new(demo, window, cx))
        })
        .expect("open window");
        cx.activate(true);
    });
}
