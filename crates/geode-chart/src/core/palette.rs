//! The default series colours: the theme's five chart colours, each
//! adjusted toward the foreground for contrast against the background.

use geode_core::colour::{Rgb, readable_on};
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

#[cfg(test)]
mod tests {
    use super::*;
    use geode_core::colour::{READABLE_RATIO, contrast_ratio};
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
}
