//! Pure sRGB, OKLab, and OKLCH conversions using Björn Ottosson's matrices.
//! Named colours interpolate along OKLCH hue arcs to retain lightness and
//! chroma, avoiding the chroma loss of a straight chord through OKLab.

use super::Rgb;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Lab {
    pub l: f32,
    pub a: f32,
    pub b: f32,
}

/// Hue in radians, `0..TAU`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Lch {
    pub l: f32,
    pub c: f32,
    pub h: f32,
}

fn to_linear(c: f32) -> f32 {
    if c <= 0.04045 {
        c / 12.92
    } else {
        ((c + 0.055) / 1.055).powf(2.4)
    }
}

fn to_srgb_channel(l: f32) -> f32 {
    if l <= 0.0031308 {
        12.92 * l
    } else {
        1.055 * l.powf(1.0 / 2.4) - 0.055
    }
}

// Ottosson's published matrices, transcribed to full double precision on
// purpose — truncating a digit to satisfy `excessive_precision` is
// exactly the kind of edit that would silently drift these constants
// off the reference and defeat `oklab_matches_the_reference_values`.
#[allow(clippy::excessive_precision)]
pub fn srgb_to_oklab(rgb: Rgb) -> Lab {
    let (r, g, b) = (to_linear(rgb.r), to_linear(rgb.g), to_linear(rgb.b));
    let l = 0.412_221_470_8 * r + 0.536_332_536_3 * g + 0.051_445_992_9 * b;
    let m = 0.211_903_498_2 * r + 0.680_699_545_1 * g + 0.107_396_956_6 * b;
    let s = 0.088_302_461_9 * r + 0.281_718_837_6 * g + 0.629_978_700_5 * b;
    let (l_, m_, s_) = (l.cbrt(), m.cbrt(), s.cbrt());
    Lab {
        l: 0.210_454_255_3 * l_ + 0.793_617_785_0 * m_ - 0.004_072_046_8 * s_,
        a: 1.977_998_495_1 * l_ - 2.428_592_205_0 * m_ + 0.450_593_709_9 * s_,
        b: 0.025_904_037_1 * l_ + 0.782_771_766_2 * m_ - 0.808_675_766_0 * s_,
    }
}

/// Unclipped: a saturated OKLab colour can land outside `0..=1`.
#[allow(clippy::excessive_precision)]
pub fn oklab_to_srgb(lab: Lab) -> Rgb {
    let l_ = lab.l + 0.396_337_777_4 * lab.a + 0.215_803_757_3 * lab.b;
    let m_ = lab.l - 0.105_561_345_8 * lab.a - 0.063_854_172_8 * lab.b;
    let s_ = lab.l - 0.089_484_177_5 * lab.a - 1.291_485_548_0 * lab.b;
    let (l, m, s) = (l_ * l_ * l_, m_ * m_ * m_, s_ * s_ * s_);
    let r = 4.076_741_662_1 * l - 3.307_711_591_3 * m + 0.230_969_929_2 * s;
    let g = -1.268_438_004_6 * l + 2.609_757_401_1 * m - 0.341_319_396_5 * s;
    let b = -0.004_196_086_3 * l - 0.703_418_614_7 * m + 1.707_614_701_0 * s;
    Rgb {
        r: to_srgb_channel(r),
        g: to_srgb_channel(g),
        b: to_srgb_channel(b),
    }
}

pub fn lab_to_lch(lab: Lab) -> Lch {
    let c = (lab.a * lab.a + lab.b * lab.b).sqrt();
    let h = lab.b.atan2(lab.a).rem_euclid(std::f32::consts::TAU);
    Lch { l: lab.l, c, h }
}

pub fn lch_to_lab(lch: Lch) -> Lab {
    Lab {
        l: lch.l,
        a: lch.c * lch.h.cos(),
        b: lch.c * lch.h.sin(),
    }
}

fn in_gamut(rgb: Rgb) -> bool {
    let ok = |c: f32| (-0.0005..=1.0005).contains(&c);
    ok(rgb.r) && ok(rgb.g) && ok(rgb.b)
}

fn clamp01(rgb: Rgb) -> Rgb {
    Rgb {
        r: rgb.r.clamp(0.0, 1.0),
        g: rgb.g.clamp(0.0, 1.0),
        b: rgb.b.clamp(0.0, 1.0),
    }
}

