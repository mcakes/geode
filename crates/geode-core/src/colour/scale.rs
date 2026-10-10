//! A diverging heat scale for value cells: no fill near zero, the sign's
//! token mixed over the theme background by magnitude, capped so the
//! number stays readable. Mixing runs in OKLCH between the background and
//! the token, so a light theme's fill darkens toward the token instead of
//! washing out the way an alpha blend of a saturated color over white does.

use super::oklab::{Lch, lab_to_lch, srgb_to_oklab, to_srgb_in_gamut};
use super::{Rgb, Tokens, readable_text_on};

/// Below this fraction of the domain a cell takes no fill; at it, the fill
/// starts at this fraction of the token.
pub const FLOOR: f32 = 0.06;
/// The fraction of the token the largest magnitude reaches; the remaining
/// background keeps the text readable.
pub const CAP: f32 = 0.55;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Diverging {
    /// The magnitude that reaches `CAP` — the largest |value| painted.
    pub domain: f64,
    pub positive: Rgb,
    pub negative: Rgb,
    pub background: Rgb,
}

impl Diverging {
    /// Bullish for positive, bearish for negative, over the theme surface.
    pub fn for_tokens(domain: f64, tokens: &Tokens) -> Diverging {
        Diverging {
            domain,
            positive: tokens.bullish,
            negative: tokens.bearish,
            background: tokens.background,
        }
    }

    /// The fill behind `value`, composed over the background. `None` at zero,
    /// for NaN, for a dead domain, and below `domain * FLOOR`; otherwise the
    /// sign's token at `FLOOR + (CAP - FLOOR) * |value| / domain`, clamped
    /// at `CAP`.
    pub fn fill(&self, value: f64) -> Option<Rgb> {
        let live_domain = self.domain.is_finite() && self.domain > 0.0;
        if !live_domain || !value.is_finite() {
            return None;
        }
        let ratio = (value.abs() / self.domain) as f32;
        if value == 0.0 || ratio < FLOOR {
            return None;
        }
        let t = FLOOR + (CAP - FLOOR) * ratio.min(1.0);
        let pole = if value > 0.0 {
            self.positive
        } else {
            self.negative
        };
        Some(mix_oklch(self.background, pole, t))
    }

    /// `text` as it must paint over `fill`: itself when it already reads at
    /// [`TEXT_READABLE_RATIO`](super::TEXT_READABLE_RATIO), else moved
    /// toward black or white.
    pub fn text_over(fill: Rgb, text: Rgb) -> Rgb {
        readable_text_on(text, fill)
    }
}

