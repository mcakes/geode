//! Prepared grid-row text colours, recomputed on theme changes.
//! CellState selects own, muted stale/inherited, or failure text; package rows have a
//! separate palette. GridModel remains independent of the theme.
//!
//! Row text is adjusted against its base background and the table's hover and selection
//! backgrounds, which replace that base. The bundled-theme test checks these prepared
//! colours on each background. Menu colors are `geode_tile::menu::MenuPaint`'s.
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
    /// The expiry date field's active segment text on its `primary` fill,
    /// and a mid-typing segment's on its `accent` fill. The field paints
    /// on the cursor row, whose ground may be the line's own, hover or
    /// selected: each fill is composited over all three (a translucent
    /// fill reads differently on each) and the text floored on every one.
    pub date_active_text: Hsla,
    pub date_typing_text: Hsla,
}

impl Paints {
    pub fn derive(theme: &Theme) -> Paints {
        let ground: Rgb = over(theme.table, to_rgb(theme.background));
        let package: Rgb = over(theme.secondary, ground);
        let danger = chip_paint(theme, Tone::DangerText).text;
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
            date_active_text: floor_on_all(
                theme.primary_foreground,
                &line.map(|g| over(theme.primary, g)),
            ),
            date_typing_text: floor_on_all(
                theme.accent_foreground,
                &line.map(|g| over(theme.accent, g)),
            ),
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

    /// Check each prepared text colour against its painted backgrounds for every
    /// bundled theme. Include row hover and selection, and date
    /// segment fills composited over the possible row backgrounds.
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
                    // The row palette is also checked on hover and selection
                    // backgrounds below.
                    // The date field's segment fills composited over the
                    // row's own ground; hover and selected below.
                    (
                        "date active",
                        p.date_active_text,
                        over(theme.primary, ground),
                    ),
                    (
                        "date typing",
                        p.date_typing_text,
                        over(theme.accent, ground),
                    ),
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
                            .into_iter()
                            .chain([
                                (
                                    leak(format!("date active on {which}")),
                                    p.date_active_text,
                                    over(theme.primary, bg),
                                ),
                                (
                                    leak(format!("date typing on {which}")),
                                    p.date_typing_text,
                                    over(theme.accent, bg),
                                ),
                            ])
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
            checked >= 18 * 40,
            "every bundled theme was swept ({checked})"
        );
        assert!(
            failures.is_empty(),
            "unreadable pricer paints:\n{}",
            failures.join("\n")
        );
    }

    /// A color equal to its background must still reach READABLE_RATIO.
    /// Moving toward the higher-contrast black or white pole supplies a
    /// distinct endpoint for bisection on both light and dark backgrounds.
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
