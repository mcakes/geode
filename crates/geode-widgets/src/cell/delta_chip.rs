//! The change chip: the signed number in its sign's color on a tint of the
//! same color whose alpha grows with the move's size relative to the
//! column's largest move, up to a host ceiling. The number carries the
//! sign; the tint only says how big.

use geode_core::format::Sign;
use gpui::prelude::FluentBuilder as _;
use gpui::{Div, Hsla, InteractiveElement as _, ParentElement, Pixels, SharedString, Styled, div};

use super::design;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Chip {
    pub sign: Sign,
    /// Fill alpha in `[0, ceiling]`.
    pub alpha: f32,
}

/// The chip for `value` against the column `scale` (`column_scale`), with
/// fill alpha capped at `ceiling` (clamped to `[0, 1]`). `sign` is the sign
/// of the number as the host displays it (`geode_core::format::Formatted::sign`,
/// which follows the rounded value), so a change shown as `0.0` gets no tint
/// and the zero color whatever its unrounded value. `None` for a non-finite
/// value: the host paints its text alone. A zero sign, or a zero or
/// non-finite scale, has no fill.
pub fn chip(value: f64, sign: Sign, scale: f64, ceiling: f32) -> Option<Chip> {
    if !value.is_finite() {
        return None;
    }
    let ceiling = ceiling.clamp(0.0, 1.0);
    let alpha = if scale.is_finite() && scale > 0.0 && sign != Sign::Zero {
        ((value.abs() / scale).min(1.0) as f32) * ceiling
    } else {
        0.0
    };
    Some(Chip { sign, alpha })
}

/// Colors the host resolves once per theme: the sign colors for text and
/// fill (the fill is the sign color at the chip's alpha), the plain text
/// color for a value with no chip, and the theme's small radius.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ChipPaint {
    pub positive: Hsla,
    pub negative: Hsla,
    pub zero: Hsla,
    /// The color of a value with no chip: a missing or non-finite one. A
    /// signed number never takes it; see [`ChipPaint::text_color`].
    pub text: Hsla,
    pub radius: Pixels,
}

impl ChipPaint {
    fn color(&self, sign: Sign) -> Hsla {
        match sign {
            Sign::Positive => self.positive,
            Sign::Negative => self.negative,
            Sign::Zero => self.zero,
        }
    }

    /// The text color for a displayed `sign`: always the sign's color, so a
    /// chip and a plain signed number read alike; the fill only says how big.
    pub fn text_color(&self, sign: Sign) -> Hsla {
        self.color(sign)
    }
}

/// Minimum chip width at the design rem (the mockup's 44 px at a 13 px
/// base), so short and long numbers line up as one column of chips.
pub(crate) const MIN_WIDTH: f32 = super::widths::CHIP_MIN;
/// Horizontal text inset at the design rem.
pub(crate) const INSET: f32 = super::widths::CHIP_INSET;

