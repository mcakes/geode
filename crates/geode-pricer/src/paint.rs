//! The pricer's text colours (line-pricer spec §8.2), derived once per
//! theme and floored to `READABLE_RATIO` against the ground each paints
//! on (planning decision 14): an own value in `foreground`, a stale
//! result or an inherited shift `muted`, a failed row's cells in
//! `Tone::DangerText`, and a package row on `secondary` with its own
//! floored trio. Resolved in `render_td` from the cell's `CellState`; the
//! `GridModel` stays theme-free. The tile re-derives it when the theme
//! global changes (an observer, never a per-cell check).
//!
//! Each floor moves toward whichever of pure black or pure white
//! contrasts more with the ground it paints on (the market-data
//! `FlooredTones` idiom, `geode-marketdata/src/tile.rs`) — one pole for
//! the table ground, one for the package ground — never toward the
//! colour's own theme anchor (`theme.foreground`). Floored toward its own
//! anchor, an already-equal pair (`own`/`package_own` against a theme
//! whose `foreground` already painted its ground, e.g. a monochrome
//! theme) gives `readable_on` nothing to bisect toward and the floor is a
//! no-op (review fix, 2026-09-24). Every colour clears `READABLE_RATIO`
//! against one of the two poles, so this floor always lands.

use crate::core::columns::CellState;
use geode_core::colour::{Rgb, contrast_ratio, readable_on};
use geode_shell::shell::chip::{Tone, chip_paint};
use geode_shell::shell::colours::{over, to_hsla, to_rgb};
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

/// Whichever of black or white contrasts more against `bg`.
fn pole(bg: Rgb) -> Rgb {
    if contrast_ratio(BLACK, bg) >= contrast_ratio(WHITE, bg) {
        BLACK
    } else {
        WHITE
    }
}

/// Floor `c` to `READABLE_RATIO` against `bg`, moving toward `bg`'s own
/// pole rather than toward `c` (or any other fixed anchor) — see the
/// module doc for why a fixed anchor can leave the bisection nothing to
/// move toward.
fn floor_toward_pole(c: Hsla, bg: Rgb) -> Hsla {
    to_hsla(readable_on(to_rgb(c), bg, pole(bg)))
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Paints {
    pub own: Hsla,
    pub muted: Hsla,
    pub danger: Hsla,
    /// Opaque: `secondary` composited over the table ground.
    pub package_ground: Hsla,
    pub package_own: Hsla,
    pub package_muted: Hsla,
    pub package_danger: Hsla,
}

impl Paints {
    pub fn derive(theme: &Theme) -> Paints {
        let ground: Rgb = over(theme.table, to_rgb(theme.background));
        let package: Rgb = over(theme.secondary, ground);
        let danger = chip_paint(theme, Tone::DangerText).text;
        Paints {
            own: floor_toward_pole(theme.foreground, ground),
            muted: floor_toward_pole(theme.muted_foreground, ground),
            danger: floor_toward_pole(danger, ground),
            package_ground: to_hsla(package),
            package_own: floor_toward_pole(theme.foreground, package),
            package_muted: floor_toward_pole(theme.muted_foreground, package),
            package_danger: floor_toward_pole(danger, package),
        }
    }

    pub fn text(&self, state: CellState, package: bool) -> Hsla {
        match (state, package) {
            (CellState::Stale | CellState::Inherited, false) => self.muted,
            (CellState::Stale | CellState::Inherited, true) => self.package_muted,
            (CellState::Failed, false) => self.danger,
            (CellState::Failed, true) => self.package_danger,
            (CellState::Own | CellState::Blank, false) => self.own,
            (CellState::Own | CellState::Blank, true) => self.package_own,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use geode_core::colour::{READABLE_RATIO, contrast_ratio};
    use gpui_component::{ActiveTheme as _, Theme};

    /// Spec §8.2: every paint the pricer adds, swept over every bundled
    /// theme with NO exception list — each text colour against the ground
    /// it actually paints on (the table over the window background for a
    /// line row, `secondary` over that for a package row).
    #[gpui::test]
    fn every_pricer_paint_is_readable_on_every_bundled_theme(cx: &mut gpui::TestAppContext) {
        cx.update(gpui_component::init);
        let (service, _) = geode_shell::theme::load_bundled();
        let mut failures = Vec::new();
        let mut checked = 0;
        for name in service.names() {
            let entry = service.resolve(&name).unwrap().clone();
            cx.update(|cx| {
                Theme::global_mut(cx).apply_config(&entry);
                let theme = cx.theme();
                let p = Paints::derive(theme);
                let ground = over(theme.table, to_rgb(theme.background));
                let package = to_rgb(p.package_ground);
                for (label, text, bg) in [
                    ("own", p.own, ground),
                    ("muted", p.muted, ground),
                    ("danger", p.danger, ground),
                    ("package own", p.package_own, package),
                    ("package muted", p.package_muted, package),
                    ("package danger", p.package_danger, package),
                ] {
                    checked += 1;
                    let ratio = contrast_ratio(to_rgb(text), bg);
                    if ratio < READABLE_RATIO {
                        failures.push(format!("{name}: {label} at {ratio:.2}:1"));
                    }
                }
            });
        }
        assert!(
            checked >= 6 * 40,
            "every bundled theme was swept ({checked})"
        );
        assert!(
            failures.is_empty(),
            "unreadable pricer paints:\n{}",
            failures.join("\n")
        );
    }

    /// Review fix (2026-09-24): floating `own`/`package_own` toward
    /// `theme.foreground` itself made `readable_on` a no-op whenever the
    /// colour and its ground already matched (`contrast_ratio` at 1:1,
    /// nothing for the bisection to move toward). Pin the fix at the
    /// helper `derive` calls: a colour equal to its own ground must still
    /// reach `READABLE_RATIO` once floored toward a pole.
    #[test]
    fn the_floor_moves_a_colour_equal_to_its_ground_to_the_readable_ratio() {
        let ground = Rgb {
            r: 0.5,
            g: 0.5,
            b: 0.5,
        };
        let floored = floor_toward_pole(to_hsla(ground), ground);
        assert!(
            contrast_ratio(to_rgb(floored), ground) >= READABLE_RATIO,
            "a colour equal to its ground must still clear {READABLE_RATIO}:1 once floored"
        );
    }

    #[gpui::test]
    fn a_state_picks_its_colour_and_a_package_row_its_own(cx: &mut gpui::TestAppContext) {
        cx.update(gpui_component::init);
        cx.update(|cx| {
            let p = Paints::derive(cx.theme());
            assert_eq!(p.text(CellState::Own, false), p.own);
            assert_eq!(p.text(CellState::Stale, false), p.muted);
            assert_eq!(p.text(CellState::Inherited, false), p.muted);
            assert_eq!(p.text(CellState::Failed, false), p.danger);
            assert_eq!(p.text(CellState::Own, true), p.package_own);
            assert_eq!(p.text(CellState::Failed, true), p.package_danger);
        });
    }
}
