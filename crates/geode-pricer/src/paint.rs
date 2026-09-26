//! Prepared grid-row and action-menu text colours, recomputed on theme changes.
//! CellState selects own, muted stale/inherited, or failure text; package rows have a
//! separate palette. GridModel remains independent of the theme.
//!
//! Row text is adjusted against its base background and the table's hover and selection
//! backgrounds, which replace that base. Menu text is adjusted against the popover or
//! enabled-row highlight. The bundled-theme test checks these prepared colours on each
//! background.
//!
//! Adjust toward whichever of black or white has greater contrast with the background.
//! Using the original text colour as the adjustment endpoint would fail when that
//! colour already matches its background. The multi-background helper is bounded; it
//! does not guarantee success for arbitrary combinations of backgrounds that require
//! opposite endpoints.

use crate::core::columns::CellState;
use geode_core::colour::{READABLE_RATIO, Rgb, contrast_ratio, readable_on};
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

/// Adjust text against the lowest-contrast failing background on each pass, for at most
/// grounds.len() passes. Bundled-theme tests check all resulting background pairs;
/// incompatible backgrounds need not converge within this bound.
fn floor_on_all(c: Hsla, grounds: &[Rgb]) -> Hsla {
    let mut c = c;
    for _ in 0..grounds.len() {
        let Some(worst) = grounds
            .iter()
            .copied()
            .filter(|g| contrast_ratio(to_rgb(c), *g) < READABLE_RATIO)
            .min_by(|a, b| contrast_ratio(to_rgb(c), *a).total_cmp(&contrast_ratio(to_rgb(c), *b)))
        else {
            break;
        };
        c = floor_toward_pole(c, worst);
    }
    c
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Paints {
    pub own: Hsla,
    pub muted: Hsla,
    pub danger: Hsla,
    /// Opaque: `secondary` composited over the table ground.
    pub package_ground: Hsla,
    /// Opaque: `table_hover` over the table ground — what the table
    /// paints on a row under the pointer, replacing the row's own ground
    /// (a package's included). The chevron's only interactive ground.
    pub row_hover: Hsla,
    pub package_own: Hsla,
    pub package_muted: Hsla,
    pub package_danger: Hsla,
    /// Action-menu text for the popover and enabled-row accent background. The muted
    /// variants serve disabled reasons, section headings, and default-key hints.
    /// Disabled actions never receive the accent fill.
    pub menu_text: Hsla,
    pub menu_muted: Hsla,
    pub menu_active_text: Hsla,
    pub menu_active_muted: Hsla,
}

impl Paints {
    pub fn derive(theme: &Theme) -> Paints {
        let ground: Rgb = over(theme.table, to_rgb(theme.background));
        let package: Rgb = over(theme.secondary, ground);
        let danger = chip_paint(theme, Tone::DangerText).text;
        let (popover, active) = Self::menu_grounds(theme);
        // A row's text reads on its own ground and on the two the table
        // paints over it (hover, selected), which replace it.
        let [hover, selected] = Self::row_grounds(theme);
        let line = [ground, hover, selected];
        let pkg = [package, hover, selected];
        Paints {
            own: floor_on_all(theme.foreground, &line),
            muted: floor_on_all(theme.muted_foreground, &line),
            danger: floor_on_all(danger, &line),
            package_ground: to_hsla(package),
            row_hover: to_hsla(hover),
            package_own: floor_on_all(theme.foreground, &pkg),
            package_muted: floor_on_all(theme.muted_foreground, &pkg),
            package_danger: floor_on_all(danger, &pkg),
            menu_text: floor_toward_pole(theme.popover_foreground, popover),
            menu_muted: floor_toward_pole(theme.muted_foreground, popover),
            menu_active_text: floor_toward_pole(theme.accent_foreground, active),
            menu_active_muted: floor_toward_pole(theme.muted_foreground, active),
        }
    }

    /// Table hover and selection backgrounds composited over the table base. Selection
    /// uses table_active when list.active_highlight is enabled, otherwise accent. Both
    /// replace a package row's own background.
    pub(crate) fn row_grounds(theme: &Theme) -> [Rgb; 2] {
        let ground: Rgb = over(theme.table, to_rgb(theme.background));
        let selected = if theme.list.active_highlight {
            *theme.tokens.table_active
        } else {
            *theme.tokens.accent
        };
        [
            over(*theme.tokens.table_hover, ground),
            over(selected, ground),
        ]
    }

    /// The popover over the window background, and the highlighted row's
    /// `accent` over that: the two grounds the menu's text paints on.
    fn menu_grounds(theme: &Theme) -> (Rgb, Rgb) {
        let popover = over(theme.popover, to_rgb(theme.background));
        (popover, over(theme.accent, popover))
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

    /// A test-only label with the sweep's `&'static str` type.
    fn leak(s: String) -> &'static str {
        Box::leak(s.into_boxed_str())
    }

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
                let (popover, active) = Paints::menu_grounds(theme);
                for (label, text, bg) in [
                    ("own", p.own, ground),
                    ("muted", p.muted, ground),
                    ("danger", p.danger, ground),
                    ("package own", p.package_own, package),
                    ("package muted", p.package_muted, package),
                    ("package danger", p.package_danger, package),
                    // Menu text uses its corresponding popover or accent background.
                    // The row palette is also checked on hover and selection
                    // backgrounds below.
                    ("menu text", p.menu_text, popover),
                    ("menu muted", p.menu_muted, popover),
                    ("menu active text", p.menu_active_text, active),
                    ("menu active muted", p.menu_active_muted, active),
                ]
                .into_iter()
                .chain(
                    Paints::row_grounds(theme)
                        .into_iter()
                        .zip(["hover", "selected"])
                        .flat_map(|(bg, which)| {
                            [
                                ("own", p.own),
                                ("muted", p.muted),
                                ("danger", p.danger),
                                ("package own", p.package_own),
                                ("package muted", p.package_muted),
                                ("package danger", p.package_danger),
                            ]
                            .map(|(label, text)| (leak(format!("{label} on {which}")), text, bg))
                        }),
                ) {
                    checked += 1;
                    let ratio = contrast_ratio(to_rgb(text), bg);
                    if ratio < READABLE_RATIO {
                        failures.push(format!("{name}: {label} at {ratio:.2}:1"));
                    }
                }
            });
        }
        assert!(
            checked >= 22 * 40,
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
    /// reach `READABLE_RATIO` once floored toward a pole. The light and
    /// dark grounds pin WHICH pole: from either, only the pole with more
    /// contrast can reach 3:1 (white on light grey peaks near 1.6:1).
    #[test]
    fn the_floor_moves_a_colour_equal_to_its_ground_to_the_readable_ratio() {
        for level in [0.2, 0.5, 0.8] {
            let ground = Rgb {
                r: level,
                g: level,
                b: level,
            };
            let floored = floor_toward_pole(to_hsla(ground), ground);
            assert!(
                contrast_ratio(to_rgb(floored), ground) >= READABLE_RATIO,
                "a colour equal to a {level} grey ground must still clear \
                 {READABLE_RATIO}:1 once floored"
            );
        }
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