/// `from` moved toward `to` by `t` in OKLCH: lightness and chroma linearly,
/// hue the target's throughout (a near-grey background has no hue worth
/// keeping). Clipped to the sRGB gamut by pulling chroma in.
pub(crate) fn mix_oklch(from: Rgb, to: Rgb, t: f32) -> Rgb {
    let a = lab_to_lch(srgb_to_oklab(from));
    let b = lab_to_lch(srgb_to_oklab(to));
    to_srgb_in_gamut(Lch {
        l: a.l + (b.l - a.l) * t,
        c: a.c + (b.c - a.c) * t,
        h: b.h,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::colour::TEXT_READABLE_RATIO;
    use crate::colour::contrast_ratio;
    use crate::colour::oklab::{lab_to_lch, srgb_to_oklab};

    fn grey(v: f32) -> Rgb {
        Rgb { r: v, g: v, b: v }
    }
    const GREEN: Rgb = Rgb {
        r: 0.1,
        g: 0.8,
        b: 0.3,
    };
    const RED: Rgb = Rgb {
        r: 0.9,
        g: 0.2,
        b: 0.2,
    };

    fn dark() -> Diverging {
        Diverging {
            domain: 100.0,
            positive: GREEN,
            negative: RED,
            background: grey(0.1),
        }
    }
    fn light() -> Diverging {
        Diverging {
            background: grey(0.95),
            ..dark()
        }
    }
    fn lightness(rgb: Rgb) -> f32 {
        lab_to_lch(srgb_to_oklab(rgb)).l
    }
    fn hue(rgb: Rgb) -> f32 {
        lab_to_lch(srgb_to_oklab(rgb)).h
    }

    #[test]
    fn zero_nan_and_a_dead_domain_take_no_fill() {
        assert_eq!(dark().fill(0.0), None);
        assert_eq!(dark().fill(f64::NAN), None);
        assert_eq!(
            Diverging {
                domain: 0.0,
                ..dark()
            }
            .fill(5.0),
            None
        );
        assert_eq!(
            Diverging {
                domain: f64::NAN,
                ..dark()
            }
            .fill(5.0),
            None
        );
    }

    #[test]
    fn cells_under_the_floor_take_no_fill_and_the_largest_takes_the_cap() {
        let s = dark();
        assert_eq!(s.fill(5.9), None, "under domain * FLOOR");
        assert!(s.fill(6.0).is_some(), "at the floor");
        let at_cap = s.fill(100.0).unwrap();
        let expected = mix_oklch(s.background, s.positive, CAP);
        assert_eq!(at_cap, expected);
        assert_eq!(
            s.fill(1_000.0),
            Some(expected),
            "beyond the domain clamps at the cap"
        );
    }

    #[test]
    fn the_sign_picks_the_pole_and_magnitude_deepens_it() {
        let s = dark();
        let small = s.fill(10.0).unwrap();
        let big = s.fill(90.0).unwrap();
        let neg = s.fill(-90.0).unwrap();
        assert!(
            (hue(big) - hue(GREEN)).abs() < 0.05,
            "a positive fill keeps the positive hue"
        );
        assert!(
            (hue(neg) - hue(RED)).abs() < 0.05,
            "a negative fill keeps the negative hue"
        );
        let d_small = (lightness(small) - lightness(s.background)).abs();
        let d_big = (lightness(big) - lightness(s.background)).abs();
        assert!(
            d_big > d_small,
            "more magnitude moves further from the background"
        );
    }

    #[test]
    fn a_light_background_darkens_toward_the_token() {
        let s = light();
        let fill = s.fill(100.0).unwrap();
        assert!(
            lightness(fill) < lightness(s.background),
            "a fill on a light theme is darker than the surface, not washed out"
        );
        assert!(
            lightness(fill) > lightness(GREEN),
            "and still lighter than the token itself at the cap"
        );
    }

    #[test]
    fn a_dark_background_lightens_toward_the_token() {
        let s = dark();
        let fill = s.fill(100.0).unwrap();
        assert!(lightness(fill) > lightness(s.background));
    }

    #[test]
    fn text_over_a_fill_is_readable_on_both_themes() {
        for (s, text) in [(dark(), grey(0.9)), (light(), grey(0.1))] {
            let fill = s.fill(100.0).unwrap();
            let ink = Diverging::text_over(fill, text);
            assert!(
                contrast_ratio(ink, fill) >= TEXT_READABLE_RATIO - 0.05,
                "{ink:?} on {fill:?}"
            );
        }
        // A foreground that already reads is left alone.
        let fill = dark().fill(100.0).unwrap();
        assert_eq!(Diverging::text_over(fill, grey(1.0)), grey(1.0));
    }

    #[test]
    fn for_tokens_takes_bullish_bearish_and_the_background() {
        let tokens = Tokens {
            foreground: grey(0.9),
            muted: grey(0.6),
            primary: grey(0.5),
            accent: grey(0.4),
            danger: grey(0.3),
            warning: grey(0.35),
            success: grey(0.45),
            info: grey(0.55),
            chart: [grey(0.1); 5],
            bullish: GREEN,
            bearish: RED,
            background: grey(0.1),
        };
        let s = Diverging::for_tokens(42.0, &tokens);
        assert_eq!(
            (s.domain, s.positive, s.negative, s.background),
            (42.0, GREEN, RED, grey(0.1))
        );
    }
}
