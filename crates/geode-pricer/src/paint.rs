//! Prepared grid-row text colours, recomputed on theme changes.
//! CellState selects own, muted stale/inherited/mixed, or failure text. Package rows
//! share the line palette: the tree column carries their structure. Grouping rows are
//! the one row with a ground of its own (`group_ground`) and so a palette of their own,
//! floored on it. A package's template chip takes the neutral chip pair (`chip_fill`,
//! `chip_text`). GridModel remains independent of the theme.
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
use geode_core::colour::{READABLE_RATIO, Rgb, Sign, contrast_ratio, readable_on};
use geode_core::view::Colour;
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
    /// Opaque: `table_hover` over the table ground — what the table
    /// paints on a row under the pointer, replacing the row's own ground.
    /// The chevron's only interactive ground.
    pub row_hover: Hsla,
    /// A package's template chip: the neutral chip's fill (`secondary`,
    /// possibly translucent) and its text, floored on that fill composited
    /// over each of the row's three grounds (own, hover, selected).
    pub chip_fill: Hsla,
    pub chip_text: Hsla,
    /// The expiry date field's active segment text on its `primary` fill,
    /// and a mid-typing segment's on its `accent` fill. The field paints
    /// on the cursor row, whose ground may be the line's own, hover or
    /// selected: each fill is composited over all three (a translucent
    /// fill reads differently on each) and the text floored on every one.
    pub date_active_text: Hsla,
    pub date_typing_text: Hsla,
    /// Opaque: `secondary` composited over the table ground — a grouping
    /// row's own ground, the only row ground the pricer paints (the
    /// blotter's group rows have none; spec §9 assumed one, so the pricer
    /// takes this one). Hover and selection replace it as they replace a
    /// line's.
    pub group_ground: Hsla,
    /// A grouping row's text, each floored on its ground and on the hover
    /// and selected grounds: the state paints (own, muted `mixed` /
    /// stale / inherited, danger), and a `sign` column's bearish and
    /// bullish, which on a line are the theme's chart tokens as they are.
    pub group_own: Hsla,
    pub group_muted: Hsla,
    pub group_danger: Hsla,
    pub group_bearish: Hsla,
    pub group_bullish: Hsla,
    /// `group_ground`, hover and selected: what a group row's text is
    /// floored on.
    group_grounds: [Rgb; 3],
}

