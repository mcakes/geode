//! Series colors from the theme. [`Palette`] is the theme's five chart
//! colors, cycled; [`HuePalette`] is an unbounded sequence whose neighbours
//! sit a golden angle apart in hue, each with a paler companion. Both pass
//! every color through [`readable_on`] against the background.

use std::f32::consts::TAU;

use geode_core::colour::oklab::{Lch, lab_to_lch, srgb_to_oklab, to_srgb_in_gamut};
use geode_core::colour::{READABLE_RATIO, Rgb, contrast_ratio, readable_on};
use gpui::Hsla;

/// Cyclic assignment of five theme chart colours to series slots.
/// Colours below the 3:1 contrast target move toward the foreground's
/// lightness through [`readable_on`]. If that lightness path cannot meet the
/// target, its endpoint is returned without a contrast guarantee.
pub struct Palette {
    colours: [Hsla; 5],
}

/// Convert a GPUI colour to the RGB value used by contrast calculations.
pub fn to_rgb(hsla: Hsla) -> Rgb {
    let c = hsla.to_rgb();
    Rgb {
        r: c.r,
        g: c.g,
        b: c.b,
    }
}

/// Convert an RGB value to an opaque GPUI colour.
pub fn to_hsla(rgb: Rgb) -> Hsla {
    gpui::Rgba {
        r: rgb.r,
        g: rgb.g,
        b: rgb.b,
        a: 1.0,
    }
    .into()
}

impl Palette {
    pub const LEN: usize = 5;

    pub fn from_theme(chart: [Hsla; 5], background: Hsla, foreground: Hsla) -> Self {
        let bg = to_rgb(background);
        let fg = to_rgb(foreground);
        Self {
            colours: chart.map(|c| to_hsla(readable_on(to_rgb(c), bg, fg))),
        }
    }

    /// The colour for the `index`-th slot, cycling.
    pub fn colour(&self, index: usize) -> Hsla {
        self.colours[index % Self::LEN]
    }
}

/// The golden angle in degrees, `360 (1 - 1/phi)`: a step no whole number
/// of turns returns to, so a hue sequence stepping by it never repeats, and
/// each new hue lands in the widest gap the earlier ones leave.
pub const GOLDEN_ANGLE: f32 = 137.507_76;

/// The chroma a [`HuePalette`] keeps at least, in OKLCH: below it hues a
/// golden angle apart stop reading as different colors.
pub const MIN_CHROMA: f32 = 0.10;

/// How far a companion's OKLCH lightness moves from its color.
pub const COMPANION_LIGHTNESS: f32 = 0.14;

/// The share of its color's chroma a companion keeps.
pub const COMPANION_CHROMA: f32 = 0.45;

/// An unbounded sequence of series colors for series that sit side by side
/// in position, such as expiries in date order: index `i` has the OKLCH hue
/// of the theme's first chart color plus `i` golden angles, at the mean
/// lightness and chroma of the theme's five chart colors (chroma at least
/// [`MIN_CHROMA`], clipped to the display gamut per hue). Neighbouring
/// indices are therefore about 137.5° apart in hue and no two indices share
/// one. Indices 8, 13 and 21 apart (Fibonacci numbers) come closest, about
/// 20°, 12° and 8° apart.
///
/// Each color passes through [`readable_on`] like [`Palette`]'s, so it
/// meets [`READABLE_RATIO`] against the background wherever the lightness
/// path toward the foreground reaches it; otherwise the path's endpoint is
/// returned without a contrast guarantee.
///
/// [`HuePalette::companion`] is the same hue at [`COMPANION_CHROMA`] of the
/// chroma and [`COMPANION_LIGHTNESS`] away in lightness: toward the
/// background (paler) when that still meets the ratio, toward the
/// foreground otherwise, and then through [`readable_on`] too. It reads as
/// the same series, plainly told apart from its color.
///
/// Each call converts and bisects; callers prepare colors when the theme
/// or the indices change, never per frame.
#[derive(Clone, Debug, PartialEq)]
pub struct HuePalette {
    /// Radians.
    base_hue: f32,
    lightness: f32,
    chroma: f32,
    background: Rgb,
    foreground: Rgb,
}

impl HuePalette {
    pub fn from_theme(chart: [Hsla; 5], background: Hsla, foreground: Hsla) -> Self {
        let lch = chart.map(|c| lab_to_lch(srgb_to_oklab(to_rgb(c))));
        let n = lch.len() as f32;
        Self {
            base_hue: lch[0].h,
            lightness: lch.iter().map(|c| c.l).sum::<f32>() / n,
            chroma: (lch.iter().map(|c| c.c).sum::<f32>() / n).max(MIN_CHROMA),
            background: to_rgb(background),
            foreground: to_rgb(foreground),
        }
    }

