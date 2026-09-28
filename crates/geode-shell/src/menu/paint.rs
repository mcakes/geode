//! A menu's colors, derived from the theme: text floored to the readable
//! ratio on the popover and on the lit row's accent, and the pointer states
//! of an enabled row that is not lit. Floored toward whichever of black or
//! white contrasts more with the ground: floored toward the color itself, a
//! color equal to its ground has nothing to move toward and stays unreadable.

use crate::shell::colours::{over, to_hsla, to_rgb};
use crate::shell::control::{self, ControlPaint, Rest};
use geode_core::colour::{Rgb, contrast_ratio, readable_on};
use gpui::Hsla;
use gpui_component::Theme;

const BLACK: Rgb = Rgb {
    r: 0.0,
    g: 0.0,
    b: 0.0,
};
const WHITE: Rgb = Rgb {
    r: 1.0,
    g: 1.0,
    b: 1.0,
};

fn pole(ground: Rgb) -> Rgb {
    if contrast_ratio(BLACK, ground) >= contrast_ratio(WHITE, ground) {
        BLACK
    } else {
        WHITE
    }
}

fn floor(color: Hsla, ground: Rgb) -> Hsla {
    to_hsla(readable_on(to_rgb(color), ground, pole(ground)))
}

/// A menu's prepared colors.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MenuPaint {
    /// An enabled row's title on the popover.
    pub text: Hsla,
    /// Lanes, section headings, and a disabled row's title, on the popover.
    pub muted: Hsla,
    /// The lit row's fill.
    pub active_fill: Hsla,
    /// The lit row's title and lane on its fill.
    pub active_text: Hsla,
    /// Hover and pressed for an enabled row that is not lit.
    pub rest_pointer: ControlPaint,
}

impl MenuPaint {
    pub fn derive(theme: &Theme) -> MenuPaint {
        let (popover, active) = Self::grounds(theme);
        let text = floor(theme.popover_foreground, popover);
        MenuPaint {
            text,
            muted: floor(theme.muted_foreground, popover),
            active_fill: theme.accent,
            active_text: floor(theme.accent_foreground, active),
            rest_pointer: control::paint(theme, Rest::Bare, theme.popover, text),
        }
    }

    /// The popover over the window background, and the lit row's accent over
    /// that: the two grounds a menu's text lands on.
    pub(crate) fn grounds(theme: &Theme) -> (Rgb, Rgb) {
        let popover = over(theme.popover, to_rgb(theme.background));
        (popover, over(theme.accent, popover))
    }
}

/// One action row's paint.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RowPaint {
    pub fill: Option<Hsla>,
    pub text: Hsla,
    pub lane: Hsla,
    /// `None` on a lit row (the pointer's row is the lit row, since a hover
    /// moves the highlight; a second fill there would recolor the highlight
    /// under the mouse) and on a disabled row (it takes no fill at all).
    pub pointer: Option<ControlPaint>,
}

/// Only a lit, enabled row takes the fill. A disabled row can hold the
/// highlight (the pointer lit it) but stays muted on the popover: a fill
/// there reads as an offer on a row that will only refuse. The lit row's
/// lane follows its title so keys never sit muted on the accent.
pub fn row_paint(p: &MenuPaint, lit: bool, enabled: bool) -> RowPaint {
    match (lit, enabled) {
        (true, true) => RowPaint {
            fill: Some(p.active_fill),
            text: p.active_text,
            lane: p.active_text,
            pointer: None,
        },
        (false, true) => RowPaint {
            fill: None,
            text: p.text,
            lane: p.muted,
            pointer: Some(p.rest_pointer),
        },
        (_, false) => RowPaint {
            fill: None,
            text: p.muted,
            lane: p.muted,
            pointer: None,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use geode_core::colour::READABLE_RATIO;
    use gpui_component::ActiveTheme as _;

    #[gpui::test]
    fn only_an_enabled_lit_row_takes_the_fill(cx: &mut gpui::TestAppContext) {
        cx.update(gpui_component::init);
        cx.update(|cx| {
            let p = MenuPaint::derive(cx.theme());
            assert_eq!(row_paint(&p, true, true).fill, Some(p.active_fill));
            assert_eq!(row_paint(&p, true, true).lane, p.active_text);
            assert_eq!(row_paint(&p, false, true).fill, None);
            assert_eq!(row_paint(&p, false, true).pointer, Some(p.rest_pointer));
            assert_eq!(row_paint(&p, true, false).fill, None);
            assert_eq!(row_paint(&p, true, false).text, p.muted);
            assert_eq!(row_paint(&p, true, false).pointer, None);
            assert_eq!(row_paint(&p, false, false), row_paint(&p, true, false));
        });
    }

    #[test]
    fn a_color_equal_to_its_ground_floors_to_the_readable_ratio() {
        for level in [0.2, 0.5, 0.8] {
            let ground = Rgb {
                r: level,
                g: level,
                b: level,
            };
            let floored = floor(to_hsla(ground), ground);
            assert!(
                contrast_ratio(to_rgb(floored), ground) >= READABLE_RATIO,
                "a {level} grey on itself must reach {READABLE_RATIO}:1"
            );
        }
    }

    #[gpui::test]
    fn every_menu_paint_is_readable_on_every_bundled_theme(cx: &mut gpui::TestAppContext) {
        cx.update(gpui_component::init);
        let (service, _) = crate::theme::load_bundled();
        let mut failures = Vec::new();
        let mut checked = 0;
        for name in service.names() {
            let entry = service.resolve(&name).unwrap().clone();
            cx.update(|cx| {
                Theme::global_mut(cx).apply_config(&entry);
                let theme = cx.theme();
                let p = MenuPaint::derive(theme);
                let (popover, active) = MenuPaint::grounds(theme);
                for (label, text, ground) in [
                    ("text", p.text, popover),
                    ("muted", p.muted, popover),
                    ("active text", p.active_text, active),
                ] {
                    checked += 1;
                    let ratio = contrast_ratio(to_rgb(text), ground);
                    if ratio < READABLE_RATIO {
                        failures.push(format!("{name}: {label} at {ratio:.2}:1"));
                    }
                }
            });
        }
        assert!(
            checked >= 3 * 40,
            "every bundled theme was swept ({checked})"
        );
        assert!(
            failures.is_empty(),
            "unreadable menu paints:\n{}",
            failures.join("\n")
        );
    }
}
