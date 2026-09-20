//! The default series colours: the theme's five chart colours, each
//! floored to 3:1 against the background (spec §8.1).

use geode_core::colour::{Rgb, readable_on};
use gpui::Hsla;

/// Per-slot line colour assignment over a theme's five chart colours,
/// each floored to 3:1 against the background (moved toward
/// `foreground`) before being handed out. Spec §8.1 named this
/// `default_for(slot, theme_chart, background)`; the floor's
/// `readable_on` needs the direction to move in, so `from_theme` takes
/// the foreground too — recorded as an as-built amendment.
pub struct Palette {
    colours: [Hsla; 5],
}

/// Mirrors `geode_shell::shell::colours::to_rgb` — the shell is not a
/// dependency of this crate.
pub fn to_rgb(hsla: Hsla) -> Rgb {
    let c = hsla.to_rgb();
    Rgb {
        r: c.r,
        g: c.g,
        b: c.b,
    }
}

/// Mirrors `geode_shell::shell::colours::to_hsla` — the shell is not a
/// dependency of this crate.
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
    fn every_bundled_themes_palette_is_readable(cx: &mut gpui::TestAppContext) {
        use gpui_component::{ActiveTheme, Theme};
        cx.update(gpui_component::init);
        let (service, _) = geode_shell::theme::load_bundled();
        let mut failures = Vec::new();
        let mut checked = 0;
        for name in service.names() {
            let entry = service.resolve(&name).unwrap().clone();
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