/// The chip, right-aligned text inside. `text` is the host's formatted
/// number, painted in its sign's color ([`ChipPaint::text_color`]) over a
/// fill of that color at the chip's alpha; an alpha of zero draws no fill.
/// With no chip (a missing or non-finite value) the text is painted alone,
/// in `ChipPaint::text`, and carries the chip's inset so it lines up.
pub fn paint(text: SharedString, chip: Option<Chip>, paint: ChipPaint, id: SharedString) -> Div {
    let text_sel = id.clone();
    // The wrapper right-aligns the chip, so it keeps its own width in any
    // host; a missing value carries the chip's text inset so its `—` lines
    // up with the numbers above and below it.
    let Some(chip) = chip else {
        return div().flex().justify_end().child(
            div().px(design(INSET)).child(
                div()
                    .text_color(paint.text)
                    .debug_selector(move || format!("{text_sel}-text"))
                    .child(text),
            ),
        );
    };
    let fill = paint.color(chip.sign);
    let text_color = paint.text_color(chip.sign);
    div().flex().justify_end().child(
        div()
            .flex_none()
            .min_w(design(MIN_WIDTH))
            .px(design(INSET))
            .rounded(paint.radius)
            .flex()
            .justify_end()
            .when(chip.alpha > 0.0, |d| d.bg(fill.opacity(chip.alpha)))
            .debug_selector(move || format!("{id}-chip"))
            .child(
                div()
                    .text_color(text_color)
                    .debug_selector(move || format!("{text_sel}-text"))
                    .child(text),
            ),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The sign the host would display for an exactly representable value.
    fn sgn(v: f64) -> Sign {
        if v > 0.0 {
            Sign::Positive
        } else if v < 0.0 {
            Sign::Negative
        } else {
            Sign::Zero
        }
    }
    use gpui::{
        AppContext as _, IntoElement, TestAppContext, VisualTestContext, Window, div, hsla, px,
    };

    #[test]
    fn alpha_is_magnitude_over_the_scale_up_to_the_ceiling() {
        let c = chip(1.1, sgn(1.1), 2.2, 0.55).unwrap();
        assert_eq!(c.sign, Sign::Positive);
        assert!((c.alpha - 0.275).abs() < 1e-6);
        let c = chip(-2.2, sgn(-2.2), 2.2, 0.55).unwrap();
        assert_eq!(c.sign, Sign::Negative);
        assert!((c.alpha - 0.55).abs() < 1e-6);
    }

    #[test]
    fn a_move_beyond_the_scale_clamps_at_the_ceiling() {
        assert!((chip(5.0, sgn(5.0), 2.0, 0.55).unwrap().alpha - 0.55).abs() < 1e-6);
        assert!((chip(-5.0, sgn(-5.0), 2.0, 0.4).unwrap().alpha - 0.4).abs() < 1e-6);
    }

    #[test]
    fn a_zero_scale_tints_nothing() {
        let c = chip(0.0, sgn(0.0), 0.0, 0.55).unwrap();
        assert_eq!(
            c,
            Chip {
                sign: Sign::Zero,
                alpha: 0.0
            }
        );
        let c = chip(0.3, sgn(0.3), 0.0, 0.55).unwrap();
        assert_eq!(c.alpha, 0.0, "no scale: sign kept, no fill");
        assert_eq!(c.sign, Sign::Positive);
    }

    #[test]
    fn zero_is_its_own_sign_with_no_fill() {
        assert_eq!(
            chip(0.0, sgn(0.0), 3.0, 0.55),
            Some(Chip {
                sign: Sign::Zero,
                alpha: 0.0
            })
        );
        assert_eq!(
            chip(-0.0, sgn(-0.0), 3.0, 0.55),
            Some(Chip {
                sign: Sign::Zero,
                alpha: 0.0
            })
        );
    }

    #[test]
    fn a_non_finite_value_has_no_chip() {
        assert_eq!(chip(f64::NAN, sgn(f64::NAN), 3.0, 0.55), None);
        assert_eq!(chip(f64::INFINITY, sgn(f64::INFINITY), 3.0, 0.55), None);
        assert_eq!(
            chip(1.0, sgn(1.0), f64::NAN, 0.55).map(|c| c.alpha),
            Some(0.0),
            "a bad scale tints nothing"
        );
    }

    #[test]
    fn a_negative_or_oversized_ceiling_is_clamped_to_zero_and_one() {
        assert_eq!(chip(1.0, sgn(1.0), 1.0, -0.5).unwrap().alpha, 0.0);
        assert_eq!(chip(1.0, sgn(1.0), 1.0, 3.0).unwrap().alpha, 1.0);
    }

    fn test_paint() -> ChipPaint {
        ChipPaint {
            positive: hsla(0.47, 0.6, 0.4, 1.0),
            negative: hsla(0.0, 0.8, 0.6, 1.0),
            zero: hsla(0.0, 0.0, 0.6, 1.0),
            text: hsla(0.0, 0.0, 0.9, 1.0),
            radius: px(3.0),
        }
    }

    #[test]
    fn text_is_the_sign_color_with_or_without_a_fill() {
        let p = test_paint();
        assert_eq!(p.text_color(Sign::Positive), p.positive);
        assert_eq!(p.text_color(Sign::Negative), p.negative);
        assert_eq!(p.text_color(Sign::Zero), p.zero);
        assert_ne!(
            p.text_color(Sign::Positive),
            p.text,
            "the plain text color is for a value with no chip"
        );
    }

    struct Probe(Option<Chip>);
    impl gpui::Render for Probe {
        fn render(&mut self, _w: &mut Window, _cx: &mut gpui::Context<Self>) -> impl IntoElement {
            div().w(px(120.0)).h(px(20.0)).child(paint(
                "+1.1".into(),
                self.0,
                test_paint(),
                "probe".into(),
            ))
        }
    }

    fn drawn(cx: &mut TestAppContext, chip: Option<Chip>) -> VisualTestContext {
        let window = cx
            .update(|cx| {
                cx.open_window(gpui::WindowOptions::default(), |_w, cx| {
                    cx.new(|_| Probe(chip))
                })
            })
            .unwrap();
        let mut vcx = VisualTestContext::from_window(window.into(), cx);
        vcx.update(|w, cx| {
            let _ = w.draw(cx);
        });
        vcx
    }

    #[gpui::test]
    fn the_chip_paints_its_text_and_a_fill_only_when_tinted(cx: &mut TestAppContext) {
        let mut vcx = drawn(cx, chip(1.1, sgn(1.1), 2.2, 0.55));
        assert!(
            vcx.debug_bounds("probe-chip").is_some(),
            "the chip with its number is painted"
        );
        assert!(vcx.debug_bounds("probe-text").is_some());
        let mut vcx = drawn(cx, None);
        assert!(
            vcx.debug_bounds("probe-chip").is_none(),
            "no chip for a missing value"
        );
        assert!(
            vcx.debug_bounds("probe-text").is_some(),
            "a missing value still paints its text"
        );
    }
}

#[cfg(test)]
mod column_tests {
    use super::*;
    use gpui::{
        AppContext as _, IntoElement, TestAppContext, VisualTestContext, Window, div, hsla, px,
    };

    fn p() -> ChipPaint {
        ChipPaint {
            positive: hsla(0.47, 0.6, 0.4, 1.0),
            negative: hsla(0.0, 0.8, 0.6, 1.0),
            zero: hsla(0.0, 0.0, 0.6, 1.0),
            text: hsla(0.0, 0.0, 0.9, 1.0),
            radius: px(3.0),
        }
    }

    struct Column;
    impl gpui::Render for Column {
        fn render(&mut self, _w: &mut Window, _cx: &mut gpui::Context<Self>) -> impl IntoElement {
            let row = |text: &'static str, c: Option<Chip>, id: &'static str| {
                div()
                    .w(px(160.0))
                    .flex()
                    .justify_end()
                    .child(paint(text.into(), c, p(), id.into()))
            };
            div()
                .flex()
                .flex_col()
                .child(row("+1.1", chip(1.1, Sign::Positive, 2.2, 0.55), "r0"))
                .child(row(crate::cell::widths::DASH, None, "r1"))
                // A block host: the chip keeps its own width, not the cell's.
                .child(div().w(px(160.0)).child(paint(
                    "+1.1".into(),
                    chip(1.1, Sign::Positive, 2.2, 0.55),
                    p(),
                    "blk".into(),
                )))
        }
    }

    fn drawn(cx: &mut TestAppContext) -> VisualTestContext {
        let window = cx
            .update(|cx| {
                cx.open_window(gpui::WindowOptions::default(), |_w, cx| cx.new(|_| Column))
            })
            .unwrap();
        let mut vcx = VisualTestContext::from_window(window.into(), cx);
        vcx.update(|w, cx| {
            let _ = w.draw(cx);
        });
        vcx
    }

    #[gpui::test]
    fn a_missing_value_lines_up_with_the_chips_numbers(cx: &mut TestAppContext) {
        let mut vcx = drawn(cx);
        let a = vcx.debug_bounds("r0-text").expect("chip text");
        let b = vcx.debug_bounds("r1-text").expect("missing text");
        let right = |r: gpui::Bounds<gpui::Pixels>| r.origin.x.as_f32() + r.size.width.as_f32();
        assert!((right(a) - right(b)).abs() < 0.5, "right edges {a:?} {b:?}");
    }

    #[gpui::test]
    fn the_chip_keeps_its_own_width_in_a_block_cell(cx: &mut TestAppContext) {
        let mut vcx = drawn(cx);
        let c = vcx.debug_bounds("blk-chip").expect("chip");
        assert!(
            c.size.width.as_f32() < 100.0,
            "the chip did not stretch to the 160 px cell: {c:?}"
        );
    }

    #[test]
    fn the_displayed_sign_decides_the_color_and_a_displayed_zero_has_no_fill() {
        // 0.04 displayed at one decimal reads 0.0: no tint behind a zero.
        assert_eq!(
            chip(0.04, Sign::Zero, 2.0, 0.55),
            Some(Chip {
                sign: Sign::Zero,
                alpha: 0.0
            })
        );
        let c = chip(-0.4, Sign::Negative, 2.0, 0.55).unwrap();
        assert_eq!(c.sign, Sign::Negative);
        assert!((c.alpha - 0.11).abs() < 1e-6);
    }
}