impl Paints {
    pub fn derive(theme: &Theme) -> Paints {
        let ground: Rgb = over(theme.table, to_rgb(theme.background));
        let danger = chip_paint(theme, Tone::DangerText).text;
        // A row's text reads on its own ground and on the two the table
        // paints over it (hover, selected), which replace it.
        let [hover, selected] = Self::row_grounds(theme);
        let line = [ground, hover, selected];
        let chip = chip_paint(theme, Tone::Neutral);
        // `Tone::Neutral` fills with `secondary`; fall back to it rather
        // than panic on a theme change.
        let chip_fill = chip.fill.unwrap_or(theme.secondary);
        let chip_grounds = line.map(|g| over(chip_fill, g));
        let group_ground = over(theme.secondary, ground);
        let group = [group_ground, hover, selected];
        Paints {
            group_ground: to_hsla(group_ground),
            group_grounds: group,
            group_own: floor_on_all(theme.foreground, &group),
            group_muted: floor_on_all(theme.muted_foreground, &group),
            group_danger: floor_on_all(danger, &group),
            group_bearish: floor_on_all(theme.chart_bearish, &group),
            group_bullish: floor_on_all(theme.chart_bullish, &group),
            own: floor_on_all(theme.foreground, &line),
            muted: floor_on_all(theme.muted_foreground, &line),
            danger: floor_on_all(danger, &line),
            row_hover: to_hsla(hover),
            chip_fill,
            chip_text: floor_on_all(chip.text, &chip_grounds),
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
    /// uses table_active when list.active_highlight is enabled, otherwise accent.
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

    /// `c` floored on a group row's three grounds, as the group palette
    /// is: for a colour the palette cannot prepare (a view's named column
    /// colour).
    pub fn floor_on_group(&self, c: Hsla) -> Hsla {
        floor_on_all(c, &self.group_grounds)
    }

    /// [`Paints::text`] on a grouping row's ground.
    pub fn group_text(&self, state: CellState) -> Hsla {
        match state {
            CellState::Stale | CellState::Inherited | CellState::Mixed => self.group_muted,
            CellState::Failed => self.group_danger,
            CellState::Own | CellState::Blank => self.group_own,
        }
    }

    pub fn text(&self, state: CellState) -> Hsla {
        match state {
            CellState::Stale | CellState::Inherited | CellState::Mixed => self.muted,
            CellState::Failed => self.danger,
            CellState::Own | CellState::Blank => self.own,
        }
    }
}

/// Which colour a painted cell takes, decided from the column's declared
/// colour, the cell's state and its value's sign. State colours (muted
/// stale, danger failed) win over any column colour: a wrong-looking
/// number must never read as a healthy one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CellColour {
    /// The state paint (`Paints::text`).
    State,
    Bearish,
    Bullish,
    /// The column's named colour, in the variant for this sign (a text
    /// cell's `Zero` is the base).
    Named(Sign),
}

pub(crate) fn cell_colour(colour: &Colour, state: CellState, sign: Option<Sign>) -> CellColour {
    if !matches!(state, CellState::Own) {
        return CellColour::State;
    }
    match (colour, sign) {
        (Colour::Sign, Some(Sign::Negative)) => CellColour::Bearish,
        (Colour::Sign, Some(Sign::Positive)) => CellColour::Bullish,
        (Colour::Named(_), sign) => CellColour::Named(sign.unwrap_or(Sign::Zero)),
        _ => CellColour::State,
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
                for (label, text, bg) in [
                    ("own", p.own, ground),
                    ("muted", p.muted, ground),
                    ("danger", p.danger, ground),
                    // A package's template chip: its text on its fill
                    // composited over the row's ground.
                    ("chip", p.chip_text, over(p.chip_fill, ground)),
                    // The row palette and the chip are also checked on
                    // hover and selection backgrounds below.
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
                // A grouping row's text on its own ground; hover and
                // selected (which replace it) below.
                .chain(
                    [
                        ("group own", p.group_own),
                        ("group muted", p.group_muted),
                        ("group danger", p.group_danger),
                        ("group bearish", p.group_bearish),
                        ("group bullish", p.group_bullish),
                    ]
                    .map(|(label, text)| (label, text, to_rgb(p.group_ground))),
                )
                .chain(
                    Paints::row_grounds(theme)
                        .into_iter()
                        .zip(["hover", "selected"])
                        .flat_map(|(bg, which)| {
                            [("own", p.own), ("muted", p.muted), ("danger", p.danger)]
                                .map(|(label, text)| {
                                    (leak(format!("{label} on {which}")), text, bg)
                                })
                                .into_iter()
                                .chain(
                                    [
                                        ("group own", p.group_own),
                                        ("group muted", p.group_muted),
                                        ("group danger", p.group_danger),
                                        ("group bearish", p.group_bearish),
                                        ("group bullish", p.group_bullish),
                                    ]
                                    .map(|(label, text)| {
                                        (leak(format!("{label} on {which}")), text, bg)
                                    }),
                                )
                                .chain([
                                    (
                                        leak(format!("chip on {which}")),
                                        p.chip_text,
                                        over(p.chip_fill, bg),
                                    ),
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
        // Thirty-three pairs a theme (six line paints and five group
        // paints, each on its row's own ground, on hover and on selected)
        // over at least forty bundled themes.
        assert!(
            checked >= 33 * 40,
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

    #[test]
    fn colour_applies_only_to_an_own_numeric_cell() {
        let (neg, pos) = (Some(Sign::Negative), Some(Sign::Positive));
        assert_eq!(
            cell_colour(&Colour::Sign, CellState::Own, neg),
            CellColour::Bearish
        );
        assert_eq!(
            cell_colour(&Colour::Sign, CellState::Own, pos),
            CellColour::Bullish
        );
        assert_eq!(
            cell_colour(&Colour::Sign, CellState::Own, Some(Sign::Zero)),
            CellColour::State,
            "zero has no sign"
        );
        assert_eq!(
            cell_colour(&Colour::Sign, CellState::Stale, neg),
            CellColour::State,
            "stale stays muted"
        );
        assert_eq!(
            cell_colour(&Colour::Sign, CellState::Failed, neg),
            CellColour::State,
            "failed stays danger"
        );
        assert_eq!(
            cell_colour(&Colour::None, CellState::Own, neg),
            CellColour::State
        );
        assert_eq!(
            cell_colour(&Colour::Named("rose".into()), CellState::Own, neg),
            CellColour::Named(Sign::Negative)
        );
        assert_eq!(
            cell_colour(&Colour::Named("rose".into()), CellState::Own, None),
            CellColour::Named(Sign::Zero),
            "text in a named column takes the base"
        );
    }

    #[gpui::test]
    fn a_state_picks_its_colour(cx: &mut gpui::TestAppContext) {
        cx.update(gpui_component::init);
        cx.update(|cx| {
            let p = Paints::derive(cx.theme());
            assert_eq!(p.text(CellState::Own), p.own);
            assert_eq!(p.text(CellState::Blank), p.own);
            assert_eq!(p.text(CellState::Stale), p.muted);
            assert_eq!(p.text(CellState::Inherited), p.muted);
            assert_eq!(p.text(CellState::Failed), p.danger);
            assert_eq!(p.text(CellState::Mixed), p.muted, "`mixed` is muted");
            assert_eq!(p.group_text(CellState::Own), p.group_own);
            assert_eq!(p.group_text(CellState::Blank), p.group_own);
            assert_eq!(p.group_text(CellState::Mixed), p.group_muted);
            assert_eq!(p.group_text(CellState::Stale), p.group_muted);
            assert_eq!(p.group_text(CellState::Inherited), p.group_muted);
            assert_eq!(p.group_text(CellState::Failed), p.group_danger);
        });
    }
}
