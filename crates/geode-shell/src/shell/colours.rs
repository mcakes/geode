//! Bridges gpui-component's `Theme` to the pure
//! [`geode_core::colour`] resolver (Part 2c spec §6.1) — the one place a
//! gpui `Hsla` and a `geode_core::colour::Rgb` meet. `geode_core::colour`
//! itself knows no gpui type (its own module doc says so), so every
//! caller that has a real theme to resolve against — the Colours dialog's
//! swatches today, the blotter's cell colouring (Task 6) tomorrow — comes
//! through here rather than hand-rolling the conversion at its own call
//! site.
//!
//! `anchors_from_theme`/`tokens_from_theme` read the theme's own colours
//! once per resolve; nothing here caches them, because a theme swap
//! (`[theme] name` in `app.toml`) has to be visible on the very next
//! paint and a cache would need its own invalidation for no real cost —
//! six-plus-eleven `Hsla` field reads is not a hot path. A caller on a
//! path where it IS one — the blotter paints `render_td` per visible
//! cell — memoises the derived pair itself behind [`theme_signature`],
//! which is the exact input to both derivations and so needs no
//! invalidation of its own either.

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

/// The theme's semantic tokens, spec §2.3's table: `muted` reads the
/// theme's `muted_foreground` (there is no bare `muted` colour on
/// gpui-component's own `Theme` — its `muted` is a background tint, not
/// a foreground token, and §2.3 names the foreground one). `background`
/// is not one of §2.3's named tokens — it is the surface
/// `geode_core::colour::readable_on` measures every generated hue
/// against (2c §2.2, §7).
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

/// Every theme colour [`anchors_from_theme`] and [`tokens_from_theme`]
/// read, in one array — the exact input to both derivations, so two
/// signatures comparing equal mean two identical derived pairs (Part 2c
/// final review, I-1).
///
/// Twelve anchors then sixteen token colours, in each function's own
/// field order. It is a read of twenty-eight `Hsla` fields and no
/// arithmetic: `Hsla` is `Copy` with a hand-written `PartialEq`, so a
/// caller that memoises its derived pair behind this pays 28 copies and
/// 28 compares on the steady path and **zero** `Hsla -> Rgb`
/// conversions, which is what spec §6.3's "one comparison a frame" asks
/// for.
///
/// **A colour added to either derivation must be added here too**, or a
/// theme that moves only that colour compares equal and the memo stays
/// stale. The whole point of the full signature (over the two-sentinel
/// sketch the ledger first carried) is that it is exact: a memo behind
/// `background` + `foreground` alone would keep painting the old colour
/// through any theme change that leaves those two equal while moving an
/// anchor, and the blotter's `ColourCache` could never notice, because
/// the stale derived pair IS its own key.
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
