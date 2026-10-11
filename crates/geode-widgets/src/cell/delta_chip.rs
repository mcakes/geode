//! The change chip: the signed number on a fill in its sign's color whose
//! alpha grows with the move's size relative to the column's largest move,
//! up to a host ceiling.

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
/// fill alpha capped at `ceiling` (clamped to `[0, 1]`). `None` for a
/// non-finite value: the host paints its text alone. A zero, or a zero or
/// non-finite scale, has no fill.
pub fn chip(value: f64, scale: f64, ceiling: f32) -> Option<Chip> {
    if !value.is_finite() {
        return None;
    }
    let sign = if value > 0.0 {
        Sign::Positive
    } else if value < 0.0 {
        Sign::Negative
    } else {
        Sign::Zero
    };
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
}

/// Minimum chip width at the design rem (the mockup's 44 px at a 13 px
/// base), so short and long numbers line up as one column of chips.
const MIN_WIDTH: f32 = 40.0;
/// Horizontal text inset at the design rem.
const INSET: f32 = 4.0;

/// The chip, right-aligned text inside. `text` is the host's formatted
/// number; with no chip it is painted in the plain text color.
pub fn paint(text: SharedString, chip: Option<Chip>, paint: ChipPaint, id: SharedString) -> Div {
    let text_sel = id.clone();
    let Some(chip) = chip else {
        return div().child(
            div()
                .text_color(paint.text)
                .debug_selector(move || format!("{text_sel}-text"))
                .child(text),
        );
    };
    let color = paint.color(chip.sign);
    div().child(
        div()
            .min_w(design(MIN_WIDTH))
            .px(design(INSET))
            .rounded(paint.radius)
            .flex()
            .justify_end()
            .when(chip.alpha > 0.0, |d| d.bg(color.opacity(chip.alpha)))
            .debug_selector(move || format!("{id}-chip"))
            .child(
                div()
                    .text_color(color)
                    .debug_selector(move || format!("{text_sel}-text"))
                    .child(text),
            ),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::{
        AppContext as _, IntoElement, TestAppContext, VisualTestContext, Window, div, hsla, px,
    };

    #[test]
    fn alpha_is_magnitude_over_the_scale_up_to_the_ceiling() {
        let c = chip(1.1, 2.2, 0.55).unwrap();
        assert_eq!(c.sign, Sign::Positive);
        assert!((c.alpha - 0.275).abs() < 1e-6);
        let c = chip(-2.2, 2.2, 0.55).unwrap();
        assert_eq!(c.sign, Sign::Negative);
        assert!((c.alpha - 0.55).abs() < 1e-6);
    }

    #[test]
    fn a_move_beyond_the_scale_clamps_at_the_ceiling() {
        assert!((chip(5.0, 2.0, 0.55).unwrap().alpha - 0.55).abs() < 1e-6);
        assert!((chip(-5.0, 2.0, 0.4).unwrap().alpha - 0.4).abs() < 1e-6);
    }

    #[test]
    fn a_zero_scale_tints_nothing() {
        let c = chip(0.0, 0.0, 0.55).unwrap();
        assert_eq!(
            c,
            Chip {
                sign: Sign::Zero,
                alpha: 0.0
            }
        );
        let c = chip(0.3, 0.0, 0.55).unwrap();
        assert_eq!(c.alpha, 0.0, "no scale: sign kept, no fill");
        assert_eq!(c.sign, Sign::Positive);
    }

    #[test]
    fn zero_is_its_own_sign_with_no_fill() {
        assert_eq!(
            chip(0.0, 3.0, 0.55),
            Some(Chip {
                sign: Sign::Zero,
                alpha: 0.0
            })
        );
        assert_eq!(
            chip(-0.0, 3.0, 0.55),
            Some(Chip {
                sign: Sign::Zero,
                alpha: 0.0
            })
        );
    }

    #[test]
    fn a_non_finite_value_has_no_chip() {
        assert_eq!(chip(f64::NAN, 3.0, 0.55), None);
        assert_eq!(chip(f64::INFINITY, 3.0, 0.55), None);
        assert_eq!(
            chip(1.0, f64::NAN, 0.55).map(|c| c.alpha),
            Some(0.0),
            "a bad scale tints nothing"
        );
    }

    #[test]
    fn a_negative_or_oversized_ceiling_is_clamped_to_zero_and_one() {
        assert_eq!(chip(1.0, 1.0, -0.5).unwrap().alpha, 0.0);
        assert_eq!(chip(1.0, 1.0, 3.0).unwrap().alpha, 1.0);
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
        let mut vcx = drawn(cx, chip(1.1, 2.2, 0.55));
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
