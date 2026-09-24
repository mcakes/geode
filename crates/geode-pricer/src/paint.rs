//! The pricer's text colours (line-pricer spec §8.2), derived once per
//! theme and floored to `READABLE_RATIO` against the ground each paints
//! on (planning decision 14): an own value in `foreground`, a stale
//! result or an inherited shift `muted`, a failed row's cells in
//! `Tone::DangerText`, and a package row on `secondary` with its own
//! floored trio. Resolved in `render_td` from the cell's `CellState`; the
//! `GridModel` stays theme-free. The tile re-derives it when the theme
//! global changes (an observer, never a per-cell check).

use crate::core::columns::CellState;
use geode_core::colour::{Rgb, readable_on};
use geode_shell::shell::chip::{Tone, chip_paint};
use geode_shell::shell::colours::{over, to_hsla, to_rgb};
use gpui::Hsla;
use gpui_component::Theme;

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
        let toward = to_rgb(theme.foreground);
        let floor = |c: Hsla, bg: Rgb| to_hsla(readable_on(to_rgb(c), bg, toward));
        let danger = chip_paint(theme, Tone::DangerText).text;
        Paints {
            own: floor(theme.foreground, ground),
            muted: floor(theme.muted_foreground, ground),
            danger: floor(danger, ground),
            package_ground: to_hsla(package),
            package_own: floor(theme.foreground, package),
            package_muted: floor(theme.muted_foreground, package),
            package_danger: floor(danger, package),
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