/// Convert, and when the colour has no sRGB counterpart pull its chroma
/// in (a bisection over `0..=c`, sixteen steps) until it has one. Chroma,
/// never lightness or hue: the trader asked for a hue at a lightness, and
/// a channel clamp would shift both.
pub fn to_srgb_in_gamut(lch: Lch) -> Rgb {
    let direct = oklab_to_srgb(lch_to_lab(lch));
    if in_gamut(direct) {
        return clamp01(direct);
    }
    let (mut lo, mut hi) = (0.0_f32, lch.c);
    for _ in 0..16 {
        let mid = (lo + hi) / 2.0;
        if in_gamut(oklab_to_srgb(lch_to_lab(Lch { c: mid, ..lch }))) {
            lo = mid
        } else {
            hi = mid
        }
    }
    clamp01(oklab_to_srgb(lch_to_lab(Lch { c: lo, ..lch })))
}

/// WCAG relative luminance of an sRGB colour.
pub fn relative_luminance(rgb: Rgb) -> f32 {
    0.2126 * to_linear(rgb.r) + 0.7152 * to_linear(rgb.g) + 0.0722 * to_linear(rgb.b)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: f32, b: f32) -> bool {
        (a - b).abs() < 2e-3
    }

    /// Björn Ottosson's published reference values for OKLab.
    #[test]
    fn oklab_matches_the_reference_values() {
        let white = srgb_to_oklab(Rgb {
            r: 1.0,
            g: 1.0,
            b: 1.0,
        });
        assert!(
            close(white.l, 1.0) && close(white.a, 0.0) && close(white.b, 0.0),
            "{white:?}"
        );
        let red = srgb_to_oklab(Rgb {
            r: 1.0,
            g: 0.0,
            b: 0.0,
        });
        assert!(
            close(red.l, 0.6280) && close(red.a, 0.2249) && close(red.b, 0.1258),
            "{red:?}"
        );
        let green = srgb_to_oklab(Rgb {
            r: 0.0,
            g: 1.0,
            b: 0.0,
        });
        assert!(
            close(green.l, 0.8664) && close(green.a, -0.2339) && close(green.b, 0.1795),
            "{green:?}"
        );
        let blue = srgb_to_oklab(Rgb {
            r: 0.0,
            g: 0.0,
            b: 1.0,
        });
        assert!(
            close(blue.l, 0.4520) && close(blue.a, -0.0325) && close(blue.b, -0.3115),
            "{blue:?}"
        );
    }

    #[test]
    fn oklab_round_trips_a_grid_of_srgb_values() {
        for r in 0..=4 {
            for g in 0..=4 {
                for b in 0..=4 {
                    let rgb = Rgb {
                        r: r as f32 / 4.0,
                        g: g as f32 / 4.0,
                        b: b as f32 / 4.0,
                    };
                    let back = oklab_to_srgb(srgb_to_oklab(rgb));
                    assert!(
                        close(back.r, rgb.r) && close(back.g, rgb.g) && close(back.b, rgb.b),
                        "{rgb:?} -> {back:?}"
                    );
                }
            }
        }
    }

    #[test]
    fn lch_round_trips_and_keeps_hue_in_range() {
        let lab = srgb_to_oklab(Rgb {
            r: 0.2,
            g: 0.4,
            b: 0.9,
        });
        let lch = lab_to_lch(lab);
        assert!((0.0..std::f32::consts::TAU).contains(&lch.h));
        let back = lch_to_lab(lch);
        assert!(close(back.a, lab.a) && close(back.b, lab.b));
    }

    /// A very saturated OKLCH colour has no sRGB counterpart; clipping
    /// pulls chroma, never lightness or hue.
    #[test]
    fn gamut_clip_pulls_chroma_and_keeps_lightness_and_hue() {
        let wild = Lch {
            l: 0.7,
            c: 0.5,
            h: 0.5,
        };
        let rgb = to_srgb_in_gamut(wild);
        assert!(
            (0.0..=1.0).contains(&rgb.r)
                && (0.0..=1.0).contains(&rgb.g)
                && (0.0..=1.0).contains(&rgb.b),
            "{rgb:?}"
        );
        let got = lab_to_lch(srgb_to_oklab(rgb));
        assert!(close(got.l, 0.7), "lightness kept: {got:?}");
        assert!((got.h - 0.5).abs() < 0.02, "hue kept: {got:?}");
        assert!(got.c < 0.5, "chroma pulled in: {got:?}");
    }

    #[test]
    fn relative_luminance_is_wcag() {
        assert!(close(
            relative_luminance(Rgb {
                r: 1.0,
                g: 1.0,
                b: 1.0
            }),
            1.0
        ));
        assert!(close(
            relative_luminance(Rgb {
                r: 0.0,
                g: 0.0,
                b: 0.0
            }),
            0.0
        ));
        assert!(close(
            relative_luminance(Rgb {
                r: 1.0,
                g: 0.0,
                b: 0.0
            }),
            0.2126
        ));
    }
}
