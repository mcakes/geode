//! Convert GPUI theme colours for the pure [`geode_core::colour`] resolver.
//!
//! [`anchors_from_theme`] and [`tokens_from_theme`] read the current theme
//! without caching, so callers see theme changes on the next paint. Callers
//! resolving colours per cell can cache the pair using [`theme_signature`],
//! which includes every theme field used by either conversion.

use geode_core::colour::{Anchors, Rgb, Tokens};
use gpui::Hsla;
use gpui_component::Theme;

/// A gpui `Hsla` as the pure resolver's own `Rgb` — alpha dropped, since
/// every colour this crate resolves through the pure vocabulary is
/// opaque (a swatch, a cell, a chart series never carries transparency
/// of its own).
pub fn to_rgb(hsla: Hsla) -> Rgb {
    let c = hsla.to_rgb();
    Rgb {
        r: c.r,
        g: c.g,
        b: c.b,
    }
}

/// The reverse of [`to_rgb`]: opaque (`a: 1.0`), through gpui's own
/// `Rgba -> Hsla` conversion rather than a hand-rolled one, so this
/// crate never re-derives HSL math gpui already has.
pub fn to_hsla(rgb: Rgb) -> Hsla {
    gpui::Rgba {
        r: rgb.r,
        g: rgb.g,
        b: rgb.b,
        a: 1.0,
    }
    .into()
}

/// `top` at its own alpha composited over an opaque `under`, in sRGB —
/// what the GPU paints for a translucent fill over what is beneath it.
/// The one place that arithmetic lives, so a readability check measures
/// the colour a tinted chip or cell actually lands on rather than the
/// tint's own opaque value.
pub fn over(top: Hsla, under: Rgb) -> Rgb {
    let (t, a) = (to_rgb(top), top.a);
    Rgb {
        r: t.r * a + under.r * (1.0 - a),
        g: t.g * a + under.g * (1.0 - a),
        b: t.b * a + under.b * (1.0 - a),
    }
}

/// The theme's six base hues, red/yellow/green/cyan/blue/magenta, in
/// both tones — [`geode_core::colour::ANCHOR_DEGREES`]'s own order,
/// which every [`Anchors`] this crate builds must agree with, or a
/// `hue` colour resolves against the wrong anchor entirely.
pub fn anchors_from_theme(theme: &Theme) -> Anchors {
    Anchors {
        normal: [
            theme.red,
            theme.yellow,
            theme.green,
            theme.cyan,
            theme.blue,
            theme.magenta,
        ]
        .map(to_rgb),
        light: [
            theme.red_light,
            theme.yellow_light,
            theme.green_light,
            theme.cyan_light,
            theme.blue_light,
            theme.magenta_light,
        ]
        .map(to_rgb),
    }
}

/// Convert the theme's semantic foreground tokens and background surface.
/// `muted` uses `muted_foreground`; the theme's `muted` field is a background
/// tint. `background` supplies the surface used for generated-hue readability
/// checks, rather than a named foreground token.
pub fn tokens_from_theme(theme: &Theme) -> Tokens {
    Tokens {
        foreground: to_rgb(theme.foreground),
        muted: to_rgb(theme.muted_foreground),
        primary: to_rgb(theme.primary),
        accent: to_rgb(theme.accent),
        danger: to_rgb(theme.danger),
        warning: to_rgb(theme.warning),
        success: to_rgb(theme.success),
        info: to_rgb(theme.info),
        chart: [
            theme.chart_1,
            theme.chart_2,
            theme.chart_3,
            theme.chart_4,
            theme.chart_5,
        ]
        .map(to_rgb),
        bullish: to_rgb(theme.chart_bullish),
        bearish: to_rgb(theme.chart_bearish),
        background: to_rgb(theme.background),
    }
}

/// All inputs to [`anchors_from_theme`] and [`tokens_from_theme`], in the
/// same order: twelve anchor colours followed by sixteen token colours.
/// Equal signatures produce equal derived pairs. Comparing these `Hsla`
/// values lets callers reuse a cached pair without colour conversions.
///
/// Add any new derivation input here too. Checking only background and
/// foreground would miss changes to individual anchors or semantic tokens.
pub fn theme_signature(theme: &Theme) -> [Hsla; 28] {
    [
        // `anchors_from_theme`: normal then light, `ANCHOR_DEGREES` order.
        theme.red,
        theme.yellow,
        theme.green,
        theme.cyan,
        theme.blue,
        theme.magenta,
        theme.red_light,
        theme.yellow_light,
        theme.green_light,
        theme.cyan_light,
        theme.blue_light,
        theme.magenta_light,
        // `tokens_from_theme`, in its own field order.
        theme.foreground,
        theme.muted_foreground,
        theme.primary,
        theme.accent,
        theme.danger,
        theme.warning,
        theme.success,
        theme.info,
        theme.chart_1,
        theme.chart_2,
        theme.chart_3,
        theme.chart_4,
        theme.chart_5,
        theme.chart_bullish,
        theme.chart_bearish,
        theme.background,
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Round-tripping through `Hsla` loses a little precision (HSL <->
    /// RGB is not exact in floating point) but must stay visually
    /// identical — within a tight tolerance, not bit-for-bit.
    #[test]
    fn to_rgb_and_to_hsla_round_trip_within_tolerance() {
        let rgb = Rgb {
            r: 0.25,
            g: 0.5,
            b: 0.75,
        };
        let back = to_rgb(to_hsla(rgb));
        assert!((back.r - rgb.r).abs() < 0.01, "{back:?}");
        assert!((back.g - rgb.g).abs() < 0.01, "{back:?}");
        assert!((back.b - rgb.b).abs() < 0.01, "{back:?}");
    }
}
