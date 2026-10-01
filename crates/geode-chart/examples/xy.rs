//! A standalone window over a generated volatility slice:
//! `cargo run -p geode-chart --example xy`.
//!
//! The display fixture has two expiries, `3m` and `1y`. Each has a solid
//! published curve, a dashed draft curve and a chain of quotes (a diamond at
//! the mid, a bar from bid to ask) on the upper left axis, in percent. The
//! `3m` density is a line on the upper right axis, in plain numbers. The lower
//! pane holds each chain's difference from its published curve: a point at
//! mid less curve, its bar from bid less curve to ask less curve.
//!
//! What to look at:
//!
//! * The x axis opens on moneyness, labelled as a percent. `r` rebuilds the
//!   model on a reversed call-delta axis, so strike still rises left to
//!   right. At an ordinary window width its ticks read `10p 25p 50 25c 10c`;
//!   a `5c` joins them only on a plot wide enough to hold it a tick gap from
//!   `10c`. Zoom in until fewer than three of those rungs are in view and the
//!   ticks become 1-2-5 steps, still labelled as deltas.
//! * Hover near a quote: the crosshair snaps onto it and that chain's row
//!   reads `mid  bid / ask`. Hover between quotes: the crosshair glides with
//!   the cursor, the chain rows read a dash and the curves read between their
//!   knots. The two chains quote the same strikes, so on the moneyness axis
//!   a snapped crosshair reads both; a strike is a different delta at each
//!   expiry, so on the delta axis it reads the one chain it snapped to.
//! * Each y axis scales over what the view shows: zoom in and pan toward a
//!   wing, and the percent axis follows the values in view.
//!
//! Example keys: `h`/`l` move the window left and right on screen, `=`/`+`
//! zoom in about the centre, `-` zoom out, `0` reset and `r` toggle the delta
//! axis; these are handled by the demo view.
//!
//! This binary computes the smiles, deltas and density it draws. Production
//! callers supply those values in the chart model.

use std::sync::Arc;

use geode_chart::core::palette::Palette;
use geode_chart::xy::{SlotKind, Style, XAxis, XFormat, XyElement, XyModel, XySlot, YFormat};
use geode_chart::{Axis, View};
use gpui::{App, Context, Hsla, KeyDownEvent, Render, Window, div, prelude::*};
use gpui_component::{ActiveTheme, Root};

/// The narrowest window a zoom reaches, in moneyness and in delta alike.
const MIN_SPAN: f64 = 0.01;

struct Demo {
    model: Arc<XyModel>,
    view: View,
    focus: gpui::FocusHandle,
}

/// One expiry's published smile: a volatility by moneyness.
#[derive(Clone, Copy)]
struct Smile {
    label: &'static str,
    years: f64,
    atm: f64,
    skew: f64,
    convexity: f64,
}

impl Smile {
    fn vol(&self, k: f64) -> f64 {
        let m = k - 1.0;
        self.atm + self.skew * m + self.convexity * m * m
    }

    /// The draft: the published smile a little higher and steeper.
    fn draft(&self, k: f64) -> f64 {
        self.vol(k) + 0.003 - 0.05 * (k - 1.0)
    }

    /// The call delta of moneyness `k` at the published volatility, with no
    /// rates or dividends.
    fn call_delta(&self, k: f64) -> f64 {
        let s = self.vol(k) * self.years.sqrt();
        normal_cdf(-k.ln() / s + s / 2.0)
    }

    /// The lognormal density of moneyness at the at-the-money volatility.
    fn density(&self, k: f64) -> f64 {
        let s = self.atm * self.years.sqrt();
        let z = (k.ln() + s * s / 2.0) / s;
        (-z * z / 2.0).exp() / (k * s * (2.0 * std::f64::consts::PI).sqrt())
    }
}

/// The standard normal distribution function, through the Abramowitz and
/// Stegun 7.1.26 polynomial for the error function.
fn normal_cdf(x: f64) -> f64 {
    let z = x.abs() / std::f64::consts::SQRT_2;
    let t = 1.0 / (1.0 + 0.3275911 * z);
    let poly = t
        * (0.254829592
            + t * (-0.284496736 + t * (1.421413741 + t * (-1.453152027 + t * 1.061405429))));
    let erf = 1.0 - poly * (-z * z).exp();
    0.5 * (1.0 + if x < 0.0 { -erf } else { erf })
}

const EXPIRIES: [Smile; 2] = [
    Smile {
        label: "3m",
        years: 0.25,
        atm: 0.20,
        skew: -0.30,
        convexity: 0.8,
    },
    Smile {
        label: "1y",
        years: 1.0,
        atm: 0.22,
        skew: -0.15,
        convexity: 0.3,
    },
];