    /// The hue of `index`, in radians.
    fn hue(&self, index: usize) -> f32 {
        // Reduced per step so a large index keeps its precision.
        let turns = (index as f64 * GOLDEN_ANGLE as f64).rem_euclid(360.0) as f32;
        (self.base_hue + turns.to_radians()).rem_euclid(TAU)
    }

    fn readable(&self, rgb: Rgb) -> Rgb {
        readable_on(rgb, self.background, self.foreground)
    }

    fn full(&self, index: usize) -> Rgb {
        self.readable(to_srgb_in_gamut(Lch {
            l: self.lightness,
            c: self.chroma,
            h: self.hue(index),
        }))
    }

    /// The color for the `index`-th series.
    pub fn color(&self, index: usize) -> Hsla {
        to_hsla(self.full(index))
    }

    /// The paler companion of [`HuePalette::color`] for the same index.
    pub fn companion(&self, index: usize) -> Hsla {
        let full = lab_to_lch(srgb_to_oklab(self.full(index)));
        let toward = |target: Rgb| {
            let target_l = lab_to_lch(srgb_to_oklab(target)).l;
            let step = COMPANION_LIGHTNESS.copysign(target_l - full.l);
            to_srgb_in_gamut(Lch {
                l: (full.l + step).clamp(0.0, 1.0),
                c: full.c * COMPANION_CHROMA,
                h: full.h,
            })
        };
        let pale = toward(self.background);
        let rgb = if contrast_ratio(pale, self.background) >= READABLE_RATIO {
            pale
        } else {
            self.readable(toward(self.foreground))
        };
        to_hsla(rgb)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::{hsla, rgb};

    #[test]
    fn a_faint_chart_colour_is_floored_and_a_clear_one_kept() {
        let bg = hsla(0.0, 0.0, 1.0, 1.0); // white
        let fg = hsla(0.0, 0.0, 0.0, 1.0);
        let faint: Hsla = rgb(0xffff66).into(); // pale yellow on white: ~1.2:1
        let clear: Hsla = rgb(0x1f3a93).into();
        let p = Palette::from_theme([faint, clear, faint, clear, faint], bg, fg);
        assert!(contrast_ratio(to_rgb(p.colour(0)), to_rgb(bg)) >= READABLE_RATIO);
        assert_eq!(p.colour(1), clear, "a clearing colour is untouched");
        assert_eq!(p.colour(5), p.colour(0), "cycles");
        assert_eq!(p.colour(7), p.colour(2));
    }

    #[gpui::test]
    fn every_bundled_themes_palette_is_readable_and_separated(cx: &mut gpui::TestAppContext) {
        use gpui_component::{ActiveTheme, Theme};
        cx.update(gpui_component::init);
        let (service, _) = geode_shell::theme::load_bundled();
        let mut failures = Vec::new();
        let mut checked = 0;
        for name in service.names() {
            let entry = service.resolve(&name).unwrap().clone();
            // Missing or misspelled chart keys silently become a blue ramp
            // upstream. Every Geode variant must carry its reviewed palette.
            assert!(
                [
                    &entry.colors.chart_1,
                    &entry.colors.chart_2,
                    &entry.colors.chart_3,
                    &entry.colors.chart_4,
                    &entry.colors.chart_5,
                ]
                .iter()
                .all(|color| color.is_some()),
                "{name}: missing chart colors"
            );
            cx.update(|cx| {
                Theme::global_mut(cx).apply_config(&entry);
                let t = cx.theme();
                let p = Palette::from_theme(
                    [t.chart_1, t.chart_2, t.chart_3, t.chart_4, t.chart_5],
                    t.background,
                    t.foreground,
                );
                for i in 0..Palette::LEN {
                    checked += 1;
                    let ratio = contrast_ratio(to_rgb(p.colour(i)), to_rgb(t.background));
                    if ratio < READABLE_RATIO {
                        failures.push(format!("{name}: chart_{} at {ratio:.2}:1", i + 1));
                    }
                    // Local screening floor chosen with the reviewed palette
                    // gallery, not an accessibility standard. Measure every
                    // pair AFTER contrast correction, which can collapse two
                    // initially distinct colors onto the same lightness.
                    let a = geode_core::colour::oklab::srgb_to_oklab(to_rgb(p.colour(i)));
                    for j in i + 1..Palette::LEN {
                        let b = geode_core::colour::oklab::srgb_to_oklab(to_rgb(p.colour(j)));
                        let distance =
                            ((a.l - b.l).powi(2) + (a.a - b.a).powi(2) + (a.b - b.b).powi(2))
                                .sqrt();
                        if distance < 0.07 {
                            failures.push(format!(
                                "{name}: chart_{} and chart_{} only {distance:.3} apart",
                                i + 1,
                                j + 1
                            ));
                        }
                    }
                }
            });
        }
        assert!(
            checked >= 5 * 40,
            "the sweep saw {checked} checks — bundled themes missing?"
        );
        assert!(failures.is_empty(), "{failures:#?}");
    }

    /// The shorter angle between two colors' OKLCH hues, in degrees.
    fn hue_distance(a: Hsla, b: Hsla) -> f32 {
        use std::f32::consts::PI;
        let h = |c: Hsla| lab_to_lch(srgb_to_oklab(to_rgb(c))).h;
        ((h(b) - h(a) + PI).rem_euclid(TAU) - PI).abs().to_degrees()
    }

    fn oklab_distance(a: Hsla, b: Hsla) -> f32 {
        let (a, b) = (srgb_to_oklab(to_rgb(a)), srgb_to_oklab(to_rgb(b)));
        ((a.l - b.l).powi(2) + (a.a - b.a).powi(2) + (a.b - b.b).powi(2)).sqrt()
    }

    fn relative(c: Hsla) -> f32 {
        geode_core::colour::oklab::relative_luminance(to_rgb(c))
    }

    /// The first expiries a strip shows, for the sweeps below.
    const SHOWN: usize = 24;

    /// Every check a hue palette must pass on one theme, as failures.
    fn hue_failures(name: &str, p: &HuePalette, background: Hsla) -> Vec<String> {
        let mut failures = Vec::new();
        let bg = to_rgb(background);
        for i in 0..SHOWN {
            let (c, pale) = (p.color(i), p.companion(i));
            for (what, x) in [("color", c), ("companion", pale)] {
                let ratio = contrast_ratio(to_rgb(x), bg);
                if ratio < READABLE_RATIO {
                    failures.push(format!("{name}: {what} {i} at {ratio:.2}:1"));
                }
            }
            if i + 1 < SHOWN {
                let d = hue_distance(c, p.color(i + 1));
                if d < 100.0 {
                    failures.push(format!(
                        "{name}: {i} and {} only {d:.0} degrees apart",
                        i + 1
                    ));
                }
            }
            for j in i + 1..SHOWN {
                if hue_distance(c, p.color(j)) < 5.0 {
                    failures.push(format!("{name}: {i} and {j} share a hue"));
                }
            }
            let d = oklab_distance(c, pale);
            if d < 0.08 {
                failures.push(format!("{name}: {i}'s companion only {d:.3} from it"));
            }
        }
        failures
    }

    #[test]
    fn neighbours_are_a_golden_angle_apart_and_never_repeat() {
        let chart: [Hsla; 5] = [
            rgb(0x2563eb).into(),
            rgb(0x16a34a).into(),
            rgb(0xdc2626).into(),
            rgb(0xca8a04).into(),
            rgb(0x9333ea).into(),
        ];
        let light = hsla(0.0, 0.0, 1.0, 1.0);
        let dark = hsla(0.0, 0.0, 0.08, 1.0);
        let on_light = HuePalette::from_theme(chart, light, dark);
        let on_dark = HuePalette::from_theme(chart, dark, light);
        let mut failures = hue_failures("light", &on_light, light);
        failures.extend(hue_failures("dark", &on_dark, dark));
        assert!(failures.is_empty(), "{failures:#?}");
        let d = hue_distance(on_light.color(0), on_light.color(1));
        assert!((d - GOLDEN_ANGLE).abs() < 1.0, "{d}");
        assert!(
            hue_distance(on_light.color(0), chart[0]) < 1.0,
            "index 0 is the first chart color's hue"
        );
        assert!(
            relative(on_dark.companion(0)) > relative(on_dark.color(0)),
            "on a dark ground the companion is lighter"
        );
    }

    /// Grey chart colors still give hues that read apart: chroma is floored.
    #[test]
    fn a_grey_theme_still_gets_distinct_hues() {
        let g = |l| hsla(0.0, 0.0, l, 1.0);
        let p = HuePalette::from_theme([g(0.3), g(0.4), g(0.5), g(0.6), g(0.35)], g(1.0), g(0.0));
        let failures = hue_failures("grey", &p, g(1.0));
        assert!(failures.is_empty(), "{failures:#?}");
    }

    #[gpui::test]
    fn every_bundled_themes_hue_palette_is_readable_and_separated(cx: &mut gpui::TestAppContext) {
        use gpui_component::{ActiveTheme, Theme};
        cx.update(gpui_component::init);
        let (service, _) = geode_shell::theme::load_bundled();
        let mut failures = Vec::new();
        let mut themes = 0;
        for name in service.names() {
            let entry = service.resolve(&name).unwrap().clone();
            cx.update(|cx| {
                Theme::global_mut(cx).apply_config(&entry);
                let t = cx.theme();
                let p = HuePalette::from_theme(
                    [t.chart_1, t.chart_2, t.chart_3, t.chart_4, t.chart_5],
                    t.background,
                    t.foreground,
                );
                themes += 1;
                failures.extend(hue_failures(&name, &p, t.background));
            });
        }
        assert!(themes >= 40, "the sweep saw {themes} themes");
        assert!(failures.is_empty(), "{failures:#?}");
    }
}
