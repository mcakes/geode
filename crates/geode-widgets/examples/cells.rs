//! The cell glyphs at grid density, for the owner's display check: eight
//! rows like the vol board's Changes and Rich/cheap views, with a heat-tinted
//! level, a change chip, a percentile slider and a bar from zero. The rows
//! include a change that displays as zero, a missing change and rank, both
//! decile edges and a two-digit spread.
//! `t` toggles light and dark.

use geode_core::format::Sign;
use geode_widgets::cell::{column_scale, delta_chip, design, heat, percent_slider, zero_bar};
use gpui::{
    App, Context, FocusHandle, KeyDownEvent, Render, SharedString, Window, div, prelude::*,
};
use gpui_component::{ActiveTheme, Root, Theme, ThemeMode};

/// Host constants the vol watchlist will own: the chip and heat ceilings
/// and the bar's fill alpha (the mockup's values).
const CHIP_CEILING: f32 = 0.55;
const HEAT_CEILING: f32 = 0.4;
const BAR_ALPHA: f32 = 0.75;

/// (underlying, ATFM level, change, rank, IV-RV spread)
const ROWS: [(&str, f64, f64, f64, f64); 8] = [
    ("SPX", 15.9, 1.1, 0.91, 3.4),
    ("SX5E", 18.2, -0.4, 0.23, -1.2),
    ("NKY", 22.8, 0.04, 0.5, 0.0),
    ("UKX", 13.1, 0.25, 0.05, 1.8),
    ("HSI", 24.4, -2.2, 0.10, -4.6),
    ("KOSPI2", 19.7, f64::NAN, f64::NAN, 2.2),
    ("AS51", 11.9, 0.6, 0.90, -0.3),
    ("SMI", 14.8, -0.9, 0.97, 12.3),
];

/// The sign of `v` as displayed at one decimal, the way
/// `geode_core::format::Formatted::sign` follows the rounded value.
fn shown_sign(v: f64) -> Sign {
    let r = (v * 10.0).round();
    if r > 0.0 {
        Sign::Positive
    } else if r < 0.0 {
        Sign::Negative
    } else {
        Sign::Zero
    }
}

fn number(v: f64, signed: bool) -> SharedString {
    if !v.is_finite() {
        "—".into()
    } else if signed {
        format!("{v:+.1}").into()
    } else {
        format!("{v:.1}").into()
    }
}

struct Cells {
    focus: FocusHandle,
    /// Formatted once, as a host's prepared model would.
    text: Vec<[SharedString; 4]>,
}

impl Render for Cells {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = cx.theme();
        let radius = t.radius_tokens().sm;
        let chip_paint = delta_chip::ChipPaint {
            positive: t.chart_bullish,
            negative: t.chart_bearish,
            zero: t.muted_foreground,
            text: t.foreground,
            radius,
        };
        let slider_paint = percent_slider::SliderPaint {
            track: t.muted,
            fill: t.primary.opacity(0.6),
            tick: t.background,
            marker: t.foreground,
            warning: t.warning,
            text: t.muted_foreground,
            radius,
        };
        let bar_paint = zero_bar::BarPaint {
            positive: t.chart_bullish,
            negative: t.chart_bearish,
            zero: t.muted_foreground,
            axis: t.border,
            text: t.foreground,
            fill_alpha: BAR_ALPHA,
            radius,
        };
        let heat_paint = heat::HeatPaint {
            hue: t.primary,
            text: t.foreground,
        };
        let change_scale = column_scale(ROWS.iter().map(|r| r.2));
        let spread_scale = column_scale(ROWS.iter().map(|r| r.4));
        let levels = heat::range(ROWS.iter().map(|r| r.1));
        let header = |label: &'static str| {
            div()
                .w(design(96.0))
                .flex()
                .justify_end()
                .text_color(t.muted_foreground)
                .child(label)
        };
        let mut grid = div()
            .flex()
            .flex_col()
            .font_family(t.mono_font_family.clone())
            .text_sm()
            .child(
                div()
                    .flex()
                    .gap(design(8.0))
                    .h(design(22.0))
                    .items_center()
                    .border_b_1()
                    .border_color(t.border)
                    .child(
                        div()
                            .w(design(64.0))
                            .text_color(t.muted_foreground)
                            .child("Name"),
                    )
                    .child(header("Level"))
                    .child(header("Δ1d"))
                    .child(header("1Y pctl"))
                    .child(header("IV−RV")),
            );
        for (i, (name, level, change, rank, spread)) in ROWS.iter().enumerate() {
            let [level_t, change_t, rank_t, spread_t] = self.text[i].clone();
            let cell = |child: gpui::Div| div().w(design(96.0)).flex().justify_end().child(child);
            grid = grid.child(
                div()
                    .flex()
                    .gap(design(8.0))
                    .h(design(21.0))
                    .items_center()
                    .border_b_1()
                    .border_color(t.border.opacity(0.4))
                    .child(div().w(design(64.0)).text_color(t.foreground).child(*name))
                    .child(cell(heat::paint(
                        level_t,
                        levels.and_then(|(lo, hi)| heat::heat(*level, lo, hi, HEAT_CEILING)),
                        heat_paint,
                        format!("heat-{i}").into(),
                    )))
                    .child(cell(delta_chip::paint(
                        change_t,
                        delta_chip::chip(*change, shown_sign(*change), change_scale, CHIP_CEILING),
                        chip_paint,
                        format!("chip-{i}").into(),
                    )))
                    .child(cell(percent_slider::paint(
                        rank_t,
                        // The rank at its display precision, so the warning agrees with the
                        // number shown (90 warns; 89 does not).
                        percent_slider::slider((rank * 100.0).round() / 100.0),
                        slider_paint,
                        format!("slider-{i}").into(),
                    )))
                    .child(cell(zero_bar::paint(
                        spread_t,
                        zero_bar::bar(*spread, shown_sign(*spread), spread_scale),
                        bar_paint,
                        format!("bar-{i}").into(),
                    ))),
            );
        }
        div()
            .track_focus(&self.focus)
            .size_full()
            .bg(t.background)
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
            .child(grid)
    }
}

fn main() {
    gpui_platform::application().run(|cx: &mut App| {
        gpui_component::init(cx);
        cx.open_window(gpui::WindowOptions::default(), |window, cx| {
            let text = ROWS
                .iter()
                .map(|(_, level, change, rank, spread)| {
                    [
                        number(*level, false),
                        number(*change, true),
                        if rank.is_finite() {
                            format!("{:.0}", rank * 100.0).into()
                        } else {
                            "—".into()
                        },
                        number(*spread, true),
                    ]
                })
                .collect();
            let cells = cx.new(|cx| {
                let focus = cx.focus_handle();
                focus.focus(window, cx);
                Cells { focus, text }
            });
            cx.new(|cx| Root::new(cells, window, cx))
        })
        .expect("open window");
        cx.activate(true);
    });
}