/// The slice on a moneyness axis, or on a reversed call-delta axis when
/// `delta` is set. Each expiry takes one palette color for its curves, its
/// chain and its differences.
fn model(cx: &App, delta: bool, version: u64) -> Arc<XyModel> {
    let t = cx.theme();
    let palette = Palette::from_theme(
        [t.chart_1, t.chart_2, t.chart_3, t.chart_4, t.chart_5],
        t.background,
        t.foreground,
    );

    // Curve knots every quarter percent of moneyness, quotes every two and a
    // half: 0.80 to 1.20.
    let knots: Vec<f64> = (0..=160).map(|i| 0.8 + i as f64 * 0.0025).collect();
    let strikes: Vec<f64> = (0..=16).map(|i| 0.8 + i as f64 * 0.025).collect();

    let mut slots = Vec::new();
    let mut number = 0u16;
    let mut slot = |label: String, color: Hsla, axis: Axis, style: Style, kind: SlotKind| {
        number += 1;
        slots.push(XySlot {
            number,
            label: label.into(),
            color,
            axis,
            visible: true,
            style,
            kind,
        });
    };

    for (e, smile) in EXPIRIES.iter().enumerate() {
        let color = palette.colour(e);
        // A strike's x on the axis in force. Call delta falls as the strike
        // rises, so the delta arrays arrive descending and `XyModel::new`
        // puts them in x order.
        let x_of = |k: f64| if delta { smile.call_delta(k) } else { k };
        let curve_x: Vec<f64> = knots.iter().map(|k| x_of(*k)).collect();
        let chain_x: Vec<f64> = strikes.iter().map(|k| x_of(*k)).collect();

        slot(
            format!("{} published", smile.label),
            color,
            Axis::Left,
            Style::Solid,
            SlotKind::Line {
                xs: curve_x.clone(),
                ys: knots.iter().map(|k| smile.vol(*k)).collect(),
            },
        );
        slot(
            format!("{} draft", smile.label),
            color,
            Axis::Left,
            Style::Dashed,
            SlotKind::Line {
                xs: curve_x,
                ys: knots.iter().map(|k| smile.draft(*k)).collect(),
            },
        );

        // A seeded xorshift scatter of the mids about the published curve,
        // inside a spread that widens into the wings.
        let mut s = (e as u64 + 1) * 7919;
        let mut mid = Vec::new();
        let mut half = Vec::new();
        for k in &strikes {
            s ^= s << 13;
            s ^= s >> 7;
            s ^= s << 17;
            let noise = ((s % 200) as f64 - 100.0) / 100.0;
            let spread = 0.002 + 0.02 * (k - 1.0).abs();
            mid.push(smile.vol(*k) + noise * spread * 0.8);
            half.push(spread);
        }
        let bid: Vec<f64> = mid.iter().zip(&half).map(|(m, h)| m - h).collect();
        let ask: Vec<f64> = mid.iter().zip(&half).map(|(m, h)| m + h).collect();
        let less_curve = |values: &[f64]| -> Vec<f64> {
            values
                .iter()
                .zip(&strikes)
                .map(|(v, k)| v - smile.vol(*k))
                .collect()
        };
        let differences = SlotKind::Points {
            xs: chain_x.clone(),
            mid: less_curve(&mid),
            lo: less_curve(&bid),
            hi: less_curve(&ask),
        };
        slot(
            format!("{} chain", smile.label),
            color,
            Axis::Left,
            Style::Solid,
            SlotKind::Points {
                xs: chain_x,
                mid,
                lo: bid,
                hi: ask,
            },
        );
        slot(
            format!("{} less curve", smile.label),
            color,
            Axis::BottomLeft,
            Style::Solid,
            differences,
        );
    }

    let front = EXPIRIES[0];
    slot(
        format!("{} density", front.label),
        palette.colour(2),
        Axis::Right,
        Style::Solid,
        SlotKind::Line {
            xs: knots
                .iter()
                .map(|k| if delta { front.call_delta(*k) } else { *k })
                .collect(),
            ys: knots.iter().map(|k| front.density(*k)).collect(),
        },
    );

    let x = if delta {
        XAxis {
            format: XFormat::Delta,
            reversed: true,
        }
    } else {
        XAxis {
            format: XFormat::Percent,
            reversed: false,
        }
    };
    // In `Axis::ALL` order: both left axes read a ratio as a percent.
    let y_format = [
        YFormat::Percent,
        YFormat::Plain,
        YFormat::Percent,
        YFormat::Plain,
    ];
    XyModel::new(version, x, y_format, 0.7, slots)
}

impl Render for Demo {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .track_focus(&self.focus)
            .size_full()
            .bg(cx.theme().background)
            .p_4()
            .on_key_down(cx.listener(move |this, e: &KeyDownEvent, _, cx| {
                let full = this.model.full();
                // The view pans and zooms in x values. The model's scale
                // turns "right on screen" and "the middle of the plot" into
                // them, whichever way the axis runs.
                let scale = this.model.x.scale();
                let right = 0.1 * scale.pan_sign();
                let centre = scale.about(0.5);
                match e.keystroke.key.as_str() {
                    "h" => this.view.pan(-right, full),
                    "l" => this.view.pan(right, full),
                    "=" | "+" => this.view.zoom(1.25, centre, full),
                    "-" => this.view.zoom(0.8, centre, full),
                    "0" => this.view.reset(full),
                    "r" => {
                        // The x units change with the axis, so the model is
                        // rebuilt under a new version and the view starts
                        // again over the new range. The delta axis is the
                        // reversed one.
                        let delta = !this.model.x.reversed;
                        this.model = model(cx, delta, this.model.version + 1);
                        this.view = View::with_min_span(this.model.full(), MIN_SPAN);
                    }
                    _ => return,
                }
                cx.notify();
            }))
            .child(XyElement::new(
                self.model.clone(),
                self.view,
                window.rem_size().as_f32(),
                "xy",
            ))
    }
}

fn main() {
    gpui_platform::application().run(|cx: &mut App| {
        gpui_component::init(cx);
        cx.open_window(gpui::WindowOptions::default(), |window, cx| {
            let model = model(cx, false, 1);
            let demo = cx.new(|cx| {
                let focus = cx.focus_handle();
                focus.focus(window, cx);
                Demo {
                    view: View::with_min_span(model.full(), MIN_SPAN),
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
